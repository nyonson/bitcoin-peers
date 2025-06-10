//! Bitcoin p2p protocol version handshake implementation.

use super::configuration::BITCOIN_PEERS_USER_AGENT;
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
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncWrite};

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
pub fn unix_timestamp() -> i64 {
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
pub fn generate_nonce() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_nanos() as u64;

    // Mix in the process ID for additional entropy.
    let pid = process::id() as u64;

    // Combine the values with bitwise operations.
    now ^ (pid.rotate_left(32))
}

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
/// 1. Send local version message.
/// 2. Receive and validate peer's version.
/// 3. Exchange verack messages.
/// 4. Negotiate protocol features (AddrV2, SendHeaders, WtxidRelay).
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
    let version_message = create_version_message(connection, nonce).await;
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
                debug!("Received unexpected message during handshake: {message:?}, ignoring");
            }
        }
    }

    debug!("Handshake completed successfully");
    Ok(())
}

/// Creates a version message for the handshake.
async fn create_version_message<R, W>(
    connection: &AsyncConnection<R, W>,
    nonce: u64,
) -> NetworkMessage
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let config = &connection.configuration;
    let peer_lock = connection.peer.lock().await;

    let receiver_services = match peer_lock.services {
        PeerServices::Known(flags) => flags,
        PeerServices::Unknown => ServiceFlags::NONE,
    };

    let receiver_socket_addr = address_to_socket(&peer_lock.address, peer_lock.port);
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
    // Check for connection loop.
    if version.nonce == our_nonce {
        error!("Connection loop detected - received same nonce");
        return Err(ConnectionError::ConnectionLoop);
    }

    match state {
        HandshakeState::VersionSent | HandshakeState::VerackReceived => {
            debug!("Received version message from peer");

            // Update peer information.
            {
                let mut peer = connection.peer.lock().await;
                peer.services = PeerServices::Known(version.services);
                peer.version = PeerProtocolVersion::Known(version.version);
            }

            // Calculate effective protocol version.
            let local_version = connection
                .configuration
                .protocol_version
                .unwrap_or(MIN_PROTOCOL_VERSION);
            let effective_version = std::cmp::min(local_version, version.version);

            // Update connection state.
            {
                let mut state = connection.state.lock().await;
                state.effective_protocol_version = PeerProtocolVersion::Known(effective_version);
            }

            // Send protocol negotiation messages.
            send_negotiation_messages(connection, effective_version).await?;

            // Send verack.
            connection.send(NetworkMessage::Verack).await?;
            debug!("Sent verack message to peer");

            Ok(if state == HandshakeState::VerackReceived {
                HandshakeState::Complete
            } else {
                HandshakeState::VersionReceived
            })
        }
        _ => {
            debug!("Received duplicate version message in state {state:?}, ignoring");
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
            debug!("Received duplicate verack message in state {state:?}, ignoring");
            state
        }
    }
}

/// Sends protocol feature negotiation messages based on effective version and configuration.
async fn send_negotiation_messages<R, W>(
    connection: &mut AsyncConnection<R, W>,
    effective_version: u32,
) -> Result<(), ConnectionError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    // Copy feature preferences to avoid borrow checker issues
    let features = connection.configuration.features;

    // SendAddrV2 for address format support (BIP155).
    if features.enable_addrv2 && effective_version >= ADDRV2_MIN_PROTOCOL_VERSION {
        connection.send(NetworkMessage::SendAddrV2).await?;
        let mut state = connection.state.lock().await;
        state.addr_v2 = state.addr_v2.on_send();
        debug!("Sent SendAddrV2 message, addrv2_state: {:?}", state.addr_v2);
    }

    // SendHeaders for header announcements.
    if features.enable_sendheaders && effective_version >= SENDHEADERS_MIN_PROTOCOL_VERSION {
        connection.send(NetworkMessage::SendHeaders).await?;
        let mut state = connection.state.lock().await;
        state.send_headers = state.send_headers.on_send();
        debug!(
            "Sent SendHeaders message, send_headers_state: {:?}",
            state.send_headers
        );
    }

    // WtxidRelay for witness transaction ID relay.
    if features.enable_wtxidrelay && effective_version >= WTXID_RELAY_MIN_PROTOCOL_VERSION {
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
