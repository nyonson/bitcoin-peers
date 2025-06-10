use bitcoin::p2p::address::AddrV2;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::Network;
use bitcoin_peers_connection::{
    user_agent, Connection, ConnectionConfiguration, ConnectionError, FeaturePreferences,
    TransportPolicy,
};
use bitcoin_peers_connection::{Peer, PeerProtocolVersion};
use log::{debug, info};
use std::net::IpAddr;
use std::time::{Duration, Instant};
use std::{
    collections::{HashSet, VecDeque},
    fmt,
    sync::Arc,
};
use tokio::sync::{
    mpsc::{self, Receiver},
    Mutex, Semaphore,
};
use tokio::time::timeout;

/// The bitcoin p2p protocol version number used by this implementation.
const PROTOCOL_VERSION: PeerProtocolVersion = PeerProtocolVersion::Known(70016);

/// Errors that can occur during crawler configuration.
#[derive(Debug, Clone)]
pub enum CrawlerBuilderError {
    /// User agent doesn't follow the required format.
    InvalidUserAgent(user_agent::UserAgentError),
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
/// This crawler connects to bitcoin peers, performs handshakes, and asks for more peers.
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
/// # fn main() -> Result<(), bitcoin_peers_crawler::CrawlerBuilderError> {
/// use bitcoin::Network;
/// use bitcoin_peers_crawler::CrawlerBuilder;
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
        user_agent::validate_bitcoin_core_format(&user_agent)
            .map_err(CrawlerBuilderError::InvalidUserAgent)?;
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
    /// Requests peer addresses from a connected node by sending a getaddr message and collects the responses.
    ///
    /// # Arguments
    ///
    /// * `conn` - A connection to a Bitcoin peer
    /// * `max_wait` - Maximum duration to wait for responses
    /// * `max_addresses` - Maximum number of addresses to collect before returning early
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Peer>)` - A vector of peer information received from the node
    /// * `Err(ConnectionError)` - If an error occurs during the exchange
    async fn get_peers(
        &self,
        conn: &mut Connection,
        max_wait: Duration,
        max_addresses: usize,
    ) -> Result<Vec<Peer>, ConnectionError> {
        // Send GetAddr message
        conn.send(NetworkMessage::GetAddr).await?;

        debug!("Sent getaddr message to peer");

        let mut received_addresses = Vec::new();
        let mut address_count = 0;
        let start_time = Instant::now();

        while start_time.elapsed() < max_wait {
            // Wait for a message with a short timeout
            let timeout_duration = std::cmp::min(
                Duration::from_secs(5),
                max_wait.saturating_sub(start_time.elapsed()),
            );

            let message = match timeout(timeout_duration, conn.receive()).await {
                Ok(Ok(message)) => message,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    // Timeout on reading - if we have some addresses, consider it done.
                    if !received_addresses.is_empty() {
                        break;
                    }
                    // Otherwise continue waiting for the overall timeout.
                    continue;
                }
            };
            match message {
                NetworkMessage::Addr(addresses) => {
                    debug!("Received {} peer addresses", addresses.len());
                    address_count += addresses.len();

                    // Process each address (tuple of timestamp and Address struct)
                    for (_, addr) in addresses {
                        // Extract socket address - only IPv4/IPv6 addresses can be converted
                        if let Ok(socket_addr) = addr.socket_addr() {
                            match socket_addr.ip() {
                                IpAddr::V4(ipv4) => received_addresses.push(
                                    Peer::new(AddrV2::Ipv4(ipv4), socket_addr.port())
                                        .with_known_services(addr.services),
                                ),
                                IpAddr::V6(ipv6) => received_addresses.push(
                                    Peer::new(AddrV2::Ipv6(ipv6), socket_addr.port())
                                        .with_known_services(addr.services),
                                ),
                            }
                        }
                    }
                }
                NetworkMessage::AddrV2(addresses) => {
                    debug!("Received {} peer addresses (v2 format)", addresses.len());
                    address_count += addresses.len();
                    for addr_msg in addresses {
                        received_addresses.push(
                            Peer::new(addr_msg.addr, addr_msg.port)
                                .with_known_services(addr_msg.services),
                        );
                    }
                }
                _ => {
                    debug!("Received unexpected message in get_peers: {message:?}, ignoring");
                }
            }

            // If we've received a substantial number of addresses, we can finish early
            if address_count >= max_addresses {
                break;
            }
        }

        debug!(
            "Collected {} total peer addresses",
            received_addresses.len()
        );
        Ok(received_addresses)
    }

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
    pub async fn crawl(&self, seed: Peer) -> Result<Receiver<CrawlerMessage>, ConnectionError> {
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
            peer.clone(),
            self.crawler.network,
            ConnectionConfiguration::non_listening(
                PROTOCOL_VERSION,
                TransportPolicy::V2Preferred,
                FeaturePreferences::default(),
                self.crawler.user_agent.clone(),
            ),
        )
        .await
        {
            Ok(conn) => conn,
            Err(_) => {
                let _ = self.crawl_tx.send(CrawlerMessage::NotListening(peer)).await;
                return;
            }
        };

        // The connection has been established and handshake completed.
        // Services and version are updated in the peer.
        let peer_info = conn.peer().await;
        let _ = self
            .crawl_tx
            .send(CrawlerMessage::Listening(peer_info))
            .await;

        if let Ok(peers) = self
            .crawler
            .get_peers(&mut conn, Duration::from_secs(20), 1000)
            .await
        {
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
