//! Bitcoin peer information structures and utilities.

use bitcoin::p2p::address::AddrV2;
use bitcoin::p2p::ServiceFlags;
use std::fmt;

/// Represents the service state of a peer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PeerServices {
    /// Known services with specific ServiceFlags.
    Known(ServiceFlags),
    /// Unknown services state.
    Unknown,
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
}

impl Peer {
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
        match &self.services {
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
        }
    }
}

impl fmt::Display for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?}:{} (services: {})",
            self.address,
            self.port,
            match &self.services {
                PeerServices::Known(flags) => flags.to_string(),
                PeerServices::Unknown => "unknown".to_string(),
            }
        )
    }
}
