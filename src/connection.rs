//! Bitcoin p2p protocol connection.
//!
//! See more [p2p documenation](https://developer.bitcoin.org/reference/p2p_networking.html) on the p2p protocol.

use crate::peer::{
    Peer, PeerProtocolVersion, PeerServices, ADDRV2_MIN_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION,
    SENDHEADERS_MIN_PROTOCOL_VERSION, WTXID_RELAY_MIN_PROTOCOL_VERSION,
};
use crate::v1::AsyncV1Transport;
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

#[derive(Debug)]
pub enum ConnectionError {
    Io(io::Error),
    TransportFailed,
    ProtocolFailed,
    UnsupportedAddressType,
    ConnectionLoop,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionError::Io(err) => write!(f, "Connection error: {err}"),
            ConnectionError::TransportFailed => {
                write!(f, "Transport layer failed in peer connection")
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
            ConnectionError::TransportFailed => None,
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

/// Non-connectable address used in version messages.
///
/// This address signals to peers that we are not accepting incoming connections
/// and should not be advertised to other nodes.
const NON_CONNECTABLE_ADDRESS: AddrV2 = AddrV2::Ipv4(Ipv4Addr::new(0, 0, 0, 0));
const NON_CONNECTABLE_PORT: u16 = 0;

/// Represents the transport protocol used for the connection.
enum Transport {
    /// V2 protocol using BIP-324 encrypted transport.
    V2(AsyncProtocol),
    /// V1 protocol with plaintext messages.
    #[allow(dead_code)]
    V1(AsyncV1Transport),
}

impl Transport {
    /// Send a bitcoin network message to the transport.
    async fn send<W>(
        &mut self,
        message: NetworkMessage,
        writer: &mut W,
    ) -> Result<(), ConnectionError>
    where
        W: AsyncWrite + Unpin + Send,
    {
        match self {
            Transport::V2(v2) => {
                let data = bip324::serde::serialize(message)
                    .map_err(|_| ConnectionError::ProtocolFailed)?;

                v2.writer()
                    .encrypt_and_write(&data, writer)
                    .await
                    .map_err(|_| ConnectionError::TransportFailed)
            }
            Transport::V1(v1) => v1.send(message, writer).await.map_err(ConnectionError::Io),
        }
    }

    /// Receive a bitcoin network message from the transport.
    async fn receive<R>(&mut self, reader: &mut R) -> Result<NetworkMessage, ConnectionError>
    where
        R: AsyncRead + Unpin + Send,
    {
        match self {
            Transport::V2(v2) => {
                let message = v2
                    .reader()
                    .read_and_decrypt(reader)
                    .await
                    .map_err(|_| ConnectionError::TransportFailed)?;

                bip324::serde::deserialize(message.contents())
                    .map_err(|_| ConnectionError::ProtocolFailed)
            }
            Transport::V1(v1) => v1
                .receive(reader)
                .await
                .map_err(|_| ConnectionError::ProtocolFailed),
        }
    }
}

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
    /// Creates a new configuration for a non-connectable node.
    ///
    /// This configuration advertises no services, uses a non-connectable address,
    /// and doesn't relay transactions. It's suitable for crawlers and other
    /// light client software that just wants to query the network without serving data.
    ///
    /// # Arguments
    ///
    /// * `protocol_version` - The protocol version to advertise. Defaults to MIN_PROTOCOL_VERSION if Unknown.
    /// * `user_agent` - Optional custom user agent string. Defaults to bitcoin-peers default if None.
    ///
    /// # Returns
    ///
    /// A new ConnectionConfiguration configured for a non-connectable node.
    pub fn non_connectable(
        protocol_version: PeerProtocolVersion,
        user_agent: Option<String>,
    ) -> Self {
        Self {
            protocol_version,
            user_agent,
            services: ServiceFlags::NONE,
            sender_address: NON_CONNECTABLE_ADDRESS,
            sender_port: NON_CONNECTABLE_PORT,
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
    /// Runtime state of the connection.
    state: ConnectionState,
    /// The peer this connection is established with.
    peer: Peer,
    /// Transport handles serialization and encryption of messages over the connection.
    transport: Transport,
    reader: R,
    writer: W,
}

/// A TCP-based connection to a bitcoin peer.
///
/// This is a convenience type alias for [`PeerConnection`] with Tokio's TCP stream halves.
pub type TcpPeerConnection = PeerConnection<OwnedReadHalf, OwnedWriteHalf>;

/// Provides a unified interface to different types of bitcoin peer connections.
///
/// This enum is the primary API for interacting with bitcoin peers. It abstracts over
/// different connection transport types (TCP, WebSocket, Tor, etc.) and provides a
/// consistent interface for sending and receiving messages.
///
/// Using this enum instead of the specific connection types allows for more flexible
/// code that can work with any connection type without needing to know the implementation
/// details.
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
        MessageSender::send(self, message).await
    }

    /// Receive a message from the peer.
    pub async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        MessageReceiver::receive(self).await
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
                            self.state.effective_protocol_version =
                                PeerProtocolVersion::Known(effective);

                            // If the effective version is high enough, send protocol negotiation messages.

                            // SendAddrV2 for address format support (BIP155).
                            if effective >= ADDRV2_MIN_PROTOCOL_VERSION {
                                // Send SendAddrV2 message to signal we want AddrV2 format
                                self.send(NetworkMessage::SendAddrV2).await?;
                                self.state.addr_v2 = self.state.addr_v2.on_send();
                                debug!(
                                    "Sent SendAddrV2 message, addrv2_state: {:?}",
                                    self.state.addr_v2
                                );
                            }

                            // SendHeaders for header announcements instead of INV.
                            if effective >= SENDHEADERS_MIN_PROTOCOL_VERSION {
                                self.send(NetworkMessage::SendHeaders).await?;
                                self.state.send_headers = self.state.send_headers.on_send();
                                debug!(
                                    "Sent SendHeaders message, send_headers_state: {:?}",
                                    self.state.send_headers
                                );
                            }

                            // WtxidRelay for witness transaction ID relay.
                            if effective >= WTXID_RELAY_MIN_PROTOCOL_VERSION {
                                self.send(NetworkMessage::WtxidRelay).await?;
                                self.state.wtxid_relay = self.state.wtxid_relay.on_send();
                                debug!(
                                    "Sent WtxidRelay message, wtxid_relay_state: {:?}",
                                    self.state.wtxid_relay
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
        self.transport.send(message, &mut self.writer).await
    }
}

impl<R, W> MessageReceiver for PeerConnection<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        let message = self.transport.receive(&mut self.reader).await?;

        // Handle protocol-level messages that affect connection state.
        match &message {
            NetworkMessage::SendAddrV2 => {
                self.state.addr_v2 = self.state.addr_v2.on_receive();
                debug!(
                    "Received SendAddrV2 message, addrv2_state: {:?}",
                    self.state.addr_v2
                );
            }
            NetworkMessage::SendHeaders => {
                self.state.send_headers = self.state.send_headers.on_receive();
                debug!(
                    "Received SendHeaders message, send_headers_state: {:?}",
                    self.state.send_headers
                );
            }
            NetworkMessage::WtxidRelay => {
                self.state.wtxid_relay = self.state.wtxid_relay.on_receive();
                debug!(
                    "Received WtxidRelay message, wtxid_relay_state: {:?}",
                    self.state.wtxid_relay
                );
            }
            NetworkMessage::Ping(nonce) => {
                // Automatically respond to pings with pongs.
                self.send(NetworkMessage::Pong(*nonce)).await?;
                debug!("Responded to ping with pong, nonce: {}", nonce);
            }
            _ => {}
        }

        Ok(message)
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
                Err(_) => return Err(ConnectionError::TransportFailed),
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
            Ok(protocol) => Transport::V2(protocol),
            Err(_) => return Err(ConnectionError::TransportFailed),
        };

        let mut conn = PeerConnection {
            configuration,
            state: ConnectionState::new(),
            transport,
            reader,
            writer,
            peer,
        };

        conn.version_handshake().await?;
        Ok(conn)
    }
}
