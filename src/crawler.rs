use crate::connection::Connection;
use crate::error::PeersError;
use crate::peer::Peer;
use bitcoin::Network;
use log::info;
use std::{
    collections::{HashSet, VecDeque},
    fmt,
    sync::Arc,
};
use tokio::sync::{
    mpsc::{self, Receiver},
    Mutex, Semaphore,
};

/// Errors that can occur during crawler configuration.
#[derive(Debug, Clone)]
pub enum CrawlerBuilderError {
    /// User agent doesn't follow the required format.
    InvalidUserAgent,
}

impl fmt::Display for CrawlerBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CrawlerBuilderError::InvalidUserAgent => {
                write!(f, "Invalid user agent: must follow format '/Name:Version/'")
            }
        }
    }
}

impl std::error::Error for CrawlerBuilderError {}

/// Validates that a user agent string follows the Bitcoin Core convention: "/Name:Version/"
///
/// # Arguments
///
/// * `user_agent` - The user agent string to validate
///
/// # Returns
///
/// * `Ok(())` - If the user agent is valid
/// * `Err(BuilderError)` - If the user agent format is invalid
fn validate_user_agent(user_agent: &str) -> Result<(), CrawlerBuilderError> {
    if !user_agent.starts_with('/') || !user_agent.ends_with('/') || !user_agent.contains(':') {
        return Err(CrawlerBuilderError::InvalidUserAgent);
    }

    // Extract the part between the slashes, split on the colon.
    let contents = &user_agent[1..user_agent.len() - 1];
    let parts: Vec<&str> = contents.split(':').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(CrawlerBuilderError::InvalidUserAgent);
    }

    Ok(())
}

/// Messages sent from the [`Crawler`] to the caller about peer discovery.
#[derive(Debug, Clone)]
pub enum CrawlerMessage {
    /// A peer that has been verified as listening by establishing a connection.
    Listening(Peer),
    /// A peer that failed to connect, perhaps due to non-listening or offline.
    NotListening(Peer),
}

impl fmt::Display for CrawlerMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CrawlerMessage::Listening(peer) => write!(f, "Listening: {peer}"),
            CrawlerMessage::NotListening(peer) => write!(f, "Not listening: {peer}"),
        }
    }
}

/// A crawler for the Bitcoin peer-to-peer network.
///
/// This crawler connects to Bitcoin nodes, performs handshakes, and can collect
/// peer information from the network.
#[derive(Debug, Clone)]
pub struct Crawler {
    /// Bitcoin network the [`Crawler`] operates on.
    network: Network,
    /// Custom user agent advertised for connection. Default is `/bitcoin-peers:$VERSION/`.
    user_agent: Option<String>,
    /// Peers which need to be tested. VecDeque for FIFO.
    discovered_peers: Arc<Mutex<VecDeque<Peer>>>,
    /// Peers which should no longer be considered.
    tested_peers: Arc<Mutex<HashSet<Peer>>>,
}

/// Builder for creating a customized [`Crawler`] instance.
///
/// # Example
///
/// ```
/// # fn main() -> Result<(), bitcoin_peers::CrawlerBuilderError> {
/// use bitcoin::Network;
/// use bitcoin_peers::CrawlerBuilder;
///
/// // Create a basic crawler for the Bitcoin mainnet
/// let basic_crawler = CrawlerBuilder::new(Network::Bitcoin).build();
///
/// // Create a crawler with a custom user agent using ? for error propagation
/// let custom_crawler = CrawlerBuilder::new(Network::Bitcoin)
///     .with_user_agent("/my-custom-crawler:1.0/")?
///     .build();
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct CrawlerBuilder {
    /// Bitcoin network the crawler will operate on.
    network: Network,
    /// Custom user agent advertised for connection.
    user_agent: Option<String>,
}

#[derive(Clone)]
struct CrawlSession {
    crawler: Crawler,
    crawl_tx: mpsc::Sender<CrawlerMessage>,
}

impl CrawlerBuilder {
    /// Create a new crawler builder for the specified network.
    ///
    /// # Arguments
    ///
    /// * `network` - The Bitcoin network to crawl.
    ///
    /// # Returns
    ///
    /// A new `CrawlerBuilder` instance.
    pub fn new(network: Network) -> Self {
        CrawlerBuilder {
            network,
            user_agent: None,
        }
    }

    /// Set a custom user agent string for the crawler.
    ///
    /// The user agent identifies the crawler to other peers on the network.
    /// It must follow Bitcoin Core's convention: "/Name:Version/".
    ///
    /// # Arguments
    ///
    /// * `user_agent` - The user agent string to use.
    ///
    /// # Returns
    ///
    /// * `Ok(Self)` - The builder for method chaining if validation succeeds.
    /// * `Err(BuilderError)` - If the user agent format is invalid.
    pub fn with_user_agent<S: Into<String>>(
        mut self,
        user_agent: S,
    ) -> Result<Self, CrawlerBuilderError> {
        let user_agent = user_agent.into();
        validate_user_agent(&user_agent)?;
        self.user_agent = Some(user_agent);
        Ok(self)
    }

    /// Build the crawler with the configured options.
    ///
    /// # Returns
    ///
    /// A configured `Crawler` instance.
    pub fn build(self) -> Crawler {
        Crawler {
            network: self.network,
            user_agent: self.user_agent,
            discovered_peers: Arc::new(Mutex::new(VecDeque::new())),
            tested_peers: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

impl Crawler {
    /// Crawl the bitcoin network starting from a seed peer.
    ///
    /// This method returns a channel that will receive peer messages as peers are verified.
    /// The channel will be closed when the crawling is complete or encounters an error.
    ///
    /// # Arguments
    ///
    /// * `seed` - The seed peer to start crawling from.
    ///
    /// # Returns
    ///
    /// * `Ok(Receiver<PeerMessage>)` - A channel that will receive peer messages
    /// * `Err(Error)` - If there was an error during crawling setup
    pub async fn crawl(&self, seed: Peer) -> Result<Receiver<CrawlerMessage>, PeersError> {
        let (crawl_tx, crawl_rx) = mpsc::channel(1000);
        self.discovered_peers.lock().await.push_back(seed);

        let session = CrawlSession {
            crawler: self.clone(),
            crawl_tx,
        };

        tokio::spawn(async move {
            session.coordinate().await;
        });

        Ok(crawl_rx)
    }
}

impl CrawlSession {
    /// Tests if a peer is listening and asks for the peers they know about.
    async fn process(&self, peer: Peer) {
        // Check and mark tested so we don't re-visit this session.
        {
            let mut tested = self.crawler.tested_peers.lock().await;
            if tested.contains(&peer) {
                return;
            }
            tested.insert(peer.clone());
        }

        let mut conn = match Connection::tcp(
            &peer.address,
            peer.port,
            self.crawler.network,
            self.crawler.user_agent.clone(),
        )
        .await
        {
            Ok(conn) => conn,
            Err(_) => {
                let _ = self.crawl_tx.send(CrawlerMessage::NotListening(peer)).await;
                return;
            }
        };

        let services = match conn.version_handshake(&peer.address, peer.port, None).await {
            Ok(services) => services,
            Err(_) => {
                let _ = self.crawl_tx.send(CrawlerMessage::NotListening(peer)).await;
                return;
            }
        };

        let peer = peer.with_services(services);
        let _ = self.crawl_tx.send(CrawlerMessage::Listening(peer)).await;

        if let Ok(peers) = conn.get_peers().await {
            let untested_peers = {
                let tested = self.crawler.tested_peers.lock().await;
                peers
                    .into_iter()
                    .filter(|p| !tested.contains(p))
                    .collect::<Vec<_>>()
            };

            if !untested_peers.is_empty() {
                let mut discovered = self.crawler.discovered_peers.lock().await;
                for new_peer in untested_peers {
                    discovered.push_back(new_peer);
                }
            }
        }
    }

    /// Coordinate crawling across the peers.
    async fn coordinate(&self) {
        let tasks = Arc::new(Semaphore::new(8));

        loop {
            let peer = self.crawler.discovered_peers.lock().await.pop_front();
            match peer {
                Some(peer) => {
                    // Acquire_owned so that it can be moved into the spawned task.
                    let permit = tasks.clone().acquire_owned().await.unwrap();
                    let task = self.clone();
                    tokio::spawn(async move {
                        task.process(peer).await;
                        drop(permit);
                    });
                }
                None => {
                    if tasks.available_permits() == 8 {
                        info!("Crawler exhausted");
                        break;
                    }
                }
            }
        }
    }
}
