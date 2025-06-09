//! Connection state management and protocol negotiation tracking.

use crate::peer::PeerProtocolVersion;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

/// State of AddrV2 support negotiation for the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrV2State {
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
    pub fn on_send(&self) -> Self {
        match self {
            AddrV2State::NotNegotiated => AddrV2State::SentOnly,
            AddrV2State::ReceivedOnly => AddrV2State::Enabled,
            _ => *self, // Already sent, state doesn't change
        }
    }

    /// Update the state when we receive a SendAddrV2 message.
    pub fn on_receive(&self) -> Self {
        match self {
            AddrV2State::NotNegotiated => AddrV2State::ReceivedOnly,
            AddrV2State::SentOnly => AddrV2State::Enabled,
            _ => *self, // Already received, state doesn't change
        }
    }
}

/// State of SendHeaders support negotiation for the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendHeadersState {
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
    pub fn on_send(&self) -> Self {
        match self {
            SendHeadersState::NotNegotiated => SendHeadersState::SentOnly,
            SendHeadersState::ReceivedOnly => SendHeadersState::Enabled,
            _ => *self, // Already sent, state doesn't change
        }
    }

    /// Update the state when we receive a SendHeaders message.
    pub fn on_receive(&self) -> Self {
        match self {
            SendHeadersState::NotNegotiated => SendHeadersState::ReceivedOnly,
            SendHeadersState::SentOnly => SendHeadersState::Enabled,
            _ => *self, // Already received, state doesn't change
        }
    }
}

/// State of WtxidRelay support negotiation for the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WtxidRelayState {
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
    pub fn on_send(&self) -> Self {
        match self {
            WtxidRelayState::NotNegotiated => WtxidRelayState::SentOnly,
            WtxidRelayState::ReceivedOnly => WtxidRelayState::Enabled,
            _ => *self, // Already sent, state doesn't change
        }
    }

    /// Update the state when we receive a WtxidRelay message.
    pub fn on_receive(&self) -> Self {
        match self {
            WtxidRelayState::NotNegotiated => WtxidRelayState::ReceivedOnly,
            WtxidRelayState::SentOnly => WtxidRelayState::Enabled,
            _ => *self, // Already received, state doesn't change
        }
    }
}

/// Runtime state of a connection.
#[derive(Debug)]
pub struct ConnectionState {
    /// The protocol version negotiated between peers (minimum of both versions).
    pub effective_protocol_version: PeerProtocolVersion,
    /// Current state of AddrV2 support negotiation.
    pub addr_v2: AddrV2State,
    /// Current state of SendHeaders support negotiation.
    pub send_headers: SendHeadersState,
    /// Current state of WtxidRelay support negotiation.
    pub wtxid_relay: WtxidRelayState,
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[connection state] protocol: {:?}, addrv2: {:?}, headers: {:?}, wtxid: {:?}",
            self.effective_protocol_version, self.addr_v2, self.send_headers, self.wtxid_relay
        )
    }
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
