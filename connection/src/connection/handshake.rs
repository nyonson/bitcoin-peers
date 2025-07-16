//! Bitcoin p2p protocol version handshake implementation.
//!
//! This module provides utility functions for the bitcoin handshake protocol.
//! The async handshake implementation is in the futures module.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use bitcoin::p2p::address::AddrV2;
use log::debug;

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
pub enum HandshakeState {
    /// Sent version message, but haven't received anything yet.
    VersionSent,
    /// Received the peer's version message (and sent verack), but no verack response yet.
    VersionReceived,
    /// Received a verack, but no version message yet.
    VerackReceived,
    /// Both version and verack received - handshake complete.
    Complete,
}

/// Handles received verack message.
pub fn handle_verack_message(state: HandshakeState) -> HandshakeState {
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

/// Converts AddrV2 to SocketAddr for version message compatibility.
pub fn address_to_socket(addr: &AddrV2, port: u16) -> SocketAddr {
    match addr {
        AddrV2::Ipv4(ipv4) => SocketAddr::new(IpAddr::V4(*ipv4), port),
        AddrV2::Ipv6(ipv6) => SocketAddr::new(IpAddr::V6(*ipv6), port),
        _ => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
    }
}
