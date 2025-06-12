//! Bitcoin network peer discovery.
//!
//! This crate provides a high-performance, concurrent crawler for discovering peers on the bitcoin network.
//!
//! # Features
//!
//! * **Concurrent Crawling**: Efficiently crawls multiple peers simultaneously.
//! * **Streaming Results**: Returns discovered peers as they're found via an async channel interface.
//! * **Resource Control**: Configurable limits on concurrent connections and timeouts.
//!
//! # Quick Start
//!
//! ```
//! use bitcoin::Network;
//! use bitcoin::p2p::address::AddrV2;
//! use bitcoin_peers_crawler::{Crawler, CrawlerBuilder, Peer};
//! use tokio::time::{timeout, Duration};
//! use std::net::Ipv4Addr;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Build a crawler with custom configuration.
//! let crawler = CrawlerBuilder::new(Network::Bitcoin)
//!     .with_max_concurrent_tasks(16)  // Increase concurrency
//!     .build();
//!
//! // Create a seed peer to start crawling from.
//! let seed = Peer::new(
//!     AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)),
//!     8333,
//! );
//!
//! // Start crawling from the seed peer.
//! let mut receiver = crawler.crawl(seed).await?;
//!
//! // Collect peers for 30 seconds.
//! let mut discovered_peers = Vec::new();
//! let duration = Duration::from_secs(30);
//!
//! if let Ok(result) = timeout(duration, async {
//!     while let Some(msg) = receiver.recv().await {
//!         match msg {
//!             bitcoin_peers_crawler::CrawlerMessage::Listening(peer) => {
//!                 discovered_peers.push(peer);
//!             }
//!             _ => {} // Ignore non-listening peers
//!         }
//!     }
//! }).await {
//!     result;
//! }
//!
//! println!("Discovered {} peers", discovered_peers.len());
//! # Ok(())
//! # }
//! ```
//!
//! # Configuration
//!
//! The [`CrawlerBuilder`] provides fine-grained control over crawler behavior:
//!
//! ```no_run
//! use bitcoin::Network;
//! use bitcoin_peers_crawler::{CrawlerBuilder, TransportPolicy};
//!
//! let crawler = CrawlerBuilder::new(Network::Bitcoin)
//!     .with_max_concurrent_tasks(32)     // Aggressive concurrency
//!     .with_transport_policy(TransportPolicy::V2Required)  // Require encrypted connections
//!     .with_protocol_version(70016)      // Minimum protocol version
//!     .build();
//! ```
//!
//! # Example: Finding V2-Capable Peers
//!
//! ```no_run
//! use bitcoin::Network;
//! use bitcoin::p2p::address::AddrV2;
//! use bitcoin::p2p::ServiceFlags;
//! use bitcoin_peers_crawler::{CrawlerBuilder, CrawlerMessage, Peer, PeerServices};
//! use std::net::Ipv4Addr;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let crawler = CrawlerBuilder::new(Network::Bitcoin).build();
//!
//! // Start from a known peer
//! let seed = Peer::new(
//!     AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)),
//!     8333,
//! );
//! let mut receiver = crawler.crawl(seed).await?;
//!
//! let mut v2_peers = Vec::new();
//! while let Some(msg) = receiver.recv().await {
//!     if let CrawlerMessage::Listening(peer) = msg {
//!         // Check if peer advertises v2 transport support.
//!         if let PeerServices::Known(flags) = peer.services {
//!             if flags.has(ServiceFlags::P2P_V2) {
//!                 v2_peers.push(peer);
//!             }
//!         }
//!         
//!         if v2_peers.len() >= 100 {
//!             break;  // Found enough v2-capable peers
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```

mod builder;
mod crawler;

pub use builder::{CrawlerBuilder, CrawlerBuilderError};
pub use crawler::{Crawler, CrawlerMessage};

// Re-exports.
pub use bitcoin_peers_connection::{
    ConnectionError, Peer, PeerProtocolVersion, PeerServices, TransportPolicy, UserAgent,
};
