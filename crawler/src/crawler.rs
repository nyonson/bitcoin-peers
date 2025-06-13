use crate::session::{CrawlSession, SessionConfig};
use bitcoin::Network;
use bitcoin_peers_connection::{
    ConnectionError, Peer, PeerProtocolVersion, TransportPolicy, UserAgent,
};
use std::fmt;
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver};

/// Messages sent from the [`Crawler`] to the caller about peer discovery.
#[derive(Debug, Clone)]
pub enum CrawlerMessage {
    /// A peer that has been verified as listening by establishing a connection.
    Listening(Peer),
    /// A peer that failed to connect, perhaps due to non-listening or offline.
    NonListening(Peer),
}

impl fmt::Display for CrawlerMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CrawlerMessage::Listening(peer) => write!(f, "Listening Peer: {peer}"),
            CrawlerMessage::NonListening(peer) => write!(f, "Non-listening Peer: {peer}"),
        }
    }
}

/// A crawler for the bitcoin peer-to-peer network.
///
/// This crawler connects to bitcoin peers, performs handshakes, and asks for more peers.
#[derive(Debug, Clone)]
pub struct Crawler {
    /// bitcoin network the [`Crawler`] operates on.
    network: Network,
    /// Custom user agent advertised for connection. Defaults to bitcoin-peers user agent if None.
    user_agent: Option<UserAgent>,
    /// Transport policy for connections.
    transport_policy: TransportPolicy,
    /// Protocol version to advertise in connections.
    protocol_version: PeerProtocolVersion,
    /// Maximum number of concurrent connection tasks.
    max_concurrent_tasks: usize,
    /// Timeout for peer operations.
    ///
    /// This timeout applies to all peer-related operations.
    ///
    /// * Connection establishment (TCP connect + handshake)
    /// * Requesting peer addresses after establishing a connection
    /// * Waiting for responses to protocol messages
    ///
    /// A longer timeout may help on slow networks, while a shorter timeout
    /// can speed up crawling when peers are unresponsive.
    peer_timeout: Duration,
}

impl Crawler {
    /// Create a new crawler with the specified configuration.
    ///
    /// This constructor allows direct creation of a crawler if you prefer not to use
    /// the [`CrawlerBuilder`]. For most use cases, [`CrawlerBuilder::new()`] provides
    /// a more convenient API with sensible defaults.
    ///
    /// # Arguments
    ///
    /// * `network` - The bitcoin network to crawl.
    /// * `user_agent` - Custom user agent for connections (None uses default).
    /// * `transport_policy` - V2 transport preference policy.
    /// * `protocol_version` - Bitcoin protocol version to advertise.
    /// * `max_concurrent_tasks` - Maximum number of concurrent connection tasks.
    /// * `peer_timeout` - Timeout for all peer operations.
    pub fn new(
        network: Network,
        user_agent: Option<UserAgent>,
        transport_policy: TransportPolicy,
        protocol_version: PeerProtocolVersion,
        max_concurrent_tasks: usize,
        peer_timeout: Duration,
    ) -> Self {
        Self {
            network,
            user_agent,
            transport_policy,
            protocol_version,
            max_concurrent_tasks,
            peer_timeout,
        }
    }

    /// Create session configuration from this crawler.
    fn session_config(&self) -> SessionConfig {
        SessionConfig {
            network: self.network,
            user_agent: self.user_agent.clone(),
            transport_policy: self.transport_policy,
            protocol_version: self.protocol_version,
            max_concurrent_tasks: self.max_concurrent_tasks,
            peer_timeout: self.peer_timeout,
        }
    }

    /// Crawl the bitcoin network starting from a seed peer.
    ///
    /// This method returns a channel that will receive peer messages as peers are verified.
    /// The channel will be closed when the crawling is complete or encounters an error.
    ///
    /// # Termination
    ///
    /// The crawler will terminate in two scenarios:
    ///
    /// * **Natural completion** - When all discovered peers have been tested and no more peers are found.
    /// * **Early termination** - When the returned receiver is dropped, the crawler will detect this and stop gracefully.
    ///
    /// # Arguments
    ///
    /// * `seed` - The seed peer to start crawling from.
    ///
    /// # Returns
    ///
    /// * `Ok(Receiver<PeerMessage>)` - A channel that will receive peer messages.
    /// * `Err(Error)` - If there was an error during crawling setup.
    pub async fn crawl(&self, seed: Peer) -> Result<Receiver<CrawlerMessage>, ConnectionError> {
        let (crawl_tx, crawl_rx) = mpsc::channel(1000);

        let session = CrawlSession::new(self.session_config(), crawl_tx);

        tokio::spawn(async move {
            session.coordinate(seed).await;
        });

        Ok(crawl_rx)
    }
}
