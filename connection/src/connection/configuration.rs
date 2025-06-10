//! Connection configuration types and constants.

use crate::peer::PeerProtocolVersion;
use crate::user_agent::UserAgent;
use bitcoin::p2p::address::AddrV2;
use bitcoin::p2p::ServiceFlags;
use std::net::Ipv4Addr;

/// Default user agent for bitcoin-peers connections.
///
/// This identifies bitcoin-peers software to other peers on the network.
/// Format follows Bitcoin Core's convention: "/$NAME:$VERSION/".
pub fn default_user_agent() -> UserAgent {
    UserAgent::from_name_version("bitcoin-peers", env!("CARGO_PKG_VERSION"))
}

/// Non-listening address used in version messages.
///
/// This address signals to peers that we are not accepting incoming connections
/// and should not be advertised to other nodes.
pub const NON_LISTENING_ADDRESS: AddrV2 = AddrV2::Ipv4(Ipv4Addr::new(0, 0, 0, 0));
pub const NON_LISTENING_PORT: u16 = 0;

/// Policy for transport protocol selection during connection establishment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPolicy {
    /// V2 encrypted transport is required. Connection will fail if v2 cannot be established.
    V2Required,
    /// V2 encrypted transport is preferred, but will fall back to v1 if necessary.
    V2Preferred,
}

/// Feature negotiation preferences for a connection.
#[derive(Debug, Clone, Copy)]
pub struct FeaturePreferences {
    /// Whether to negotiate AddrV2 support (BIP-155).
    pub enable_addrv2: bool,
    /// Whether to negotiate SendHeaders support (BIP-130).
    pub enable_sendheaders: bool,
    /// Whether to negotiate WtxidRelay support (BIP-339).
    pub enable_wtxidrelay: bool,
}

impl Default for FeaturePreferences {
    fn default() -> Self {
        Self {
            enable_addrv2: true,
            enable_sendheaders: true,
            enable_wtxidrelay: true,
        }
    }
}

/// Configuration used to build a connection.
#[derive(Debug, Clone)]
pub struct ConnectionConfiguration {
    /// Local minimum supported protocol version.
    pub protocol_version: PeerProtocolVersion,
    /// Custom user agent advertised for connection. Defaults to bitcoin-peers user agent if None.
    pub user_agent: Option<UserAgent>,
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
    /// Hopefully 0 doesn't initiate some IBD functionality.
    pub start_height: i32,
    /// Whether to relay transactions to this peer.
    pub relay: bool,
    /// Transport protocol selection policy.
    pub transport_policy: TransportPolicy,
    /// Feature negotiation preferences.
    pub feature_preferences: FeaturePreferences,
}

impl ConnectionConfiguration {
    /// Creates a new configuration for a non-listening node.
    ///
    /// This configuration advertises no services, uses a non-listening address,
    /// and doesn't relay transactions. It's suitable for crawlers and other
    /// light client software that just wants to query the network without accepting
    /// incoming connections.
    ///
    /// # Arguments
    ///
    /// * `protocol_version` - The protocol version to advertise. Defaults to MIN_PROTOCOL_VERSION if Unknown.
    /// * `transport_policy` - Should v2 failures fallback to unencrypted v1 transport.
    /// * `feature_preferences` - What features to attempt to enable on connection.
    /// * `user_agent` - Optional custom user agent. Defaults to bitcoin-peers user agent if None.
    ///
    /// # Returns
    ///
    /// A new ConnectionConfiguration configured for a non-listening node.
    pub fn non_listening(
        protocol_version: PeerProtocolVersion,
        transport_policy: TransportPolicy,
        feature_preferences: FeaturePreferences,
        user_agent: Option<UserAgent>,
    ) -> Self {
        Self {
            protocol_version,
            user_agent,
            services: ServiceFlags::NONE,
            sender_address: NON_LISTENING_ADDRESS,
            sender_port: NON_LISTENING_PORT,
            start_height: 0,
            relay: false,
            transport_policy,
            feature_preferences,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_preferences_default() {
        let prefs = FeaturePreferences::default();
        assert!(prefs.enable_addrv2);
        assert!(prefs.enable_sendheaders);
        assert!(prefs.enable_wtxidrelay);
    }
}
