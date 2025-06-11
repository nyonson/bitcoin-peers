//! TCP-specific connections.

use super::configuration::TransportPolicy;
use super::{
    AsyncConnection, AsyncConnectionReceiver, AsyncConnectionSender, ConnectionConfiguration,
    ConnectionError,
};
use crate::peer::{Peer, PeerServices};
use crate::transport::Transport;
use crate::PeerProtocolVersion;
use bip324::{AsyncProtocol, Role};
use bitcoin::p2p::address::AddrV2;
use bitcoin::p2p::ServiceFlags;
use bitcoin::Network;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

/// A TCP-based connection to a bitcoin peer.
///
/// This is a convenience type alias for [`AsyncConnection`] with Tokio's TCP stream halves.
pub type TcpConnection = AsyncConnection<OwnedReadHalf, OwnedWriteHalf>;

/// A TCP-based connection receiver.
///
/// This is a convenience type alias for [`AsyncConnectionReceiver`] with Tokio's TCP read half.
pub type TcpConnectionReceiver = AsyncConnectionReceiver<OwnedReadHalf>;

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

/// Helper function to establish TCP connection with timeout and nodelay.
async fn establish_tcp_connection(
    socket_addr: SocketAddr,
    timeout: Duration,
) -> Result<TcpStream, ConnectionError> {
    match tokio::time::timeout(timeout, TcpStream::connect(socket_addr)).await {
        Ok(Ok(stream)) => {
            configure_tcp_stream(&stream)?;
            Ok(stream)
        }
        Ok(Err(e)) => Err(ConnectionError::Io(e)),
        Err(_) => Err(ConnectionError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "Connection attempt timed out",
        ))),
    }
}

/// Negotiate transport protocol for outbound TCP connection.
async fn negotiate_outbound_transport(
    socket_addr: SocketAddr,
    network: Network,
    peer: &Peer,
    policy: TransportPolicy,
    connection_timeout: Duration,
) -> Result<(Transport, OwnedReadHalf, OwnedWriteHalf), ConnectionError> {
    let peer_supports_v2 = match peer.services {
        PeerServices::Unknown => true,
        PeerServices::Known(flags) => flags.has(ServiceFlags::P2P_V2),
    };

    // Only skip v2 attempt if two criteria are met:
    // 1. Connection configuration policy allows for v1 fallback (V2Preferred).
    // 2. Peer explicitly doesn't advertise v2 support.
    if !peer_supports_v2 && matches!(policy, TransportPolicy::V2Preferred) {
        let stream = establish_tcp_connection(socket_addr, connection_timeout).await?;
        let (reader, writer) = stream.into_split();
        log::info!(
            "Using v1 plaintext connection to {:?} (no P2P_V2 flag)",
            peer.address
        );
        return Ok((Transport::v1(network.magic()), reader, writer));
    }

    let stream = establish_tcp_connection(socket_addr, connection_timeout).await?;
    let (mut reader, mut writer) = stream.into_split();

    match AsyncProtocol::new(
        network,
        Role::Initiator,
        None,
        None,
        &mut reader,
        &mut writer,
    )
    .await
    {
        Ok(v2) => {
            log::info!(
                "Successfully established v2 encrypted connection to {:?}",
                peer.address
            );
            Ok((Transport::v2(v2), reader, writer))
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
                    let stream = establish_tcp_connection(socket_addr, connection_timeout).await?;
                    let (reader, writer) = stream.into_split();

                    log::info!("Using v1 plaintext connection to {:?}", peer.address);
                    Ok((Transport::v1(network.magic()), reader, writer))
                }
            }
        }
    }
}

/// Negotiate transport protocol for inbound TCP connection.
///
/// For inbound connections, we act as the responder and must detect whether
/// the peer is attempting a v2 (BIP324) or v1 (legacy) connection.
async fn negotiate_inbound_transport(
    network: Network,
    mut reader: OwnedReadHalf,
    mut writer: OwnedWriteHalf,
    policy: TransportPolicy,
) -> Result<(Transport, OwnedReadHalf, OwnedWriteHalf), ConnectionError> {
    // Try to establish v2 transport as responder.
    match AsyncProtocol::new(
        network,
        Role::Responder,
        None,
        None,
        &mut reader,
        &mut writer,
    )
    .await
    {
        Ok(v2) => {
            log::info!("Successfully established v2 encrypted inbound connection");
            Ok((Transport::v2(v2), reader, writer))
        }
        Err(e) => {
            log::debug!("V2 handshake failed for inbound connection: {e:?}");

            match policy {
                TransportPolicy::V2Required => {
                    log::error!(
                        "V2 transport required but could not be established for inbound connection"
                    );
                    Err(ConnectionError::V2TransportRequired)
                }
                TransportPolicy::V2Preferred => {
                    // For inbound connections, if v2 fails we can try v1 with the existing stream.
                    log::info!("Using v1 plaintext inbound connection");
                    Ok((Transport::v1(network.magic()), reader, writer))
                }
            }
        }
    }
}

/// Establish a TCP connection to a bitcoin peer and perform the handshake.
///
/// 1. TCP connection establishment with timeout.
/// 2. TCP socket configuration (nodelay).
/// 3. Transport protocol negotiation (tries v2, falls back to v1).
/// 4. Version handshake.
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
    let (transport, reader, writer) = negotiate_outbound_transport(
        socket_addr,
        network,
        &peer,
        configuration.transport_policy,
        configuration.connection_timeout,
    )
    .await?;

    let mut connection = AsyncConnection::new(peer, configuration, transport, reader, writer);
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
    let (transport, reader, writer) =
        negotiate_inbound_transport(network, reader, writer, configuration.transport_policy)
            .await?;
    let mut connection = AsyncConnection::new(peer, configuration, transport, reader, writer);

    // Perform handshake (peer will send version first, we respond).
    super::handshake::perform_handshake(&mut connection).await?;

    Ok(connection)
}
