//! Bitcoin p2p protocol connection.
//!
//! This module provides connection handling for the bitcoin peer-to-peer network.
//! It covers the bitcoin p2p protocol, including version handshake, message
//! serialization/deserialization, and feature negotiation.
//!
//! The [`futures`] module provides the primary async API for applications.

mod configuration;
mod error;
pub mod futures;
mod handshake;
mod state;
mod tcp;

pub use configuration::{ConnectionConfiguration, FeaturePreferences, TransportPolicy};
pub use error::ConnectionError;
pub use state::{AddrV2State, ConnectionState, SendHeadersState, WtxidRelayState};
