//! Bitcoin p2p protocol version handshake implementation.

use super::configuration::BITCOIN_PEERS_USER_AGENT;
use super::state::{generate_nonce, unix_timestamp};
use super::{AsyncConnection, ConnectionError};
use crate::peer::{
    PeerProtocolVersion, PeerServices, ADDRV2_MIN_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION,
    SENDHEADERS_MIN_PROTOCOL_VERSION, WTXID_RELAY_MIN_PROTOCOL_VERSION,
};
use bitcoin::p2p::address::{AddrV2, Address};
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::p2p::ServiceFlags;
use log::{debug, error};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use tokio::io::{AsyncRead, AsyncWrite};

/// State machine for tracking handshake progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandshakeState {
    /// Sent version message, but haven't received anything yet.
    VersionSent,
    /// Received the peer's version message (and sent verack), but no verack response yet.
    VersionReceived,
    /// Received a verack, but no version message yet.
    VerackReceived,
    /// Both version and verack received - handshake complete.
    Complete,
}

/// Performs the bitcoin p2p version handshake protocol.
///
/// This includes:
/// 1. Sending our version message
/// 2. Receiving and validating peer's version
/// 3. Exchanging verack messages
/// 4. Negotiating protocol features (AddrV2, SendHeaders, WtxidRelay)
///
/// # Arguments
///
/// * `connection` - The connection to perform handshake on.
///
/// # Returns
///
/// Ok(()) if handshake completed successfully, otherwise an error.
pub async fn perform_handshake<R, W>(
    connection: &mut AsyncConnection<R, W>,
) -> Result<(), ConnectionError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    // Generate nonce for connection loop detection
    let nonce = generate_nonce();

    // Send our version message
    let version_message = create_version_message(connection, nonce);
    connection.send(version_message).await?;
    debug!("Sent version message to peer");

    let mut state = HandshakeState::VersionSent;

    // Process messages until handshake completes
    while state != HandshakeState::Complete {
        let message = connection.receive().await?;

        match message {
            NetworkMessage::Version(version) => {
                state = handle_version_message(connection, version, nonce, state).await?;
            }
            NetworkMessage::Verack => {
                state = handle_verack_message(state);
            }
            _ => {
                debug!(
                    "Received unexpected message during handshake: {:?}, ignoring",
                    message
                );
            }
        }
    }

    debug!("Handshake completed successfully");
    Ok(())
}

/// Creates a version message for the handshake.
fn create_version_message<R, W>(connection: &AsyncConnection<R, W>, nonce: u64) -> NetworkMessage
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let config = &connection.configuration;
    let peer = &connection.peer;

    // Use known services or NONE
    let receiver_services = match peer.services {
        PeerServices::Known(flags) => flags,
        PeerServices::Unknown => ServiceFlags::NONE,
    };

    // Convert addresses for version message compatibility
    let receiver_socket_addr = address_to_socket(&peer.address, peer.port);

    let sender_socket_addr = address_to_socket(&config.sender_address, config.sender_port);

    let user_agent = config
        .user_agent
        .as_deref()
        .unwrap_or(BITCOIN_PEERS_USER_AGENT);

    let version = VersionMessage {
        version: config.protocol_version.unwrap_or(MIN_PROTOCOL_VERSION),
        services: config.services,
        timestamp: unix_timestamp(),
        receiver: Address::new(&receiver_socket_addr, receiver_services),
        sender: Address::new(&sender_socket_addr, config.services),
        nonce,
        user_agent: user_agent.to_string(),
        start_height: config.start_height,
        relay: config.relay,
    };

    NetworkMessage::Version(version)
}

/// Handles received version message.
async fn handle_version_message<R, W>(
    connection: &mut AsyncConnection<R, W>,
    version: VersionMessage,
    our_nonce: u64,
    state: HandshakeState,
) -> Result<HandshakeState, ConnectionError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    // Check for connection loop
    if version.nonce == our_nonce {
        error!("Connection loop detected - received same nonce");
        return Err(ConnectionError::ConnectionLoop);
    }

    match state {
        HandshakeState::VersionSent | HandshakeState::VerackReceived => {
            debug!("Received version message from peer");

            // Update peer information
            connection.peer.services = PeerServices::Known(version.services);
            connection.peer.version = PeerProtocolVersion::Known(version.version);

            // Calculate effective protocol version
            let our_version = connection
                .configuration
                .protocol_version
                .unwrap_or(MIN_PROTOCOL_VERSION);
            let effective_version = std::cmp::min(our_version, version.version);

            // Update connection state
            {
                let mut state = connection.state.lock().await;
                state.effective_protocol_version = PeerProtocolVersion::Known(effective_version);
            }

            // Send protocol negotiation messages
            send_negotiation_messages(connection, effective_version).await?;

            // Send verack
            connection.send(NetworkMessage::Verack).await?;
            debug!("Sent verack message to peer");

            Ok(if state == HandshakeState::VerackReceived {
                HandshakeState::Complete
            } else {
                HandshakeState::VersionReceived
            })
        }
        _ => {
            debug!(
                "Received duplicate version message in state {:?}, ignoring",
                state
            );
            Ok(state)
        }
    }
}

/// Handles received verack message.
fn handle_verack_message(state: HandshakeState) -> HandshakeState {
    match state {
        HandshakeState::VersionSent | HandshakeState::VersionReceived => {
            debug!("Received verack from peer");
            if state == HandshakeState::VersionReceived {
                HandshakeState::Complete
            } else {
                HandshakeState::VerackReceived
            }
        }
        _ => {
            debug!(
                "Received duplicate verack message in state {:?}, ignoring",
                state
            );
            state
        }
    }
}

/// Sends protocol feature negotiation messages based on effective version.
async fn send_negotiation_messages<R, W>(
    connection: &mut AsyncConnection<R, W>,
    effective_version: u32,
) -> Result<(), ConnectionError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    // SendAddrV2 for address format support (BIP155)
    if effective_version >= ADDRV2_MIN_PROTOCOL_VERSION {
        connection.send(NetworkMessage::SendAddrV2).await?;
        let mut state = connection.state.lock().await;
        state.addr_v2 = state.addr_v2.on_send();
        debug!("Sent SendAddrV2 message, addrv2_state: {:?}", state.addr_v2);
    }

    // SendHeaders for header announcements
    if effective_version >= SENDHEADERS_MIN_PROTOCOL_VERSION {
        connection.send(NetworkMessage::SendHeaders).await?;
        let mut state = connection.state.lock().await;
        state.send_headers = state.send_headers.on_send();
        debug!(
            "Sent SendHeaders message, send_headers_state: {:?}",
            state.send_headers
        );
    }

    // WtxidRelay for witness transaction ID relay
    if effective_version >= WTXID_RELAY_MIN_PROTOCOL_VERSION {
        connection.send(NetworkMessage::WtxidRelay).await?;
        let mut state = connection.state.lock().await;
        state.wtxid_relay = state.wtxid_relay.on_send();
        debug!(
            "Sent WtxidRelay message, wtxid_relay_state: {:?}",
            state.wtxid_relay
        );
    }

    Ok(())
}

/// Converts AddrV2 to SocketAddr for version message compatibility.
fn address_to_socket(addr: &AddrV2, port: u16) -> SocketAddr {
    match addr {
        AddrV2::Ipv4(ipv4) => SocketAddr::new(IpAddr::V4(*ipv4), port),
        AddrV2::Ipv6(ipv6) => SocketAddr::new(IpAddr::V6(*ipv6), port),
        _ => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
    }
}
