//! Bitcoin p2p protocol connection.
//!
//! See more [p2p documenation](https://developer.bitcoin.org/reference/p2p_networking.html) on the p2p protocol.

use crate::peer::{Peer, PeerProtocolVersion, PeerServices, MIN_PROTOCOL_VERSION};
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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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

/// Runtime state of a connection.
struct ConnectionState {
    /// The protocol version negotiated between peers (minimum of both versions).
    effective_protocol_version: PeerProtocolVersion,
}

impl ConnectionState {
    pub fn new() -> Self {
        ConnectionState {
            effective_protocol_version: PeerProtocolVersion::Unknown,
        }
    }
}

/// Represents a connection to a bitcoin peer.
///
/// This struct manages a connection to a bitcoin peer using the bitcoin p2p protocol.
/// It handles the underlying transport, serialization, protocol management,
/// and connection state (e.g. upgrades).
///
/// # Short Lived
///
/// Designed for short lived connections.
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
pub struct Connection<R, W>
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
/// This is a convenience type alias for [`Connection`] with Tokio's TCP stream halves.
pub type TcpConnection = Connection<OwnedReadHalf, OwnedWriteHalf>;

impl<R, W> Connection<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Get a reference to the peer this connection is established with.
    pub fn peer(&self) -> &Peer {
        &self.peer
    }

    /// Requests peer addresses by sending a getaddr message and collects the responses.
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Peer>)` - A vector of peer information received from the node
    /// * `Err(ConnectionError)` - If an error occurs during the exchange
    pub async fn get_peers(&mut self) -> Result<Vec<Peer>, ConnectionError> {
        // Send GetAddr message
        self.transport
            .send(NetworkMessage::GetAddr, &mut self.writer)
            .await?;

        debug!("Sent getaddr message to peer");

        let mut received_addresses = Vec::new();
        let mut address_count = 0;

        let max_wait = Duration::from_secs(20);
        let start_time = Instant::now();

        while start_time.elapsed() < max_wait {
            // Wait for a message with a short timeout
            let message = match tokio::time::timeout(
                Duration::from_secs(5),
                self.transport.receive(&mut self.reader),
            )
            .await
            {
                Ok(Ok(message)) => message,
                Ok(Err(_)) => return Err(ConnectionError::ProtocolFailed),
                Err(_) => {
                    // Timeout on reading - if we have some addresses, consider it done.
                    if !received_addresses.is_empty() {
                        break;
                    }
                    // Otherwise continue waiting for the overall timeout.
                    continue;
                }
            };
            match message {
                NetworkMessage::Addr(addresses) => {
                    debug!("Received {} peer addresses", addresses.len());
                    address_count += addresses.len();

                    // Process each address (tuple of timestamp and Address struct)
                    for (_, addr) in addresses {
                        // Extract socket address - only IPv4/IPv6 addresses can be converted
                        if let Ok(socket_addr) = addr.socket_addr() {
                            match socket_addr.ip() {
                                IpAddr::V4(ipv4) => received_addresses.push(
                                    Peer::new(AddrV2::Ipv4(ipv4), socket_addr.port())
                                        .with_known_services(addr.services),
                                ),
                                IpAddr::V6(ipv6) => received_addresses.push(
                                    Peer::new(AddrV2::Ipv6(ipv6), socket_addr.port())
                                        .with_known_services(addr.services),
                                ),
                            }
                        }
                    }
                }
                NetworkMessage::AddrV2(addresses) => {
                    debug!("Received {} peer addresses (v2 format)", addresses.len());
                    address_count += addresses.len();
                    for addr_msg in addresses {
                        received_addresses.push(
                            Peer::new(addr_msg.addr, addr_msg.port)
                                .with_known_services(addr_msg.services),
                        );
                    }
                }
                NetworkMessage::Ping(nonce) => {
                    // Simply respond to ping with pong and continue
                    self.transport
                        .send(NetworkMessage::Pong(nonce), &mut self.writer)
                        .await?;
                    debug!("Responded to ping with pong");
                }
                _ => {
                    debug!("Received unexpected message in get_peers: {message:?}, ignoring");
                }
            }

            // If we've received a substantial number of addresses, we can finish early
            if address_count >= 1000 {
                // Configurable threshold
                break;
            }
        }

        debug!(
            "Collected {} total peer addresses",
            received_addresses.len()
        );
        Ok(received_addresses)
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
    /// * `Err(`[`CrawlerError`]`)` - If the handshake failed
    async fn version_handshake(&mut self) -> Result<ServiceFlags, ConnectionError> {
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
        self.transport
            .send(version_message, &mut self.writer)
            .await?;

        debug!("Sent version message to peer");
        let mut state = HandshakeState::VersionSent;
        let mut services = ServiceFlags::NONE;

        // Keep processing messages until handshake is complete.
        while state != HandshakeState::Complete {
            let message = self.transport.receive(&mut self.reader).await?;

            match message {
                NetworkMessage::Version(version) => {
                    // Check if the received nonce matches our own (connection loop detection).
                    // While this would be hard to trigger in a non-listening crawler scenario,
                    // there are still some network setups which could loopback.
                    if version.nonce == nonce {
                        error!("Connection loop detected - received same nonce");
                        return Err(ConnectionError::ConnectionLoop);
                    }

                    // Determine if we can process this version message
                    match state {
                        HandshakeState::VersionSent | HandshakeState::VerackReceived => {
                            debug!("Received version message from peer");
                            services = version.services;

                            // Update our peer's services with what we learned.
                            self.peer.services = PeerServices::Known(services);
                            // Store the peer's protocol version
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

                            // Maybe send a SendAddrV2 here to upgrade the connection state.

                            // Send verack
                            self.transport
                                .send(NetworkMessage::Verack, &mut self.writer)
                                .await?;
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

        Ok(services)
    }
}

impl TcpConnection {
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
    /// * `Ok(`[`TcpConnection`]`)` - A successfully established and handshaked connection
    /// * `Err(`[`CrawlerError`]`)` - If the connection attempt or handshake failed
    pub async fn tcp(
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

        let mut conn = Connection {
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
