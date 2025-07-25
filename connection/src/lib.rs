//! Bitcoin peer connection and transport handling.
//!
//! This crate provides a robust implementation of the bitcoin peer-to-peer network protocol,
//! supporting both the legacy v1 transport and the modern encrypted v2 transport (BIP-324).
//! It handles the complete connection lifecycle including handshake, feature negotiation,
//! and message serialization,
//!
//! # Features
//!
//! * **Dual Transport Support**: Seamlessly handles both v1 (legacy) and v2 (BIP-324 encrypted) transports.
//! * **Feature Negotiation**: Automatic negotiation of protocol features like compact blocks and address relay.
//! * **Split Architecture**: Connections can be split into separate sender/receiver halves for concurrent use.
//!
//! # Quick Start
//!
//! ```
//! use bitcoin::Network;
//! use bitcoin_peers_connection::{
//!     futures::Connection, ConnectionConfiguration, Peer, PeerProtocolVersion,
//!     TransportPolicy, FeaturePreferences
//! };
//! use bitcoin::p2p::address::AddrV2;
//! use bitcoin::p2p::message::NetworkMessage;
//! use std::net::Ipv4Addr;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Create a peer address.
//! let peer = Peer::new(
//!     AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)),
//!     8333,
//! );
//!
//! // Configure connection for a non-listening client.
//! let config = ConnectionConfiguration::non_listening(
//!     PeerProtocolVersion::Known(70016),  // Local protocol version
//!     TransportPolicy::V2Preferred,       // Try v2, fallback to v1
//!     FeaturePreferences::default(),      // Default features
//!     None,                               // No custom user agent
//! );
//!
//! // Establish connection with automatic handshake.
//! let mut connection = Connection::connect(peer, Network::Bitcoin, config).await?;
//!
//! // Send and receive messages.
//! connection.write(NetworkMessage::GetAddr).await?;
//! let message = connection.read().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Architecture
//!
//! The crate is organized into several key modules:
//!
//! * **Connection Layer**: High-level API for establishing and managing connections.
//! * **Transport Layer**: Lower level message serialization and encryption (v1 and v2).
//!
//! # Connection
//!
//! * [`futures::Connection`] - The main async connection type that handles both sending and receiving.
//! * [`futures::ConnectionWriter`] / [`futures::ConnectionReader`] - Split connection for concurrent operations.
//!
//! ## Split Connection for Concurrent Operations
//!
//! ```
//! # use bitcoin::Network;
//! # use bitcoin_peers_connection::{futures::Connection, ConnectionConfiguration, Peer, PeerProtocolVersion, TransportPolicy, FeaturePreferences};
//! # use bitcoin::p2p::message::NetworkMessage;
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! # let peer = Peer::new(bitcoin::p2p::address::AddrV2::Ipv4(std::net::Ipv4Addr::new(127, 0, 0, 1)), 8333);
//! # let config = ConnectionConfiguration::non_listening(
//! #     PeerProtocolVersion::Known(70016),
//! #     TransportPolicy::V2Preferred,
//! #     FeaturePreferences::default(),
//! #     None,
//! # );
//! let connection = Connection::connect(peer, Network::Bitcoin, config).await?;
//! let (mut receiver, mut sender) = connection.into_split();
//!
//! // Can now send and receive concurrently from different tasks.
//! tokio::spawn(async move {
//!     sender.write(NetworkMessage::Ping(42)).await.unwrap();
//! });
//!
//! let message = receiver.read().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Accepting Inbound Connections
//!
//! ```
//! # use bitcoin::Network;
//! # use bitcoin_peers_connection::{futures::Connection, ConnectionConfiguration, PeerProtocolVersion, TransportPolicy, FeaturePreferences, UserAgent};
//! # use tokio::net::TcpListener;
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Configure for a listening node with custom user agent.
//! let config = ConnectionConfiguration::non_listening(
//!     PeerProtocolVersion::Known(70016),
//!     TransportPolicy::V2Preferred,
//!     FeaturePreferences::default(),
//!     Some(UserAgent::from_name_version("MyNode", "0.1.0")),
//! );
//!
//! // Accept incoming connections.
//! let listener = TcpListener::bind("0.0.0.0:8333").await?;
//! let (stream, _addr) = listener.accept().await?;
//! let connection = Connection::accept(stream, Network::Bitcoin, config).await?;
//! # Ok(())
//! # }
//! ````
//!
//! # Transport
//!
//! * [`transport::Transport`] - Lower-level transport abstraction over both v1 and v2.

pub mod connection;
mod peer;
pub mod transport;
mod user_agent;

pub use connection::{
    futures, ConnectionConfiguration, ConnectionError, FeaturePreferences, TransportPolicy,
};
pub use peer::{Peer, PeerProtocolVersion, PeerServices};
pub use user_agent::{UserAgent, UserAgentError};
