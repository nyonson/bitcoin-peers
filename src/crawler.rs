use crate::connection::Connection;
use crate::error::PeersError;
use bitcoin::p2p::address::AddrV2;
use bitcoin::Network;
use std::fmt;

/// Errors that can occur during crawler configuration.
#[derive(Debug, Clone)]
pub enum BuilderError {
    /// User agent doesn't follow the required format.
    InvalidUserAgent,
}

impl fmt::Display for BuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuilderError::InvalidUserAgent => {
                write!(f, "Invalid user agent: must follow format '/Name:Version/'")
            }
        }
    }
}

impl std::error::Error for BuilderError {}

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
fn validate_user_agent(user_agent: &str) -> Result<(), BuilderError> {
    if !user_agent.starts_with('/') || !user_agent.ends_with('/') || !user_agent.contains(':') {
        return Err(BuilderError::InvalidUserAgent);
    }

    // Extract the part between the slashes, split on the colon.
    let contents = &user_agent[1..user_agent.len() - 1];
    let parts: Vec<&str> = contents.split(':').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(BuilderError::InvalidUserAgent);
    }

    Ok(())
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
    pub fn with_user_agent<S: Into<String>>(mut self, user_agent: S) -> Result<Self, BuilderError> {
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
    /// Crawl the Bitcoin network starting from a seed peer.
    ///
    /// # Arguments
    ///
    /// * `seed_addr` - The address of the seed peer to start crawling from.
    /// * `port` - The port to connect to.
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the crawling was successful.
    /// * `Err(Error)` - If there was an error during crawling.
    pub async fn crawl(&self, seed_addr: AddrV2, port: u16) -> Result<(), PeersError> {
        self.pull_peers(seed_addr, port).await
    }

    /// Connect to a peer and pull its peers.
    ///
    /// # Arguments
    ///
    /// * `address` - The bitcoin network address (AddrV2).
    /// * `port` - The port number.
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the connection and handshake were successful.
    /// * `Err(Error)` - If the connection failed or the address type is not supported.
    async fn pull_peers(&self, address: AddrV2, port: u16) -> Result<(), PeersError> {
        let mut conn =
            Connection::tcp(&address, port, self.network, self.user_agent.clone()).await?;
        let services = conn.version_handshake(&address, port, None).await?;

        println!(
            "Successfully connected to peer at {:?}:{} with services {}",
            address, port, services
        );

        Ok(())
    }
}
