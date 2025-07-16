//! TCP-specific connections.

use super::configuration::TransportPolicy;
use super::{
    AsyncConnection, AsyncConnectionReceiver, AsyncConnectionSender, ConnectionConfiguration,
    ConnectionError,
};
use crate::peer::{Peer, PeerServices};
use crate::transport::{Transport, TransportError};
use crate::PeerProtocolVersion;
use bip324::Role;
use bitcoin::p2p::address::AddrV2;
use bitcoin::p2p::ServiceFlags;
use bitcoin::Network;
use std::net::{IpAddr, SocketAddr};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

/// A TCP-based connection to a bitcoin peer.
///
/// This is a convenience type alias for [`AsyncConnection`] with Tokio's TCP stream halves.
pub type TcpConnection = AsyncConnection<BufReader<OwnedReadHalf>, OwnedWriteHalf>;

/// A TCP-based connection receiver.
///
/// This is a convenience type alias for [`AsyncConnectionReceiver`] with Tokio's TCP read half.
pub type TcpConnectionReceiver = AsyncConnectionReceiver<BufReader<OwnedReadHalf>>;

/// A TCP-based connection sender.
///
/// This is a convenience type alias for [`AsyncConnectionSender`] with Tokio's TCP write half.
pub type TcpConnectionSender = AsyncConnectionSender<OwnedWriteHalf>;

/// Create a Peer from an incoming TCP connection.
fn peer_from_socket_addr(socket_addr: SocketAddr) -> Peer {
    let address = match socket_addr.ip() {
        IpAddr::V4(ipv4) => AddrV2::Ipv4(ipv4),
        IpAddr::V6(ipv6) => AddrV2::Ipv6(ipv6),
    };

    Peer {
        address,
        port: socket_addr.port(),
        services: PeerServices::Unknown,
        version: PeerProtocolVersion::Unknown,
    }
}

/// Configure TCP stream for Bitcoin P2P protocol usage.
///
/// Sets TCP_NODELAY to true, which disables Nagle's algorithm. This is beneficial
/// for the bitcoin p2p protocol since it uses many small messages where latency
/// is more important than bandwidth efficiency.
fn configure_tcp_stream(stream: &TcpStream) -> Result<(), ConnectionError> {
    stream.set_nodelay(true)?;
    Ok(())
}

/// Helper function to establish TCP connection and configure it for Bitcoin P2P.
async fn establish_tcp_connection(socket_addr: SocketAddr) -> Result<TcpStream, ConnectionError> {
    let stream = TcpStream::connect(socket_addr).await?;
    configure_tcp_stream(&stream)?;
    Ok(stream)
}

/// Negotiate transport protocol for outbound TCP connection.
async fn negotiate_outbound_transport(
    socket_addr: SocketAddr,
    network: Network,
    peer: &Peer,
    policy: TransportPolicy,
) -> Result<Transport<BufReader<OwnedReadHalf>, OwnedWriteHalf>, ConnectionError> {
    let peer_supports_v2 = match peer.services {
        PeerServices::Unknown => true,
        PeerServices::Known(flags) => flags.has(ServiceFlags::P2P_V2),
    };

    // Only skip v2 attempt if two criteria are met:
    // 1. Connection configuration policy allows for v1 fallback (V2Preferred).
    // 2. Peer explicitly doesn't advertise v2 support.
    if !peer_supports_v2 && matches!(policy, TransportPolicy::V2Preferred) {
        let stream = establish_tcp_connection(socket_addr).await?;
        let (reader, writer) = stream.into_split();
        let buf_reader = BufReader::new(reader);
        log::info!(
            "Using v1 plaintext connection to {:?} (no P2P_V2 flag)",
            peer.address
        );
        return Ok(Transport::v1(network.magic(), buf_reader, writer));
    }

    let stream = establish_tcp_connection(socket_addr).await?;
    let (reader, writer) = stream.into_split();
    let buf_reader = BufReader::new(reader);

    match bip324::futures::Protocol::new(network, Role::Initiator, None, None, buf_reader, writer)
        .await
    {
        Ok(v2) => {
            log::info!(
                "Successfully established v2 encrypted connection to {:?}",
                peer.address
            );
            Ok(Transport::v2(v2))
        }
        Err(e) => {
            log::debug!("V2 handshake failed for {:?}: {:?}", peer.address, e);

            match policy {
                TransportPolicy::V2Required => {
                    log::error!("V2 transport required but could not be established");
                    Err(ConnectionError::V2TransportRequired)
                }
                TransportPolicy::V2Preferred => {
                    // Need fresh connection for v1 since v2 protocol probably caused disconnection.
                    let stream = establish_tcp_connection(socket_addr).await?;
                    let (reader, writer) = stream.into_split();
                    let buf_reader = BufReader::new(reader);

                    log::info!("Using v1 plaintext connection to {:?}", peer.address);
                    Ok(Transport::v1(network.magic(), buf_reader, writer))
                }
            }
        }
    }
}

/// Negotiate transport protocol for inbound TCP connection.
///
/// For inbound connections, we act as the responder and must detect whether
/// the peer is attempting a v2 (BIP324) or v1 (legacy) connection by checking
/// the first 4 bytes for network magic.
async fn negotiate_inbound_transport(
    network: Network,
    mut reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    policy: TransportPolicy,
) -> Result<Transport<BufReader<OwnedReadHalf>, OwnedWriteHalf>, ConnectionError> {
    // Peek at first 4 bytes to detect protocol.
    let peeked = reader.fill_buf().await?;
    if peeked.len() >= 4 && peeked[0..4] == network.magic().to_bytes() {
        match policy {
            TransportPolicy::V2Required => {
                log::error!("V1 inbound connection detected but V2 transport required");
                return Err(ConnectionError::V2TransportRequired);
            }
            TransportPolicy::V2Preferred => {
                log::info!("Detected v1 plaintext inbound connection");
                return Ok(Transport::v1(network.magic(), reader, writer));
            }
        }
    }

    // Not v1 magic, attempt v2 handshake.
    match bip324::futures::Protocol::new(network, Role::Responder, None, None, reader, writer).await
    {
        Ok(v2) => {
            log::info!("Successfully established v2 encrypted inbound connection");
            Ok(Transport::v2(v2))
        }
        Err(e) => {
            log::error!("V2 handshake failed for inbound connection: {e:?}");
            Err(ConnectionError::TransportFailed(TransportError::Encryption))
        }
    }
}

/// Establish a TCP connection to a bitcoin peer and perform the handshake.
///
/// This function handles:
/// 1. TCP connection establishment.
/// 2. TCP socket configuration (nodelay).
/// 3. Transport protocol negotiation (tries v2, falls back to v1).
/// 4. Version handshake.
///
/// # Timeouts
///
/// This function does not enforce any connection timeout. Callers should wrap
/// the call with `tokio::time::timeout` if a timeout is desired:
///
/// ```no_run
/// # use bitcoin::Network;
/// # use bitcoin_peers_connection::{Connection, ConnectionConfiguration, Peer, PeerProtocolVersion, TransportPolicy, FeaturePreferences};
/// # use bitcoin::p2p::address::AddrV2;
/// # use std::net::Ipv4Addr;
/// # use std::time::Duration;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// # let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);
/// # let config = ConnectionConfiguration::non_listening(
/// #     PeerProtocolVersion::Known(70016),
/// #     TransportPolicy::V2Required,
/// #     FeaturePreferences::default(),
/// #     None,
/// # );
/// let connection = tokio::time::timeout(
///     Duration::from_secs(30),
///     Connection::tcp(peer, Network::Bitcoin, config)
/// ).await??;
/// # Ok(())
/// # }
/// ```
///
/// # Arguments
///
/// * `peer` - The bitcoin peer to connect to.
/// * `network` - The bitcoin network to use.
/// * `configuration` - Configuration for the connection.
///
/// # Returns
///
/// A fully established and handshaked connection ready for use.
pub async fn connect(
    peer: Peer,
    network: Network,
    configuration: ConnectionConfiguration,
) -> Result<TcpConnection, ConnectionError> {
    // Convert peer address to socket address
    let ip_addr = match &peer.address {
        AddrV2::Ipv4(ipv4) => IpAddr::V4(*ipv4),
        AddrV2::Ipv6(ipv6) => IpAddr::V6(*ipv6),
        _ => return Err(ConnectionError::UnsupportedAddressType),
    };
    let socket_addr = SocketAddr::new(ip_addr, peer.port);

    // Negotiate transport based on peer capabilities and configuration policy.
    let transport =
        negotiate_outbound_transport(socket_addr, network, &peer, configuration.transport_policy)
            .await?;

    let mut connection = AsyncConnection::new(peer, configuration, transport);
    super::handshake::perform_handshake(&mut connection).await?;

    Ok(connection)
}

/// Accept an incoming TCP connection from a bitcoin peer and perform the handshake.
///
/// # Arguments
///
/// * `stream` - The incoming TCP stream from a connecting peer.
/// * `network` - The bitcoin network to use.
/// * `configuration` - Configuration for the connection.
///
/// # Returns
///
/// A fully established and handshaked connection ready for use.
///
/// # Notes
///
/// Unlike outbound connections where we know the peer details beforehand,
/// inbound connections start with unknown peer information that gets discovered
/// during the handshake process.
pub async fn accept(
    stream: TcpStream,
    network: Network,
    configuration: ConnectionConfiguration,
) -> Result<TcpConnection, ConnectionError> {
    configure_tcp_stream(&stream)?;

    let peer_addr = stream.peer_addr()?;
    let peer = peer_from_socket_addr(peer_addr);

    let (reader, writer) = stream.into_split();
    let buf_reader = BufReader::new(reader);

    let transport =
        negotiate_inbound_transport(network, buf_reader, writer, configuration.transport_policy)
            .await?;
    let mut connection = AsyncConnection::new(peer, configuration, transport);

    // Perform handshake (peer will send version first, we respond).
    super::handshake::perform_handshake(&mut connection).await?;

    Ok(connection)
}
