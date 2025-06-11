//! Connection configuration types and constants.

use crate::peer::PeerProtocolVersion;
use crate::user_agent::UserAgent;
use bitcoin::p2p::address::AddrV2;
use bitcoin::p2p::ServiceFlags;
use std::fmt;
use std::net::Ipv4Addr;
use std::time::Duration;

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

/// Default timeout for connection establishment.
pub const DEFAULT_CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

/// Policy for transport protocol selection during connection establishment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPolicy {
    /// V2 encrypted transport is required. Connection will fail if v2 cannot be established.
    V2Required,
    /// V2 encrypted transport is preferred, but will fall back to v1 if necessary.
    V2Preferred,
}

impl fmt::Display for TransportPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportPolicy::V2Required => write!(f, "V2Required"),
            TransportPolicy::V2Preferred => write!(f, "V2Preferred"),
        }
    }
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

impl fmt::Display for FeaturePreferences {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let features: Vec<&str> = [
            if self.enable_addrv2 {
                Some("AddrV2")
            } else {
                None
            },
            if self.enable_sendheaders {
                Some("SendHeaders")
            } else {
                None
            },
            if self.enable_wtxidrelay {
                Some("WtxidRelay")
            } else {
                None
            },
        ]
        .iter()
        .filter_map(|&opt| opt)
        .collect();

        if features.is_empty() {
            write!(f, "none")
        } else {
            write!(f, "{}", features.join(", "))
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
    /// Timeout for connection establishment.
    pub connection_timeout: Duration,
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
            connection_timeout: DEFAULT_CONNECTION_TIMEOUT,
        }
    }

    /// Set the timeout for connection establishment.
    ///
    /// This timeout applies to the initial connection attempt.
    /// The default is 10 seconds, which provides a good balance between
    /// allowing enough time for high-latency networks while failing
    /// quickly on unreachable peers.
    ///
    /// # Arguments
    ///
    /// * `timeout` - The maximum time to wait for connection establishment.
    ///
    /// # Returns
    ///
    /// Self for method chaining.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::time::Duration;
    /// # use bitcoin_peers_connection::{ConnectionConfiguration, TransportPolicy, FeaturePreferences};
    /// # use bitcoin_peers_connection::PeerProtocolVersion;
    ///
    /// let config = ConnectionConfiguration::non_listening(
    ///     PeerProtocolVersion::Known(70016),
    ///     TransportPolicy::V2Preferred,
    ///     FeaturePreferences::default(),
    ///     None
    /// ).with_connection_timeout(Duration::from_secs(30));
    /// ```
    pub fn with_connection_timeout(mut self, timeout: Duration) -> Self {
        self.connection_timeout = timeout;
        self
    }
}

impl fmt::Display for ConnectionConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let user_agent = match &self.user_agent {
            Some(ua) => ua.as_str(),
            None => "none",
        };

        write!(
            f,
            "ConnectionConfiguration {{ protocol: {}, user_agent: \"{}\", services: {}, transport: {}, features: [{}], relay: {} }}",
            self.protocol_version,
            user_agent,
            self.services,
            self.transport_policy,
            self.feature_preferences,
            self.relay
        )
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

    #[test]
    fn test_connection_configuration_timeout() {
        // Test default timeout
        let config = ConnectionConfiguration::non_listening(
            PeerProtocolVersion::Known(70016),
            TransportPolicy::V2Preferred,
            FeaturePreferences::default(),
            None,
        );
        assert_eq!(config.connection_timeout, DEFAULT_CONNECTION_TIMEOUT);
        assert_eq!(config.connection_timeout, Duration::from_secs(10));

        // Test custom timeout
        let custom_timeout = Duration::from_secs(30);
        let config_with_timeout = config.with_connection_timeout(custom_timeout);
        assert_eq!(config_with_timeout.connection_timeout, custom_timeout);
    }
}
