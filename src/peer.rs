//! Bitcoin peer information structures and utilities.

use bitcoin::p2p::address::AddrV2;
use bitcoin::p2p::ServiceFlags;
use std::fmt;

/// Minimum protocol version for basic compatibility with modern bitcoin nodes.
pub const MIN_PROTOCOL_VERSION: u32 = 70001;
/// Minimum protocol version that supports AddrV2 messages (BIP155).
///
/// Bitcoin Core implemented this in version 0.21.0 with protocol version 70016.
pub const ADDRV2_MIN_PROTOCOL_VERSION: u32 = 70016;
/// Minimum protocol version that supports SendHeaders.
///
/// Bitcoin Core implemented this in version 0.12.0 with protocol version 70012.
pub const SENDHEADERS_MIN_PROTOCOL_VERSION: u32 = 70012;
/// Minimum protocol version that supports WtxidRelay.
///
/// Bitcoin Core implemented this in version 0.21.0 with protocol version 70016.
pub const WTXID_RELAY_MIN_PROTOCOL_VERSION: u32 = 70016;

/// Represents the service state of a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerServices {
    /// Known services with specific ServiceFlags.
    Known(ServiceFlags),
    /// Unknown services state.
    Unknown,
}

/// Represents the protocol version of a peer.
///
/// * **70001** - BIP 0031, absolute minimum for modern nodes.
/// * **70002** - BIP 0035, added mempool message.
/// * **70012** - BIP 0065, added CheckLockTimeVerify.
/// * **70013** - BIP 0130/BIP 0133, added sendheaders and feefilter.
/// * **70014** - BIP 0152, added compact blocks.
/// * **70015** - BIP 0141/BIP 0143/BIP 0147, SegWit support.
/// * **70016** - BIP 157/158, coompact block filters.
///
/// Nodes running Bitcoin Core 0.10.0 and later typically reject connections from peers
/// with protocol versions below 70001. If you use a lower version, you'll likely be
/// unable to connect to most of the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerProtocolVersion {
    /// Known protocol version.
    Known(u32),
    /// Unknown protocol version.
    Unknown,
}

impl PeerProtocolVersion {
    /// Returns the protocol version value if known, or a default value if unknown.
    ///
    /// # Arguments
    ///
    /// * `default` - The default value to return if the version is unknown.
    ///
    /// # Returns
    ///
    /// The version number if known, or the provided default value.
    pub fn unwrap_or(self, default: u32) -> u32 {
        match self {
            PeerProtocolVersion::Known(v) => v,
            PeerProtocolVersion::Unknown => default,
        }
    }
}

/// Represents a bitcoin peer on the network.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Peer {
    /// The peer's network address.
    pub address: AddrV2,
    /// The port number the peer is listening on.
    pub port: u16,
    /// The service flags advertised by the peer.
    pub services: PeerServices,
    /// The protocol version of the peer.
    pub version: PeerProtocolVersion,
}

impl Peer {
    /// Create a new peer with unknown services and version.
    pub fn new(address: AddrV2, port: u16) -> Self {
        Peer {
            address,
            port,
            services: PeerServices::Unknown,
            version: PeerProtocolVersion::Unknown,
        }
    }

    /// Checks if the peer advertises the specified service.
    ///
    /// # Arguments
    ///
    /// * `service` - The service flag to check for.
    ///
    /// # Returns
    ///
    /// `true` if the peer advertises the service, `false` otherwise.
    pub fn has_service(&self, service: ServiceFlags) -> bool {
        match self.services {
            PeerServices::Known(flags) => flags.has(service),
            PeerServices::Unknown => false,
        }
    }

    /// Returns a new Peer with known services.
    pub fn with_known_services(&self, services: ServiceFlags) -> Self {
        Peer {
            address: self.address.clone(),
            port: self.port,
            services: PeerServices::Known(services),
            version: self.version,
        }
    }

    /// Returns a new Peer with known version.
    pub fn with_known_version(&self, version: u32) -> Self {
        Peer {
            address: self.address.clone(),
            port: self.port,
            services: self.services,
            version: PeerProtocolVersion::Known(version),
        }
    }
}

impl fmt::Display for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?}:{} ([peer] services: {}, version: {})",
            self.address,
            self.port,
            match self.services {
                PeerServices::Known(flags) => flags.to_string(),
                PeerServices::Unknown => "unknown".to_string(),
            },
            match self.version {
                PeerProtocolVersion::Known(v) => v.to_string(),
                PeerProtocolVersion::Unknown => "unknown".to_string(),
            }
        )
    }
}
