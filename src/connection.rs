//! Bitcoin p2p protocol connection.
//!
//! This module provides connection handling for the bitcoin peer-to-peer network.
//! It implements the bitcoin p2p protocol, including version handshake, message
//! serialization/deserialization, and protocol-level behaviors like automatic
//! ping-pong responses.
//!
//! # Examples
//!
//! Creating a TCP connection to a bitcoin peer:
//!
//! ```rust,no_run
//! use bitcoin::Network;
//! use bitcoin_peers::{Connection, ConnectionConfiguration, Peer, PeerProtocolVersion};
//! use bitcoin::p2p::address::AddrV2;
//! use bitcoin::p2p::message::NetworkMessage;
//! use std::net::Ipv4Addr;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let peer = Peer::new(
//!     AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)),
//!     8333,
//! );
//!
//! // Configure the connection as non-listening (appropriate for light client software).
//! // Set the required protocol version and use the default user agent.
//! let config = ConnectionConfiguration::non_listening(
//!     PeerProtocolVersion::Known(70016),
//!     None,
//! );
//!
//! // Establish the connection with automatic handshake.
//! let mut connection = Connection::tcp(peer, Network::Bitcoin, config).await?;
//!
//! // Send a getaddr message to request peer addresses
//! connection.send(NetworkMessage::GetAddr).await?;
//!
//! // Receive a response
//! let response = connection.receive().await?;
//! println!("Received: {:?}", response);
//! # Ok(())
//! # }
//! ```
//!
//! The [`Connection`] type is the recommended high-level API for most applications.
//!
//! See more [lower level documentation](https://developer.bitcoin.org/reference/p2p_networking.html) on the bitcoin p2p protocol.

use crate::peer::{
    Peer, PeerProtocolVersion, PeerServices, ADDRV2_MIN_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION,
    SENDHEADERS_MIN_PROTOCOL_VERSION, WTXID_RELAY_MIN_PROTOCOL_VERSION,
};
use crate::transport::{Transport, TransportError};
use bip324::{AsyncProtocol, Role};
use bitcoin::p2p::address::{AddrV2, Address};
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::p2p::ServiceFlags;
use bitcoin::Network;
use log::{debug, error};
use std::error::Error;
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::process;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

#[derive(Debug)]
pub enum ConnectionError {
    Io(io::Error),
    TransportFailed(TransportError),
    ProtocolFailed,
    UnsupportedAddressType,
    ConnectionLoop,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionError::Io(err) => write!(f, "Connection error: {err}"),
            ConnectionError::TransportFailed(err) => {
                write!(f, "Transport layer failed in peer connection: {err}")
            }
            ConnectionError::ProtocolFailed => {
                write!(f, "Protocol handling failed in peer communication")
            }
            ConnectionError::UnsupportedAddressType => write!(f, "Unsupported address type"),
            ConnectionError::ConnectionLoop => {
                write!(f, "Detected connection to self (matching nonce)")
            }
        }
    }
}

impl Error for ConnectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ConnectionError::Io(err) => Some(err),
            ConnectionError::TransportFailed(err) => Some(err),
            ConnectionError::ProtocolFailed => None,
            ConnectionError::UnsupportedAddressType => None,
            ConnectionError::ConnectionLoop => None,
        }
    }
}

impl From<io::Error> for ConnectionError {
    fn from(err: io::Error) -> Self {
        ConnectionError::Io(err)
    }
}

impl From<TransportError> for ConnectionError {
    fn from(err: TransportError) -> Self {
        ConnectionError::TransportFailed(err)
    }
}

/// Trait for types that can send Bitcoin network messages.
pub trait MessageSender {
    /// Send a message to the peer.
    ///
    /// # Arguments
    ///
    /// * `message` - The Bitcoin network message to send.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or the specific error that occurred.
    async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError>;
}

/// Trait for types that can receive Bitcoin network messages.
pub trait MessageReceiver {
    /// Receive a message from the peer.
    ///
    /// # Returns
    ///
    /// * `Ok(`[`NetworkMessage`]`)` - The received message
    /// * `Err(`[`ConnectionError`]`)` - If an error occurred during message reception
    async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError>;
}

/// User agent string sent in version messages.
///
/// This identifies crawler software to other peers on the network.
/// Format follows Bitcoin Core's convention: "/$NAME:$VERSION/".
const BITCOIN_PEERS_USER_AGENT: &str = concat!("/bitcoin-peers:", env!("CARGO_PKG_VERSION"), "/");

/// Non-listening address used in version messages.
///
/// This address signals to peers that we are not accepting incoming connections
/// and should not be advertised to other nodes.
const NON_LISTENING_ADDRESS: AddrV2 = AddrV2::Ipv4(Ipv4Addr::new(0, 0, 0, 0));
const NON_LISTENING_PORT: u16 = 0;

/// Gets the current Unix timestamp (seconds since January 1, 1970 00:00:00 UTC).
///
/// # Returns
///
/// An i64 representing the current Unix timestamp.
///
/// # Panics
///
/// If the system clock is set to a time before the Unix epoch
/// (January 1, 1970), which is extremely unlikely on modern systems.
fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("System time is before the Unix epoch")
        .as_secs() as i64
}

/// Generates a 64-bit nonce for use in Bitcoin P2P version messages.
///
/// This function creates a reasonably unique nonce without requiring a `rand` crate.
/// While *not* cryptographically secure, this nonce is suitable for the bitcoin p2p
/// protocol's connection loop detection mechanism.
///
/// # Returns
///
/// A 64-bit unsigned integer to use as a nonce.
fn generate_nonce() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_nanos() as u64;

    // Mix in the process ID for additional entropy.
    let pid = process::id() as u64;

    // Combine the values with bitwise operations.
    now ^ (pid.rotate_left(32))
}

/// Configration used to build a connection.
#[derive(Debug, Clone)]
pub struct ConnectionConfiguration {
    /// Local minimum supported protocol version.
    pub protocol_version: PeerProtocolVersion,
    /// Custom user agent advertised for connection. Default is `/bitcoin-peers:$VERSION/`.
    pub user_agent: Option<String>,
    /// Service flags advertised by this node.
    pub services: ServiceFlags,
    /// Address advertised as the sender in version messages.
    /// For non-IP addresses (Tor, I2P, etc.), the legacy version message
    /// will use a placeholder, but the real address can be communicated
    /// to nodes supporting AddrV2.
    pub sender_address: AddrV2,
    /// Port for the sender address.
    pub sender_port: u16,
    /// Block height advertised in version messages.
    ///
    /// Hopefully 0 doesn't initiate some IBD functionallity.
    pub start_height: i32,
    /// Whether to relay transactions to this peer.
    pub relay: bool,
}

impl ConnectionConfiguration {
    /// Creates a new configuration for a non-listening node.
    ///
    /// This configuration advertises no services, uses a non-listening address,
    /// and doesn't relay transactions. It's suitable for crawlers and other
    /// light client software that just wants to query the network without accepting
    /// incoming connections.
    ///
    /// # Arguments
    ///
    /// * `protocol_version` - The protocol version to advertise. Defaults to MIN_PROTOCOL_VERSION if Unknown.
    /// * `user_agent` - Optional custom user agent string. Defaults to bitcoin-peers default if None.
    ///
    /// # Returns
    ///
    /// A new ConnectionConfiguration configured for a non-listening node.
    pub fn non_listening(
        protocol_version: PeerProtocolVersion,
        user_agent: Option<String>,
    ) -> Self {
        Self {
            protocol_version,
            user_agent,
            services: ServiceFlags::NONE,
            sender_address: NON_LISTENING_ADDRESS,
            sender_port: NON_LISTENING_PORT,
            start_height: 0,
            relay: false,
        }
    }
}

/// State of AddrV2 support negotiation for the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddrV2State {
    /// AddrV2 support has not been negotiated yet.
    NotNegotiated,
    /// We sent a SendAddrV2 message but haven't received one.
    SentOnly,
    /// We received a SendAddrV2 message but haven't sent one.
    ReceivedOnly,
    /// Both sides have exchanged SendAddrV2 messages and AddrV2 is enabled.
    Enabled,
}

impl AddrV2State {
    /// Update the state when we send a SendAddrV2 message.
    fn on_send(&self) -> Self {
        match self {
            AddrV2State::NotNegotiated => AddrV2State::SentOnly,
            AddrV2State::ReceivedOnly => AddrV2State::Enabled,
            _ => *self, // Already sent, state doesn't change
        }
    }

    /// Update the state when we receive a SendAddrV2 message.
    fn on_receive(&self) -> Self {
        match self {
            AddrV2State::NotNegotiated => AddrV2State::ReceivedOnly,
            AddrV2State::SentOnly => AddrV2State::Enabled,
            _ => *self, // Already received, state doesn't change
        }
    }
}

/// State of SendHeaders support negotiation for the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendHeadersState {
    /// SendHeaders support has not been negotiated yet.
    NotNegotiated,
    /// We sent a SendHeaders message but haven't received one.
    SentOnly,
    /// We received a SendHeaders message but haven't sent one.
    ReceivedOnly,
    /// Both sides have exchanged SendHeaders messages and header announcements are enabled.
    Enabled,
}

impl SendHeadersState {
    /// Update the state when we send a SendHeaders message.
    fn on_send(&self) -> Self {
        match self {
            SendHeadersState::NotNegotiated => SendHeadersState::SentOnly,
            SendHeadersState::ReceivedOnly => SendHeadersState::Enabled,
            _ => *self, // Already sent, state doesn't change
        }
    }

    /// Update the state when we receive a SendHeaders message.
    fn on_receive(&self) -> Self {
        match self {
            SendHeadersState::NotNegotiated => SendHeadersState::ReceivedOnly,
            SendHeadersState::SentOnly => SendHeadersState::Enabled,
            _ => *self, // Already received, state doesn't change
        }
    }
}

/// State of WtxidRelay support negotiation for the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WtxidRelayState {
    /// WtxidRelay support has not been negotiated yet.
    NotNegotiated,
    /// We sent a WtxidRelay message but haven't received one.
    SentOnly,
    /// We received a WtxidRelay message but haven't sent one.
    ReceivedOnly,
    /// Both sides have exchanged WtxidRelay messages and witness tx IDs are enabled.
    Enabled,
}

impl WtxidRelayState {
    /// Update the state when we send a WtxidRelay message.
    fn on_send(&self) -> Self {
        match self {
            WtxidRelayState::NotNegotiated => WtxidRelayState::SentOnly,
            WtxidRelayState::ReceivedOnly => WtxidRelayState::Enabled,
            _ => *self, // Already sent, state doesn't change
        }
    }

    /// Update the state when we receive a WtxidRelay message.
    fn on_receive(&self) -> Self {
        match self {
            WtxidRelayState::NotNegotiated => WtxidRelayState::ReceivedOnly,
            WtxidRelayState::SentOnly => WtxidRelayState::Enabled,
            _ => *self, // Already received, state doesn't change
        }
    }
}

/// Runtime state of a connection.
struct ConnectionState {
    /// The protocol version negotiated between peers (minimum of both versions).
    effective_protocol_version: PeerProtocolVersion,
    /// Current state of AddrV2 support negotiation.
    addr_v2: AddrV2State,
    /// Current state of SendHeaders support negotiation.
    send_headers: SendHeadersState,
    /// Current state of WtxidRelay support negotiation.
    wtxid_relay: WtxidRelayState,
}

impl ConnectionState {
    pub fn new() -> Self {
        ConnectionState {
            effective_protocol_version: PeerProtocolVersion::Unknown,
            addr_v2: AddrV2State::NotNegotiated,
            send_headers: SendHeadersState::NotNegotiated,
            wtxid_relay: WtxidRelayState::NotNegotiated,
        }
    }
}

/// Implements the sender half of a peer connection.
///
/// This struct handles sending Bitcoin network messages to a connected peer.
/// It maintains only the necessary state for sending operations.
pub struct PeerConnectionSender<W>
where
    W: AsyncWrite + Unpin + Send,
{
    /// The peer this connection is established with.
    peer: Peer,
    /// Transport handles serialization and encryption of messages.
    transport_sender: crate::transport::TransportSender,
    /// The writer half of the connection.
    writer: W,
}

impl<W> PeerConnectionSender<W>
where
    W: AsyncWrite + Unpin + Send,
{
    /// Creates a new sender with the given transport sender and writer.
    fn new(peer: Peer, transport_sender: crate::transport::TransportSender, writer: W) -> Self {
        Self {
            peer,
            transport_sender,
            writer,
        }
    }

    /// Get a reference to the peer this connection is established with.
    pub fn peer(&self) -> &Peer {
        &self.peer
    }

    /// Send a message to the peer.
    pub async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        self.transport_sender
            .send(message, &mut self.writer)
            .await
            .map_err(ConnectionError::TransportFailed)
    }
}

/// Implements the receiver half of a peer connection.
///
/// This struct handles receiving bitcoin network messages from a connected peer
/// and performs automatic protocol-level responses like responding to pings.
pub struct PeerConnectionReceiver<R>
where
    R: AsyncRead + Unpin + Send,
{
    /// The peer this connection is established with.
    peer: Peer,
    /// Transport handles deserialization and decryption of messages.
    transport_receiver: crate::transport::TransportReceiver,
    /// The reader half of the connection.
    reader: R,
    /// State related to protocol negotiation.
    state: Arc<Mutex<ConnectionState>>,
}

impl<R> PeerConnectionReceiver<R>
where
    R: AsyncRead + Unpin + Send,
{
    /// Creates a new receiver with the given transport receiver and reader.
    fn new(
        peer: Peer,
        transport_receiver: crate::transport::TransportReceiver,
        reader: R,
        state: Arc<Mutex<ConnectionState>>,
    ) -> Self {
        Self {
            peer,
            transport_receiver,
            reader,
            state,
        }
    }

    /// Get a reference to the peer this connection is established with.
    pub fn peer(&self) -> &Peer {
        &self.peer
    }

    /// Receive a message from the peer.
    ///
    /// This method handles certain protocol-level messages automatically,
    /// such as updating protocol negotiation state.
    pub async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        let message = self
            .transport_receiver
            .receive(&mut self.reader)
            .await
            .map_err(ConnectionError::TransportFailed)?;

        // Handle protocol-level messages that affect connection state.
        match &message {
            NetworkMessage::SendAddrV2 => {
                let mut state = self.state.lock().await;
                state.addr_v2 = state.addr_v2.on_receive();
                debug!(
                    "Received SendAddrV2 message, addrv2_state: {:?}",
                    state.addr_v2
                );
            }
            NetworkMessage::SendHeaders => {
                let mut state = self.state.lock().await;
                state.send_headers = state.send_headers.on_receive();
                debug!(
                    "Received SendHeaders message, send_headers_state: {:?}",
                    state.send_headers
                );
            }
            NetworkMessage::WtxidRelay => {
                let mut state = self.state.lock().await;
                state.wtxid_relay = state.wtxid_relay.on_receive();
                debug!(
                    "Received WtxidRelay message, wtxid_relay_state: {:?}",
                    state.wtxid_relay
                );
            }
            _ => {}
        }

        Ok(message)
    }
}

/// Represents a connection to a bitcoin peer.
///
/// This struct manages a connection to a bitcoin peer using the bitcoin p2p protocol.
/// It handles the underlying transport, serialization, protocol management,
/// and connection state (e.g. upgrades).
///
/// # Trait Bounds
///
/// * [`AsyncRead`]/[`AsyncWrite`] - Required for async I/O operations.
/// * [`Unpin`] - Required because uses `&mut self` with `.await`.
/// * [`Send`] - Allows the connection to be sent between threads/tasks.
///
/// Note that [`Sync`] is not required because this struct uses `&mut self` methods
/// which enforce exclusive access. `Connection` is designed to be used by one thread
/// at a time, but if you need to share a `Connection` between threads, wrap it in
/// an [`Arc`]<[`Mutex`]<`Connection<R, W>`>>.
///
/// [`AsyncRead`]: tokio::io::AsyncRead
/// [`AsyncWrite`]: tokio::io::AsyncWrite
/// [`Unpin`]: core::marker::Unpin
/// [`Send`]: core::marker::Send
/// [`Sync`]: core::marker::Sync
/// [`Arc`]: std::sync::Arc
/// [`Mutex`]: tokio::sync::Mutex
pub struct PeerConnection<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Configuration to build the connection.
    configuration: ConnectionConfiguration,
    /// The peer this connection is established with.
    peer: Peer,
    /// Runtime state of the connection, shared with receiver.
    state: Arc<Mutex<ConnectionState>>,
    /// Receiver component for incoming messages.
    receiver: PeerConnectionReceiver<R>,
    /// Sender component for outgoing messages.
    sender: PeerConnectionSender<W>,
}

/// A TCP-based connection to a bitcoin peer.
///
/// This is a convenience type alias for [`PeerConnection`] with Tokio's TCP stream halves.
pub type TcpPeerConnection = PeerConnection<OwnedReadHalf, OwnedWriteHalf>;

/// A TCP-based connection receiver.
///
/// This is a convenience type alias for [`PeerConnectionReceiver`] with Tokio's TCP read half.
pub type TcpPeerConnectionReceiver = PeerConnectionReceiver<OwnedReadHalf>;

/// A TCP-based connection sender.
///
/// This is a convenience type alias for [`PeerConnectionSender`] with Tokio's TCP write half.
pub type TcpPeerConnectionSender = PeerConnectionSender<OwnedWriteHalf>;

/// Receiver half of a split connection.
pub enum ConnectionReceiver {
    /// TCP connection receiver.
    Tcp(TcpPeerConnectionReceiver),
}

impl ConnectionReceiver {
    /// Get a reference to the peer this connection is established with.
    pub fn peer(&self) -> &Peer {
        match self {
            ConnectionReceiver::Tcp(tcp) => tcp.peer(),
        }
    }

    /// Receive a message from the peer.
    ///
    /// This method handles certain protocol-level messages automatically,
    /// such as updating protocol negotiation state.
    ///
    /// # Returns
    ///
    /// * `Ok(NetworkMessage)` - Successfully received and parsed message
    /// * `Err(ConnectionError)` - Error occurred during reception
    pub async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        match self {
            ConnectionReceiver::Tcp(tcp) => tcp.receive().await,
        }
    }
}

/// Sender half of a split connection.
pub enum ConnectionSender {
    /// TCP connection sender.
    Tcp(TcpPeerConnectionSender),
}

impl ConnectionSender {
    /// Get a reference to the peer this connection is established with.
    pub fn peer(&self) -> &Peer {
        match self {
            ConnectionSender::Tcp(tcp) => tcp.peer(),
        }
    }

    /// Send a message to the peer.
    ///
    /// # Arguments
    ///
    /// * `message` - The Bitcoin network message to send
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Message was successfully sent
    /// * `Err(ConnectionError)` - Error occurred during sending
    pub async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        match self {
            ConnectionSender::Tcp(tcp) => tcp.send(message).await,
        }
    }
}

/// Provides a unified interface to different types of bitcoin peer connections.
///
/// This enum is the primary API for interacting with bitcoin peers. It abstracts over
/// different connection transport types (TCP, WebSocket, Tor, etc.) and provides a
/// consistent interface for sending and receiving messages.
pub enum Connection {
    Tcp(TcpPeerConnection),
    // WebSocket(WebSocketConnection),
    // Tor(TorConnection),
    // etc.
}

impl Connection {
    /// Get a reference to the peer this connection is established with.
    pub fn peer(&self) -> &Peer {
        match self {
            Connection::Tcp(conn) => conn.peer(),
        }
    }

    /// Send a message to the peer.
    ///
    /// # Arguments
    ///
    /// * `message` - The Bitcoin network message to send.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or the specific error that occurred.
    pub async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        match self {
            Connection::Tcp(conn) => conn.send(message).await,
        }
    }

    /// Receive a message from the peer.
    ///
    /// This method handles certain protocol-level messages automatically,
    /// such as responding to pings and updating protocol negotiation state.
    ///
    /// # Returns
    ///
    /// * `Ok(`[`NetworkMessage`]`)` - The received message
    /// * `Err(`[`ConnectionError`]`)` - If an error occurred during message reception
    pub async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        match self {
            Connection::Tcp(conn) => conn.receive().await,
        }
    }

    /// Split this connection into separate receiver and sender halves.
    ///
    /// This allows for independent reading and writing operations, which is useful
    /// for concurrent processing in async contexts.
    ///
    /// # Important Note
    ///
    /// When using split connections, automatic ping/pong handling is disabled.
    /// The caller must manually respond to ping messages if needed.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use bitcoin::Network;
    /// # use bitcoin_peers::{Connection, ConnectionConfiguration, Peer, PeerProtocolVersion};
    /// # use bitcoin::p2p::address::AddrV2;
    /// # use bitcoin::p2p::message::NetworkMessage;
    /// # use std::net::Ipv4Addr;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let peer = Peer::new(
    ///     AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)),
    ///     8333,
    /// );
    ///
    /// let config = ConnectionConfiguration::non_listening(
    ///     PeerProtocolVersion::Known(70016),
    ///     None,
    /// );
    ///
    /// let connection = Connection::tcp(peer, Network::Bitcoin, config).await?;
    /// let (mut receiver, mut sender) = connection.split();
    ///
    /// // Now receiver and sender can be used in separate tasks
    /// tokio::spawn(async move {
    ///     while let Ok(msg) = receiver.receive().await {
    ///         // Handle received messages
    ///         match msg {
    ///             NetworkMessage::Ping(nonce) => {
    ///                 // Must manually handle ping/pong in split mode
    ///             }
    ///             _ => {}
    ///         }
    ///     }
    /// });
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Returns
    ///
    /// A tuple containing the receiver and sender halves of the connection.
    pub fn split(self) -> (ConnectionReceiver, ConnectionSender) {
        match self {
            Connection::Tcp(tcp) => {
                let (receiver, sender) = tcp.into_split();
                (
                    ConnectionReceiver::Tcp(receiver),
                    ConnectionSender::Tcp(sender),
                )
            }
        }
    }

    /// Establish a TCP connection to a bitcoin peer and perform the handshake.
    ///
    /// # Arguments
    ///
    /// * `peer` - The bitcoin peer to connect to.
    /// * `network` - The bitcoin [`Network`] to use.
    /// * `configuration` - Configuration for the connection.
    ///
    /// # Returns
    ///
    /// * `Ok(`[`Self`]`)` - A successfully established and handshaked connection
    /// * `Err(`[`ConnectionError`]`)` - If the connection attempt or handshake failed
    pub async fn tcp(
        peer: Peer,
        network: Network,
        configuration: ConnectionConfiguration,
    ) -> Result<Self, ConnectionError> {
        Ok(Connection::Tcp(
            TcpPeerConnection::tcp(peer, network, configuration).await?,
        ))
    }
}

impl MessageSender for Connection {
    async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        match self {
            Connection::Tcp(conn) => conn.send(message).await,
        }
    }
}

impl MessageReceiver for Connection {
    async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        match self {
            Connection::Tcp(conn) => conn.receive().await,
        }
    }
}

impl<R, W> PeerConnection<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Get a reference to the peer this connection is established with.
    pub fn peer(&self) -> &Peer {
        &self.peer
    }

    /// Send a message to the peer.
    pub async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        self.sender.send(message).await
    }

    /// Receive a message from the peer.
    pub async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        self.receiver.receive().await
    }

    /// Split this connection into separate receiver and sender halves.
    ///
    /// This allows for independent reading and writing operations, which is useful
    /// for concurrent processing in async contexts.
    ///
    /// # Returns
    ///
    /// A tuple containing the receiver and sender halves of the connection.
    pub fn into_split(self) -> (PeerConnectionReceiver<R>, PeerConnectionSender<W>) {
        (self.receiver, self.sender)
    }

    /// Creates a bitcoin version message for the connected peer.
    ///
    /// # Arguments
    ///
    /// * `nonce` - The nonce value to use for detecting connection loops.
    ///
    /// # Returns
    ///
    /// A [`NetworkMessage::Version`] containing the version information.
    fn create_version_message(&self, nonce: u64) -> NetworkMessage {
        // Use the known services of the peer if available, otherwise NONE.
        let receiver_services = match self.peer.services {
            PeerServices::Known(flags) => flags,
            PeerServices::Unknown => ServiceFlags::NONE,
        };

        // The version message uses the old Address type for the receiver and sender
        // fields for backwards compatability. For AddrV2 specific transports
        // (Tor/I2P/CJDNS), a dummp address is used and then set later by an AddrV2 message.
        //
        // Convert AddrV2 to SocketAddr for the version message. For non-IP types, use a placeholder IPv6 address.
        let receiver_socket_addr = match &self.peer.address {
            AddrV2::Ipv4(ipv4) => SocketAddr::new(IpAddr::V4(*ipv4), self.peer.port),
            AddrV2::Ipv6(ipv6) => SocketAddr::new(IpAddr::V6(*ipv6), self.peer.port),
            _ => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), self.peer.port),
        };

        let sender_socket_addr = match &self.configuration.sender_address {
            AddrV2::Ipv4(ipv4) => {
                SocketAddr::new(IpAddr::V4(*ipv4), self.configuration.sender_port)
            }
            AddrV2::Ipv6(ipv6) => {
                SocketAddr::new(IpAddr::V6(*ipv6), self.configuration.sender_port)
            }
            _ => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
        };

        let user_agent = match &self.configuration.user_agent {
            Some(agent) => agent.clone(),
            None => BITCOIN_PEERS_USER_AGENT.to_string(),
        };

        let version = VersionMessage {
            // The bitcoin p2p protocol version number.
            version: self
                .configuration
                .protocol_version
                .unwrap_or(MIN_PROTOCOL_VERSION),
            // Services supported by this node.
            services: self.configuration.services,
            // Helps peers synchronize their time and detect significant clock differences.
            timestamp: unix_timestamp(),
            // What we believe of the node, gives them a view of what the network thinks of them.
            receiver: Address::new(&receiver_socket_addr, receiver_services),
            // The address other peers should use to connect to us.
            sender: Address::new(&sender_socket_addr, self.configuration.services),
            // Used to detect connection loops where a node connects to itself.
            nonce,
            // Client identification.
            user_agent,
            // Tell the peer the block height of the blockchain which it is aware of.
            start_height: self.configuration.start_height,
            // Whether to relay transactions to this peer.
            relay: self.configuration.relay,
        };

        NetworkMessage::Version(version)
    }

    /// Performs the bitcoin p2p version handshake protocol.
    ///
    /// Uses the peer information stored in the connection to perform the handshake.
    /// Any services discovered during handshake will update the peer's service flags.
    ///
    /// # Returns
    ///
    /// * `Ok(`[`ServiceFlags`]`)` - The peer's advertised service flags
    /// * `Err(`[`ConnectionError`]`)` - If the handshake failed
    async fn version_handshake(&mut self) -> Result<(), ConnectionError> {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum HandshakeState {
            // Sent version message, but haven't received anything yet.
            VersionSent,
            // Received the peer's version message (and sent verack), but no verack response yet.
            VersionReceived,
            // Received a verack, but no version message yet.
            VerackReceived,
            // Both version and verack received - handshake complete.
            Complete,
        }

        // Generate a nonce for connection loop detection.
        let nonce = generate_nonce();

        let version_message = self.create_version_message(nonce);
        self.send(version_message).await?;

        debug!("Sent version message to peer");
        let mut state = HandshakeState::VersionSent;

        // Keep processing messages until handshake is complete.
        while state != HandshakeState::Complete {
            let message = self.receive().await?;

            match message {
                NetworkMessage::Version(version) => {
                    // Check if the received nonce matches our own (connection loop detection).
                    // While this would be hard to trigger in a non-listening crawler scenario,
                    // there are still some network setups which could loopback.
                    if version.nonce == nonce {
                        error!("Connection loop detected - received same nonce");
                        return Err(ConnectionError::ConnectionLoop);
                    }

                    match state {
                        HandshakeState::VersionSent | HandshakeState::VerackReceived => {
                            debug!("Received version message from peer");

                            // Update our peer with what we learned.
                            self.peer.services = PeerServices::Known(version.services);
                            self.peer.version = PeerProtocolVersion::Known(version.version);

                            // Calculate and store the effective version (minimum of both) of the connection.
                            let effective = std::cmp::min(
                                self.configuration
                                    .protocol_version
                                    .unwrap_or(MIN_PROTOCOL_VERSION),
                                version.version,
                            );

                            {
                                let mut state = self.state.lock().await;
                                state.effective_protocol_version =
                                    PeerProtocolVersion::Known(effective);
                            }

                            // If the effective version is high enough, send protocol negotiation messages.

                            // SendAddrV2 for address format support (BIP155).
                            if effective >= ADDRV2_MIN_PROTOCOL_VERSION {
                                // Send SendAddrV2 message to signal we want AddrV2 format
                                self.send(NetworkMessage::SendAddrV2).await?;
                                let mut state = self.state.lock().await;
                                state.addr_v2 = state.addr_v2.on_send();
                                debug!(
                                    "Sent SendAddrV2 message, addrv2_state: {:?}",
                                    state.addr_v2
                                );
                            }

                            // SendHeaders for header announcements instead of INV.
                            if effective >= SENDHEADERS_MIN_PROTOCOL_VERSION {
                                self.send(NetworkMessage::SendHeaders).await?;
                                let mut state = self.state.lock().await;
                                state.send_headers = state.send_headers.on_send();
                                debug!(
                                    "Sent SendHeaders message, send_headers_state: {:?}",
                                    state.send_headers
                                );
                            }

                            // WtxidRelay for witness transaction ID relay.
                            if effective >= WTXID_RELAY_MIN_PROTOCOL_VERSION {
                                self.send(NetworkMessage::WtxidRelay).await?;
                                let mut state = self.state.lock().await;
                                state.wtxid_relay = state.wtxid_relay.on_send();
                                debug!(
                                    "Sent WtxidRelay message, wtxid_relay_state: {:?}",
                                    state.wtxid_relay
                                );
                            }

                            self.send(NetworkMessage::Verack).await?;
                            debug!("Sent verack message to peer");

                            state = if state == HandshakeState::VerackReceived {
                                HandshakeState::Complete
                            } else {
                                HandshakeState::VersionReceived
                            };
                        }
                        _ => {
                            debug!(
                                "Received duplicate version message in state {state:?}, ignoring"
                            );
                        }
                    }
                }
                NetworkMessage::Verack => match state {
                    HandshakeState::VersionSent | HandshakeState::VersionReceived => {
                        debug!("Received verack from peer");

                        state = if state == HandshakeState::VersionReceived {
                            HandshakeState::Complete
                        } else {
                            HandshakeState::VerackReceived
                        };
                    }
                    _ => {
                        debug!("Received duplicate verack message in state {state:?}, ignoring");
                    }
                },
                _ => {
                    debug!(
                        "Received unexpected message in version handshake: {message:?}, ignoring"
                    );
                }
            }
        }

        debug!("Handshake completed successfully");

        Ok(())
    }
}

impl<R, W> MessageSender for PeerConnection<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        self.sender.send(message).await
    }
}

impl<R, W> MessageReceiver for PeerConnection<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        self.receiver.receive().await
    }
}

impl TcpPeerConnection {
    /// Establish a TCP connection to a bitcoin peer and perform the handshake.
    ///
    /// This method establishes the TCP connection and performs the protocol handshake,
    /// returning a ready connection that can be used for communication.
    ///
    /// # Arguments
    ///
    /// * `peer` - The bitcoin peer to connect to.
    /// * `network` - The bitcoin [`Network`] to use.
    /// * `configuration` - Configuration for the connection.
    ///
    /// # Returns
    ///
    /// * `Ok(`[`TcpPeerConnection`]`)` - A successfully established and handshaked connection
    /// * `Err(`[`ConnectionError`]`)` - If the connection attempt or handshake failed
    async fn tcp(
        peer: Peer,
        network: Network,
        configuration: ConnectionConfiguration,
    ) -> Result<Self, ConnectionError> {
        let ip_addr = match &peer.address {
            AddrV2::Ipv4(ipv4) => IpAddr::V4(*ipv4),
            AddrV2::Ipv6(ipv6) => IpAddr::V6(*ipv6),
            // Other address types (Torv2, Torv3, I2p, Cjdns, etc.) are not supported yet.
            _ => return Err(ConnectionError::UnsupportedAddressType),
        };
        let socket_addr = SocketAddr::new(ip_addr, peer.port);

        let stream =
            match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(socket_addr))
                .await
            {
                Ok(Ok(stream)) => stream,
                Ok(Err(e)) => return Err(ConnectionError::Io(e)),
                Err(_) => {
                    return Err(ConnectionError::TransportFailed(TransportError::Io(
                        io::Error::new(io::ErrorKind::TimedOut, "Connection attempt timed out"),
                    )))
                }
            };

        // Allow small packets which is helpful for p2p protocol.
        stream.set_nodelay(true)?;

        let (mut reader, mut writer) = stream.into_split();
        let transport = match AsyncProtocol::new(
            network,
            Role::Initiator,
            None,
            None,
            &mut reader,
            &mut writer,
        )
        .await
        {
            Ok(protocol) => Transport::v2(protocol),
            Err(_) => return Err(ConnectionError::TransportFailed(TransportError::Encryption)),
        };

        // Split the transport into receiver and sender components
        let (transport_receiver, transport_sender) = transport.split();

        // Create shared state
        let state = Arc::new(Mutex::new(ConnectionState::new()));

        // Create receiver and sender components
        let receiver =
            PeerConnectionReceiver::new(peer.clone(), transport_receiver, reader, state.clone());

        let sender = PeerConnectionSender::new(peer.clone(), transport_sender, writer);

        let mut conn = PeerConnection {
            configuration,
            peer,
            state,
            receiver,
            sender,
        };

        conn.version_handshake().await?;
        Ok(conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::AsyncV1Transport;
    use bitcoin::consensus::encode;
    use bitcoin::p2p::message::NetworkMessage;
    use bitcoin::p2p::message_network::VersionMessage;
    use tokio_test::io::Builder as MockIoBuilder;

    // Helper function to create a test PeerConnection
    fn create_test_connection<R, W>(
        config: ConnectionConfiguration,
        peer: Peer,
        reader: R,
        writer: W,
    ) -> PeerConnection<R, W>
    where
        R: AsyncRead + Unpin + Send,
        W: AsyncWrite + Unpin + Send,
    {
        let transport = Transport::V1(AsyncV1Transport::new(bitcoin::p2p::Magic::BITCOIN));
        let (transport_receiver, transport_sender) = transport.split();
        let state = Arc::new(Mutex::new(ConnectionState::new()));

        let receiver =
            PeerConnectionReceiver::new(peer.clone(), transport_receiver, reader, state.clone());

        let sender = PeerConnectionSender::new(peer.clone(), transport_sender, writer);

        PeerConnection {
            configuration: config,
            peer,
            state,
            receiver,
            sender,
        }
    }

    #[tokio::test]
    async fn test_peer_connection_receive_message() {
        let pong_message = NetworkMessage::Pong(42);
        let raw_msg = bitcoin::p2p::message::RawNetworkMessage::new(
            bitcoin::p2p::Magic::BITCOIN,
            pong_message.clone(),
        );
        let message_bytes = encode::serialize(&raw_msg);

        let mock_reader = MockIoBuilder::new().read(&message_bytes).build();
        let mock_writer = Vec::new();

        let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);

        let config =
            ConnectionConfiguration::non_listening(PeerProtocolVersion::Known(70016), None);

        let mut connection = create_test_connection(config, peer, mock_reader, mock_writer);

        let received = connection.receive().await.unwrap();

        match received {
            NetworkMessage::Pong(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Pong message, got {received:?}"),
        }
    }

    #[tokio::test]
    async fn test_peer_connection_no_auto_ping_pong() {
        // Test that the connection does NOT automatically respond to ping messages.
        // Users must handle ping/pong manually now.

        let ping_message = NetworkMessage::Ping(123);
        let ping_bytes = create_raw_message(bitcoin::p2p::Magic::BITCOIN, ping_message);

        let mock_reader = MockIoBuilder::new().read(&ping_bytes).build();
        let mock_writer = Vec::new();

        let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);

        let config =
            ConnectionConfiguration::non_listening(PeerProtocolVersion::Known(70016), None);

        let mut connection = create_test_connection(config, peer, mock_reader, mock_writer);

        let received = connection.receive().await.unwrap();

        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 123),
            _ => panic!("Expected Ping message, got {received:?}"),
        }

        // The writer should be empty since we don't auto-respond to pings anymore
        // (sender is not accessible in tests, but in real usage no pong would be sent)
    }

    #[tokio::test]
    async fn test_peer_connection_version_handshake() {
        // This test simulates a full version handshake with a peer

        // 1. Create mock I/O that will respond appropriately to our handshake messages
        // First the peer will respond to our version message with their version and verack
        let peer_version = create_version_message(70016, ServiceFlags::NETWORK);
        let peer_version_bytes =
            create_raw_message(bitcoin::p2p::Magic::BITCOIN, peer_version.clone());

        // Then we expect to see the verack from the peer
        let peer_verack = NetworkMessage::Verack;
        let peer_verack_bytes = create_raw_message(bitcoin::p2p::Magic::BITCOIN, peer_verack);

        // Set up mock reader that will return the peer's responses in sequence
        let mock_reader = MockIoBuilder::new()
            .read(&peer_version_bytes) // First they send version
            .read(&peer_verack_bytes) // Then they send verack
            .build();

        // Mock writer will capture our outgoing messages
        let mock_writer = Vec::new();

        // 2. Set up peer connection with mocked I/O
        let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);

        let config =
            ConnectionConfiguration::non_listening(PeerProtocolVersion::Known(70016), None);

        let mut connection = create_test_connection(config, peer.clone(), mock_reader, mock_writer);

        // 3. Perform the handshake
        connection.version_handshake().await.unwrap();

        // 4. Verify peer information was updated
        match connection.peer.services {
            PeerServices::Known(services) => {
                assert_eq!(services, ServiceFlags::NETWORK);
            }
            _ => panic!("Expected known services"),
        }

        match connection.peer.version {
            PeerProtocolVersion::Known(version) => {
                assert_eq!(version, 70016);
            }
            _ => panic!("Expected known version"),
        }

        // 5. Verify connection state was updated with correct effective version
        {
            let state = connection.state.lock().await;
            match state.effective_protocol_version {
                PeerProtocolVersion::Known(version) => {
                    assert_eq!(version, 70016);
                }
                _ => panic!("Expected known effective version"),
            }
        }
    }

    // Helper functions for creating test messages.

    fn create_version_message(version: u32, services: ServiceFlags) -> NetworkMessage {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs() as i64;

        let sender_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8333);
        let receiver_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)), 8333);

        let version_msg = VersionMessage {
            version,
            services,
            timestamp,
            receiver: Address::new(&receiver_addr, ServiceFlags::NONE),
            sender: Address::new(&sender_addr, services),
            nonce: 42,
            user_agent: "/bitcoin-core:23.0/".to_string(),
            start_height: 0,
            relay: true,
        };

        NetworkMessage::Version(version_msg)
    }

    fn create_raw_message(magic: bitcoin::p2p::Magic, message: NetworkMessage) -> Vec<u8> {
        let raw_msg = bitcoin::p2p::message::RawNetworkMessage::new(magic, message);
        encode::serialize(&raw_msg)
    }

    #[tokio::test]
    async fn test_peer_connection_split() {
        // Test that split connections work correctly
        let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);
        let config =
            ConnectionConfiguration::non_listening(PeerProtocolVersion::Known(70016), None);

        // Create test messages
        let ping_message = NetworkMessage::Ping(456);
        let ping_bytes = create_raw_message(bitcoin::p2p::Magic::BITCOIN, ping_message);

        let pong_message = NetworkMessage::Pong(789);
        let pong_bytes = create_raw_message(bitcoin::p2p::Magic::BITCOIN, pong_message);

        // Set up mock reader/writer
        let mock_reader = MockIoBuilder::new()
            .read(&ping_bytes)
            .read(&pong_bytes)
            .build();
        let mock_writer = Vec::new();

        let connection = create_test_connection(config, peer, mock_reader, mock_writer);

        // Split the connection
        let (mut receiver, mut sender) = connection.into_split();

        // Test receiving with the split receiver
        let received = receiver.receive().await.unwrap();
        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 456),
            _ => panic!("Expected Ping message, got {received:?}"),
        }

        // Test that split receiver doesn't automatically respond to pings
        // Now manually send a pong using the sender
        sender.send(NetworkMessage::Pong(456)).await.unwrap();

        // Receive the next message
        let received = receiver.receive().await.unwrap();
        match received {
            NetworkMessage::Pong(nonce) => assert_eq!(nonce, 789),
            _ => panic!("Expected Pong message, got {received:?}"),
        }
    }

    #[tokio::test]
    async fn test_connection_enum_split() {
        // Test the high-level ConnectionReceiver/ConnectionSender enums
        // We'll test the pattern matching and delegation, not the actual TCP connection

        // Create a mock transport receiver and sender for testing
        let transport = Transport::v1(bitcoin::p2p::Magic::BITCOIN);
        let (transport_receiver, transport_sender) = transport.split();

        // Create test message
        let ping_message = NetworkMessage::Ping(999);
        let ping_bytes = create_raw_message(bitcoin::p2p::Magic::BITCOIN, ping_message);

        // Set up mock reader/writer
        let mock_reader = MockIoBuilder::new().read(&ping_bytes).build();
        let mock_writer = Vec::new();

        let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);

        // Create the split connection parts directly
        let receiver = PeerConnectionReceiver::new(
            peer.clone(),
            transport_receiver,
            mock_reader,
            Arc::new(Mutex::new(ConnectionState::new())),
        );
        let sender = PeerConnectionSender::new(peer, transport_sender, mock_writer);

        // Test that we can call methods on the receiver
        let mut receiver = receiver;
        let received = receiver.receive().await.unwrap();
        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 999),
            _ => panic!("Expected Ping message, got {received:?}"),
        }

        // Test that we can call methods on the sender
        let mut sender = sender;
        sender.send(NetworkMessage::Pong(999)).await.unwrap();
    }

    #[tokio::test]
    async fn test_split_receiver_protocol_state() {
        // Test that split receivers properly handle protocol state messages
        let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);
        let config =
            ConnectionConfiguration::non_listening(PeerProtocolVersion::Known(70016), None);

        // Create protocol negotiation messages
        let sendaddrv2 = NetworkMessage::SendAddrV2;
        let sendaddrv2_bytes = create_raw_message(bitcoin::p2p::Magic::BITCOIN, sendaddrv2);

        let sendheaders = NetworkMessage::SendHeaders;
        let sendheaders_bytes = create_raw_message(bitcoin::p2p::Magic::BITCOIN, sendheaders);

        let wtxidrelay = NetworkMessage::WtxidRelay;
        let wtxidrelay_bytes = create_raw_message(bitcoin::p2p::Magic::BITCOIN, wtxidrelay);

        // Set up mock reader with protocol messages
        let mock_reader = MockIoBuilder::new()
            .read(&sendaddrv2_bytes)
            .read(&sendheaders_bytes)
            .read(&wtxidrelay_bytes)
            .build();
        let mock_writer = Vec::new();

        let connection = create_test_connection(config, peer, mock_reader, mock_writer);

        // Split and test receiver state updates
        let (mut receiver, _sender) = connection.into_split();

        // Receive SendAddrV2
        let msg = receiver.receive().await.unwrap();
        assert!(matches!(msg, NetworkMessage::SendAddrV2));
        {
            let state = receiver.state.lock().await;
            assert_eq!(state.addr_v2, AddrV2State::ReceivedOnly);
        }

        // Receive SendHeaders
        let msg = receiver.receive().await.unwrap();
        assert!(matches!(msg, NetworkMessage::SendHeaders));
        {
            let state = receiver.state.lock().await;
            assert_eq!(state.send_headers, SendHeadersState::ReceivedOnly);
        }

        // Receive WtxidRelay
        let msg = receiver.receive().await.unwrap();
        assert!(matches!(msg, NetworkMessage::WtxidRelay));
        {
            let state = receiver.state.lock().await;
            assert_eq!(state.wtxid_relay, WtxidRelayState::ReceivedOnly);
        }
    }

    #[tokio::test]
    async fn test_split_peer_access() {
        // Test that both sender and receiver have access to peer information
        let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1)), 8333);
        let config =
            ConnectionConfiguration::non_listening(PeerProtocolVersion::Known(70016), None);

        let mock_reader = MockIoBuilder::new().build();
        let mock_writer = Vec::new();

        let connection = create_test_connection(config, peer, mock_reader, mock_writer);

        // Split the connection
        let (receiver, sender) = connection.into_split();

        // Check that both halves have access to the peer
        assert_eq!(
            receiver.peer().address,
            AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1))
        );
        assert_eq!(receiver.peer().port, 8333);
        assert_eq!(
            sender.peer().address,
            AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1))
        );
        assert_eq!(sender.peer().port, 8333);
    }
}
