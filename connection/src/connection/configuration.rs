//! Connection configuration types and constants.

use crate::peer::PeerProtocolVersion;
use crate::transport::TransportPolicy;
use bitcoin::p2p::address::AddrV2;
use bitcoin::p2p::ServiceFlags;
use std::net::Ipv4Addr;

/// User agent string sent in version messages.
///
/// This identifies crawler software to other peers on the network.
/// Format follows Bitcoin Core's convention: "/$NAME:$VERSION/".
pub const BITCOIN_PEERS_USER_AGENT: &str =
    concat!("/bitcoin-peers:", env!("CARGO_PKG_VERSION"), "/");

/// Non-listening address used in version messages.
///
/// This address signals to peers that we are not accepting incoming connections
/// and should not be advertised to other nodes.
pub const NON_LISTENING_ADDRESS: AddrV2 = AddrV2::Ipv4(Ipv4Addr::new(0, 0, 0, 0));
pub const NON_LISTENING_PORT: u16 = 0;

/// Configuration used to build a connection.
#[derive(Debug, Clone)]
pub struct ConnectionConfiguration {
    /// Local minimum supported protocol version.
    pub protocol_version: PeerProtocolVersion,
    /// Custom user agent advertised for connection. Default is `/bitcoin-peers:$VERSION/`.
    pub user_agent: Option<String>,
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
    /// * `user_agent` - Optional custom user agent string. Defaults to bitcoin-peers default if None.
    ///
    /// # Returns
    ///
    /// A new ConnectionConfiguration configured for a non-listening node.
    pub fn non_listening(
        protocol_version: PeerProtocolVersion,
        user_agent: Option<String>,
    ) -> Self {
        Self {
            protocol_version,
            user_agent,
            services: ServiceFlags::NONE,
            sender_address: NON_LISTENING_ADDRESS,
            sender_port: NON_LISTENING_PORT,
            start_height: 0,
            relay: false,
            transport_policy: TransportPolicy::V2Required,
        }
    }
}
