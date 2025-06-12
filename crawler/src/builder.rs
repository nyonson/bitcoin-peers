//! Builder pattern for configuring and creating crawler instances.

use crate::crawler::Crawler;
use bitcoin::Network;
use bitcoin_peers_connection::{PeerProtocolVersion, TransportPolicy, UserAgent, UserAgentError};
use std::fmt;
use std::time::Duration;

/// Default protocol version for crawler connections.
const DEFAULT_PROTOCOL_VERSION: PeerProtocolVersion = PeerProtocolVersion::Known(70016);
/// Default maximum number of concurrent connection tasks.
const DEFAULT_MAX_CONCURRENT_TASKS: usize = 8;
/// Default timeout for peer operations.
const DEFAULT_PEER_TIMEOUT: Duration = Duration::from_secs(20);

/// Errors that can occur during crawler configuration.
#[derive(Debug, Clone)]
pub enum CrawlerBuilderError {
    /// User agent doesn't follow the required format.
    InvalidUserAgent(UserAgentError),
}

impl fmt::Display for CrawlerBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CrawlerBuilderError::InvalidUserAgent(err) => {
                write!(f, "Invalid user agent: {err}")
            }
        }
    }
}

impl std::error::Error for CrawlerBuilderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CrawlerBuilderError::InvalidUserAgent(err) => Some(err),
        }
    }
}

/// Builder for creating a customized [`Crawler`] instance.
///
/// # Example
///
/// ```
/// # fn main() -> Result<(), bitcoin_peers_crawler::CrawlerBuilderError> {
/// use bitcoin::Network;
/// use bitcoin_peers_crawler::{CrawlerBuilder, TransportPolicy};
///
/// // Create a basic crawler for the Bitcoin mainnet
/// let basic_crawler = CrawlerBuilder::new(Network::Bitcoin).build();
///
/// // Create a crawler with custom settings
/// let custom_crawler = CrawlerBuilder::new(Network::Bitcoin)
///     .with_user_agent("/my-custom-crawler:1.0/")?
///     .with_transport_policy(TransportPolicy::V2Required)
///     .with_protocol_version(70015)
///     .with_max_concurrent_tasks(16)
///     .build();
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct CrawlerBuilder {
    /// Bitcoin network the crawler will operate on.
    network: Network,
    /// Custom user agent advertised for connection.
    user_agent: Option<UserAgent>,
    /// Transport policy for connections.
    transport_policy: TransportPolicy,
    /// Protocol version to advertise in connections.
    protocol_version: PeerProtocolVersion,
    /// Maximum number of concurrent connection tasks.
    max_concurrent_tasks: usize,
    /// Timeout for peer operations.
    peer_timeout: Duration,
}

impl CrawlerBuilder {
    /// Create a new crawler builder for the specified network.
    ///
    /// # Arguments
    ///
    /// * `network` - The bitcoin network to crawl.
    ///
    /// # Returns
    ///
    /// A new `CrawlerBuilder` instance.
    pub fn new(network: Network) -> Self {
        CrawlerBuilder {
            network,
            user_agent: None,
            transport_policy: TransportPolicy::V2Preferred,
            protocol_version: DEFAULT_PROTOCOL_VERSION,
            max_concurrent_tasks: DEFAULT_MAX_CONCURRENT_TASKS,
            peer_timeout: DEFAULT_PEER_TIMEOUT,
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
        let user_agent =
            UserAgent::new(user_agent.into()).map_err(CrawlerBuilderError::InvalidUserAgent)?;
        self.user_agent = Some(user_agent);
        Ok(self)
    }

    /// Set the transport policy for connections.
    ///
    /// Controls whether to require V2 transport or prefer V2 with V1 fallback.
    ///
    /// # Arguments
    ///
    /// * `policy` - The transport policy to use for connections.
    ///
    /// # Returns
    ///
    /// Self for method chaining.
    pub fn with_transport_policy(mut self, policy: TransportPolicy) -> Self {
        self.transport_policy = policy;
        self
    }

    /// Set the protocol version to advertise in connections.
    ///
    /// # Arguments
    ///
    /// * `version` - The protocol version to advertise.
    ///
    /// # Returns
    ///
    /// Self for method chaining.
    pub fn with_protocol_version(mut self, version: u32) -> Self {
        self.protocol_version = PeerProtocolVersion::Known(version);
        self
    }

    /// Set the maximum number of concurrent connection tasks.
    ///
    /// Controls how many peers can be tested simultaneously. Higher values
    /// may speed up crawling, but increase resource usage and network load.
    ///
    /// # Recommendations
    ///
    /// * **Conservative (1-4)** - For slow networks or resource-constrained environments.
    /// * **Default (8)** - Good balance for most use cases.
    /// * **Aggressive (16-32)** - For fast crawling with ample resources.
    ///
    /// # Arguments
    ///
    /// * `max_tasks` - Maximum concurrent tasks (defaults to 8).
    ///
    /// # Returns
    ///
    /// Self for method chaining.
    pub fn with_max_concurrent_tasks(mut self, max_tasks: usize) -> Self {
        self.max_concurrent_tasks = max_tasks;
        self
    }

    /// Set the timeout for peer operations.
    ///
    /// This timeout applies to all peer-related operations.
    ///
    /// * Connection establishment (TCP connect + handshake)
    /// * Requesting peer addresses after establishing a connection
    /// * Waiting for responses to protocol messages
    ///
    /// A longer timeout may help on slow networks, while a shorter timeout
    /// can speed up crawling when peers are unresponsive.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for peer operations (defaults to 20 seconds).
    ///
    /// # Returns
    ///
    /// Self for method chaining.
    pub fn with_peer_timeout(mut self, timeout: Duration) -> Self {
        self.peer_timeout = timeout;
        self
    }

    /// Build the crawler with the configured options.
    ///
    /// # Returns
    ///
    /// A configured `Crawler` instance.
    pub fn build(self) -> Crawler {
        Crawler::new(
            self.network,
            self.user_agent,
            self.transport_policy,
            self.protocol_version,
            self.max_concurrent_tasks,
            self.peer_timeout,
        )
    }
}
