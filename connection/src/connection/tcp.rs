//! TCP-specific connections.

use super::configuration::TransportPolicy;
use super::{
    AsyncConnection, AsyncConnectionReceiver, AsyncConnectionSender, ConnectionConfiguration,
    ConnectionError,
};
use crate::peer::{Peer, PeerServices};
use crate::transport::Transport;
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

/// Helper function to establish TCP connection with timeout and nodelay.
async fn establish_tcp_connection(socket_addr: SocketAddr) -> Result<TcpStream, ConnectionError> {
    match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(socket_addr)).await {
        Ok(Ok(stream)) => {
            // No delay is helpful for the small packets of the bitcoin p2p protocol.
            stream.set_nodelay(true)?;
            Ok(stream)
        }
        Ok(Err(e)) => Err(ConnectionError::Io(e)),
        Err(_) => Err(ConnectionError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "Connection attempt timed out",
        ))),
    }
}

/// Negotiate transport protocol for TCP connection.
async fn negotiate_tcp_transport(
    socket_addr: SocketAddr,
    network: Network,
    peer: &Peer,
    policy: TransportPolicy,
) -> Result<(Transport, OwnedReadHalf, OwnedWriteHalf), ConnectionError> {
    let peer_supports_v2 = match peer.services {
        PeerServices::Unknown => true,
        PeerServices::Known(flags) => flags.has(ServiceFlags::P2P_V2),
    };

    // Only skip v2 attempt if two criteria are met.
    //
    // 1. Connection configuration policy allows for v1 fallback (V2Preferred).
    // 2. Peer explicitly doesn't advertise v2 support.
    if !peer_supports_v2 && matches!(policy, TransportPolicy::V2Preferred) {
        let stream = establish_tcp_connection(socket_addr).await?;
        let (reader, writer) = stream.into_split();
        log::info!(
            "Using v1 plaintext connection to {:?} (no P2P_V2 flag)",
            peer.address
        );
        return Ok((Transport::v1(network.magic()), reader, writer));
    }

    let stream = establish_tcp_connection(socket_addr).await?;
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
                    let stream = establish_tcp_connection(socket_addr).await?;
                    let (reader, writer) = stream.into_split();

                    log::info!("Using v1 plaintext connection to {:?}", peer.address);
                    Ok((Transport::v1(network.magic()), reader, writer))
                }
            }
        }
    }
}

/// Establish a TCP connection to a bitcoin peer and perform the handshake.
///
/// This function handles:
/// 1. TCP connection establishment with timeout
/// 2. TCP socket configuration (nodelay)
/// 3. Transport protocol negotiation (tries v2, falls back to v1)
/// 4. Version handshake
///
/// # Arguments
///
/// * `peer` - The bitcoin peer to connect to
/// * `network` - The bitcoin network to use
/// * `configuration` - Configuration for the connection
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

    // Negotiate transport based on peer capabilities and configuration policy
    let (transport, reader, writer) =
        negotiate_tcp_transport(socket_addr, network, &peer, configuration.transport_policy)
            .await?;

    let mut connection = AsyncConnection::new(peer, configuration, transport, reader, writer);
    super::handshake::perform_handshake(&mut connection).await?;

    Ok(connection)
}
