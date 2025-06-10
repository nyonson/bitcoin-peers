//! Connection feature tracking.
//!
//! The connection state evolves during the lifetime of a connection.
//!
//! 1. **Initialized**: When a connection is established, all negotiation states start as
//!    `NotNegotiated` and the protocol version is `Unknown`.
//! 2. **Handshake**: During the version handshake, the protocol version is determined
//!    by taking the minimum of both peers' versions.
//! 3. **Feature Negotiation**: During the handshake, peers may exchange feature negotiation
//!    messages. But there is not hard defined ordering.
//! 4. **Steady State**: Once features are negotiated, the state usually remains stable and can be
//!    queried to determine which features are active.
//!
//! # Feature Negotiation
//!
//! The bitcoin p2p protocol uses a two-tier system to establish if a feature is enabled for a connection.
//!
//! 1. **Protocol Version**: A numeric version (e.g., 70016) that defines which messages
//!    and features a node implementation can understand. This prevents nodes from sending
//!    messages that would be completely unrecognized by older implementations.
//! 2. **Feature Negotiation**: Optional messages exchanged after the handshake to enable
//!    specific features. Even if both nodes support a feature (based on protocol version),
//!    they must explicitly opt-in through negotiation.
//!
//! Most features follow a symmetric negotiation pattern where a feature is enabled only
//! after both peers have sent the corresponding message (e.g. `SendAddrV2`). Although
//! some are one-directional.

use crate::peer::PeerProtocolVersion;

/// State of AddrV2 support negotiation for the connection.
///
/// Once enabled, peers SHOULD send `addrv2` messages instead of
/// legacy `addr` messages when sharing addresses. But peers
/// MUST still accept legacy `addr` messages even if enabled
/// for backwards compatability.
///
/// See [BIP-155](https://en.bitcoin.it/wiki/BIP_0155).
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
///
/// Indicates that a node prefers to receive new block announcements via a
/// `headers` message rather than an `inv`. A node SHOULD should announce
/// new blocks to a peer with a `headers` message if they receive
/// a `sendheaders` message.
///
/// See [BIP-130](https://en.bitcoin.it/wiki/BIP_0130).
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
///
/// With wtxidrelay, transactions are announced via `inv` using their
/// witeness transaction ID (wtxid) instead of their transaction hash (txid).
///
/// See [BIP-339](https://en.bitcoin.it/wiki/BIP_0339).
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
///
/// This struct tracks the negotiated capabilities and features of a bitcoin p2p connection.
#[derive(Debug, Clone)]
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

impl Default for ConnectionState {
    fn default() -> Self {
        Self::new()
    }
}
