//! Bitcoin p2p protocol connection.
//!
//! See more [p2p documenation](https://developer.bitcoin.org/reference/p2p_networking.html) on the p2p protocol.

use bip324::serde::{deserialize, serialize};
use bip324::{AsyncProtocol, Role};
use bitcoin::p2p::address::{AddrV2, Address};
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::p2p::ServiceFlags;
use bitcoin::Network;
use log::{debug, error};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::process;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

use crate::error::PeersError;
use crate::peer::Peer;

/// User agent string sent in version messages.
///
/// This identifies crawler software to other peers on the network.
/// Format follows Bitcoin Core's convention: "/$NAME:$VERSION/".
const BITCOIN_PEERS_USER_AGENT: &str = concat!("/bitcoin-peers:", env!("CARGO_PKG_VERSION"), "/");

/// Non-connectable address used in version messages.
///
/// This address signals to peers that we are not accepting incoming connections
/// and should not be advertised to other nodes.
const NON_CONNECTABLE_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0);

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
    /// Custom user agent advertised for connection. Default is `/bitcoin-peers:$VERSION/`.
    user_agent: Option<String>,
    transport: AsyncProtocol,
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
    /// Requests peer addresses by sending a getaddr message and collects the responses.
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Peer>)` - A vector of peer information received from the node
    /// * `Err(PeersError)` - If an error occurs during the exchange
    pub async fn get_peers(&mut self) -> Result<Vec<Peer>, PeersError> {
        let getaddr_message =
            serialize(NetworkMessage::GetAddr).map_err(|_| PeersError::PeerConnectionFailed)?;

        self.transport
            .writer()
            .encrypt_and_write(&getaddr_message, &mut self.writer)
            .await
            .map_err(|_| PeersError::PeerConnectionFailed)?;

        debug!("Sent getaddr message to peer");

        let mut received_addresses = Vec::new();
        let mut address_count = 0;

        let max_wait = Duration::from_secs(20);
        let start_time = Instant::now();

        while start_time.elapsed() < max_wait {
            // Wait for a message with a short timeout
            let response = match tokio::time::timeout(
                Duration::from_secs(5),
                self.transport.reader().read_and_decrypt(&mut self.reader),
            )
            .await
            {
                Ok(Ok(response)) => response,
                Ok(Err(_)) => return Err(PeersError::PeerConnectionFailed),
                Err(_) => {
                    // Timeout on reading - if we have some addresses, consider it done.
                    if !received_addresses.is_empty() {
                        break;
                    }
                    // Otherwise continue waiting for the overall timeout.
                    continue;
                }
            };

            match deserialize(response.contents()) {
                Ok(NetworkMessage::Addr(addresses)) => {
                    debug!("Received {} peer addresses", addresses.len());
                    address_count += addresses.len();

                    // Process each address (tuple of timestamp and Address struct)
                    for (_, addr) in addresses {
                        // Extract socket address - only IPv4/IPv6 addresses can be converted
                        if let Ok(socket_addr) = addr.socket_addr() {
                            match socket_addr.ip() {
                                IpAddr::V4(ipv4) => received_addresses.push(Peer {
                                    address: AddrV2::Ipv4(ipv4),
                                    port: socket_addr.port(),
                                    services: addr.services,
                                }),
                                IpAddr::V6(ipv6) => received_addresses.push(Peer {
                                    address: AddrV2::Ipv6(ipv6),
                                    port: socket_addr.port(),
                                    services: addr.services,
                                }),
                            }
                        }
                    }
                }
                Ok(NetworkMessage::AddrV2(addresses)) => {
                    debug!("Received {} peer addresses (v2 format)", addresses.len());
                    address_count += addresses.len();
                    for addr_msg in addresses {
                        received_addresses.push(Peer {
                            address: addr_msg.addr,
                            port: addr_msg.port,
                            services: addr_msg.services,
                        });
                    }
                }
                Ok(NetworkMessage::Ping(nonce)) => {
                    // Simply respond to ping with pong and continue,
                    // just in case this function is hanging around for awhile.
                    let pong_message = serialize(NetworkMessage::Pong(nonce))
                        .map_err(|_| PeersError::PeerConnectionFailed)?;

                    self.transport
                        .writer()
                        .encrypt_and_write(&pong_message, &mut self.writer)
                        .await
                        .map_err(|_| PeersError::PeerConnectionFailed)?;

                    debug!("Responded to ping with pong");
                }
                Ok(message) => {
                    debug!(
                        "Received unexpected message in version handshake: {:?}, ignoring",
                        message
                    );
                }
                Err(e) => {
                    error!("Failed to deserialize message: {:?}", e);
                    return Err(PeersError::PeerConnectionFailed);
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

    /// Creates a bitcoin version message for the given peer address.
    ///
    /// # Arguments
    ///
    /// * `peer_addr` - The address of the peer in AddrV2 format.
    /// * `port` - The port number to connect to.
    /// * `nonce` - The nonce value to use for detecting connection loops.
    /// * `receiver_services` - Optional services we believe the receiver supports. Defaults to [`ServiceFlags::NONE`].
    ///
    /// # Returns
    ///
    /// A [`NetworkMessage::Version`] containing the version information.
    fn create_version_message(
        &self,
        peer_addr: &AddrV2,
        port: u16,
        nonce: u64,
        receiver_services: Option<ServiceFlags>,
    ) -> NetworkMessage {
        // Use provided value or default for to NONE for unknown.
        let receiver_services = receiver_services.unwrap_or(ServiceFlags::NONE);

        // The version message uses the old Address type for the receiver and sender
        // fields for backwards compatability. For AddrV2 specific transports
        // (Tor/I2P/CJDNS), a dummp address is used and then set later by an AddrV2 message.
        //
        // Convert AddrV2 to SocketAddr for the version message. For non-IP types, use a placeholder IPv6 address.
        let receiver_socket_addr = match peer_addr {
            AddrV2::Ipv4(ipv4) => SocketAddr::new(IpAddr::V4(*ipv4), port),
            AddrV2::Ipv6(ipv6) => SocketAddr::new(IpAddr::V6(*ipv6), port),
            _ => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
        };

        let user_agent = match &self.user_agent {
            Some(agent) => agent.clone(),
            None => BITCOIN_PEERS_USER_AGENT.to_string(),
        };

        let version = VersionMessage {
            // The bitcoin p2p protocol version number.
            //
            // A peer crawler is not going to making use of these features, but using
            // too low of version could get filtered by peers.
            //
            // Using 70016 because we're interested in compact block filter support (BIP 158),
            // even though we don't implement the feature ourselves. This allows nodes to
            // accurately signal their filter capabilities to the crawler.
            version: 70016,
            // For a crawler that only connects outbound and doesn't serve data, no services are supported.
            services: ServiceFlags::NONE,
            // Helps peers synchronize their time and detect significant clock differences.
            timestamp: unix_timestamp(),
            // What we believe of the node, gives them a view of what the network thinks of them.
            receiver: Address::new(&receiver_socket_addr, receiver_services),
            // Let the receiving peer know we are not connectable.
            // The 0.0.0.0 address is a common signal to not connect back and do not advertise to other nodes.
            sender: Address::new(&NON_CONNECTABLE_ADDRESS, ServiceFlags::NONE),
            // Used to detect connection loops where a node connects to itself.
            nonce,
            // Client identification.
            user_agent,
            // Tell the peer the block height of the blockchain which it is aware of. Doesn't make
            // much sense for a crawler which isn't tracking the blockchain.
            //
            // Hopefully won't be treated as a node in Initial Block Download (IBD) mode.
            start_height: 0,
            // Crawler doesn't need to receive any beyond the things explicitly requested.
            relay: false,
        };

        NetworkMessage::Version(version)
    }

    /// Performs the bitcoin p2p version handshake protocol.
    ///
    /// # Arguments
    ///
    /// * `peer_addr` - The address of the peer in [`AddrV2`] format
    /// * `peer_port` - The port number to connect to
    /// * `peer_services` - Optional services we believe the peer supports
    ///
    /// # Returns
    ///
    /// * `Ok(`[`ServiceFlags`]`)` - The peer's advertised service flags
    /// * `Err(`[`CrawlerError`]`)` - If the handshake failed
    pub async fn version_handshake(
        &mut self,
        peer_addr: &AddrV2,
        peer_port: u16,
        peer_services: Option<ServiceFlags>,
    ) -> Result<ServiceFlags, PeersError> {
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

        // Send version message.
        let version_message =
            serialize(self.create_version_message(peer_addr, peer_port, nonce, peer_services))
                .map_err(|_| PeersError::PeerConnectionFailed)?;

        self.transport
            .writer()
            .encrypt_and_write(&version_message, &mut self.writer)
            .await
            .map_err(|_| PeersError::PeerConnectionFailed)?;

        debug!("Sent version message to peer");
        let mut state = HandshakeState::VersionSent;
        let mut services = ServiceFlags::NONE;

        // Keep processing messages until handshake is complete.
        while state != HandshakeState::Complete {
            let response = self
                .transport
                .reader()
                .read_and_decrypt(&mut self.reader)
                .await
                .map_err(|_| PeersError::PeerConnectionFailed)?;

            match deserialize(response.contents()) {
                Ok(NetworkMessage::Version(version)) => {
                    // Check if the received nonce matches our own (connection loop detection).
                    // While this would be hard to trigger in a non-listening crawler scenario,
                    // there are still some network setups which could loopback.
                    if version.nonce == nonce {
                        error!("Connection loop detected - received same nonce");
                        return Err(PeersError::ConnectionLoop);
                    }

                    // Determine if we can process this version message
                    match state {
                        HandshakeState::VersionSent | HandshakeState::VerackReceived => {
                            debug!("Received version message from peer");
                            services = version.services;

                            // Maybe send a SendAddrV2 here to upgrade the connection state.

                            let verack_message = serialize(NetworkMessage::Verack)
                                .map_err(|_| PeersError::PeerConnectionFailed)?;
                            self.transport
                                .writer()
                                .encrypt_and_write(&verack_message, &mut self.writer)
                                .await
                                .map_err(|_| PeersError::PeerConnectionFailed)?;

                            debug!("Sent verack message to peer");

                            state = if state == HandshakeState::VerackReceived {
                                HandshakeState::Complete
                            } else {
                                HandshakeState::VersionReceived
                            };
                        }
                        _ => {
                            debug!(
                                "Received duplicate version message in state {:?}, ignoring",
                                state
                            );
                        }
                    }
                }
                Ok(NetworkMessage::Verack) => match state {
                    HandshakeState::VersionSent | HandshakeState::VersionReceived => {
                        debug!("Received verack from peer");

                        state = if state == HandshakeState::VersionReceived {
                            HandshakeState::Complete
                        } else {
                            HandshakeState::VerackReceived
                        };
                    }
                    _ => {
                        debug!(
                            "Received duplicate verack message in state {:?}, ignoring",
                            state
                        );
                    }
                },
                Ok(message) => {
                    debug!(
                        "Received unexpected message in version handshake: {:?}, ignoring",
                        message
                    );
                }
                Err(e) => {
                    error!("Failed to deserialize message: {:?}", e);
                    return Err(PeersError::PeerConnectionFailed);
                }
            }
        }

        debug!("Handshake completed successfully");
        Ok(services)
    }
}

impl TcpConnection {
    /// Establish a TCP connection to a bitcoin peer.
    ///
    /// # Arguments
    ///
    /// * `address` - The Bitcoin peer address in [`AddrV2`] format
    /// * `port` - The port to connect to (typically 8333 for mainnet)
    /// * `network` - The Bitcoin [`Network`] to use
    /// * `user_agent` - Optional custom user agent string to use in version messages
    ///
    /// # Returns
    ///
    /// * `Ok(`[`TcpConnection`]`)` - A successfully established connection
    /// * `Err(`[`CrawlerError`]`)` - If the connection attempt failed
    pub async fn tcp(
        address: &AddrV2,
        port: u16,
        network: Network,
        user_agent: Option<String>,
    ) -> Result<Self, PeersError> {
        let ip_addr = match &address {
            AddrV2::Ipv4(ipv4) => IpAddr::V4(*ipv4),
            AddrV2::Ipv6(ipv6) => IpAddr::V6(*ipv6),
            // Other address types (Torv2, Torv3, I2p, Cjdns, etc.) are not supported yet.
            _ => return Err(PeersError::UnsupportedAddressType),
        };
        let socket_addr = SocketAddr::new(ip_addr, port);

        let stream =
            match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(socket_addr))
                .await
            {
                Ok(Ok(stream)) => stream,
                Ok(Err(e)) => return Err(PeersError::ConnectionError(e)),
                Err(_) => return Err(PeersError::PeerConnectionFailed),
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
            Ok(transport) => transport,
            Err(_) => return Err(PeersError::PeerConnectionFailed),
        };

        Ok(Connection {
            transport,
            reader,
            writer,
            user_agent,
        })
    }
}
