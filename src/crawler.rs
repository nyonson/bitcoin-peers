use crate::connection::Connection;
use crate::error::PeersError;
use crate::peer::Peer;
use bitcoin::p2p::address::AddrV2;
use bitcoin::Network;
use std::fmt;
use tokio::sync::mpsc::{self, Receiver};

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
            CrawlerMessage::Listening(peer) => write!(f, "Listening: {}", peer),
            CrawlerMessage::NotListening(peer) => write!(f, "Not listening: {}", peer),
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
}

/// Builder for creating a customized [`Crawler`] instance.
///
/// # Example
///
/// ```
/// # fn main() -> Result<(), bitcoin_peers::crawler::BuilderError> {
/// use bitcoin::Network;
/// use bitcoin_peers::crawler::CrawlerBuilder;
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
    /// * `seed_addr` - The address of the seed peer to start crawling from.
    /// * `port` - The port to connect to.
    ///
    /// # Returns
    ///
    /// * `Ok(Receiver<PeerMessage>)` - A channel that will receive peer messages
    /// * `Err(Error)` - If there was an error during crawling setup
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use bitcoin::p2p::address::AddrV2;
    /// # use bitcoin::Network;
    /// # use bitcoin_peers::CrawlerBuilder;
    /// # use std::net::Ipv4Addr;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let crawler = CrawlerBuilder::new(Network::Bitcoin).build();
    /// let addr = AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1));
    ///
    /// // Start crawling and get a channel of peer messages
    /// let mut peers_rx = crawler.crawl(addr, 8333).await?;
    ///
    /// // Process peer messages as they arrive
    /// while let Some(peer_msg) = peers_rx.recv().await {
    ///     println!("{}", peer_msg);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn crawl(
        &self,
        seed_addr: AddrV2,
        port: u16,
    ) -> Result<Receiver<CrawlerMessage>, PeersError> {
        let (tx, rx) = mpsc::channel(1000);

        // Clone the necessary parts of self for the async task
        let network = self.network;
        let user_agent = self.user_agent.clone();

        tokio::spawn(async move {
            let connect_result = Connection::tcp(&seed_addr, port, network, user_agent).await;

            let mut conn = match connect_result {
                Ok(conn) => conn,
                Err(_) => {
                    let peer = Peer {
                        address: seed_addr,
                        port,
                        services: Default::default(),
                    };
                    let _ = tx.send(CrawlerMessage::NotListening(peer)).await;
                    return;
                }
            };

            let services = match conn.version_handshake(&seed_addr, port, None).await {
                Ok(services) => services,
                Err(_) => {
                    let peer = Peer {
                        address: seed_addr,
                        port,
                        services: Default::default(),
                    };
                    let _ = tx.send(CrawlerMessage::NotListening(peer)).await;
                    return;
                }
            };

            let seed_peer = Peer {
                address: seed_addr.clone(),
                port,
                services,
            };
            if tx
                .send(CrawlerMessage::Listening(seed_peer.clone()))
                .await
                .is_err()
            {
                return; // Receiver was dropped
            }

            println!(
                "Successfully connected to peer at {:?}:{} with services {}",
                seed_addr, port, services
            );

            // Get peers from the connected node
            match conn.get_peers().await {
                Ok(_peers) => {
                    println!("Got peers");
                    // For this initial version, we're not verifying connections to discovered peers
                    // In a future version, we could attempt to connect to each discovered peer
                    // and report back their connection status
                }
                Err(e) => {
                    eprintln!("Error getting peers: {}", e);
                }
            }
        });

        Ok(rx)
    }
}
