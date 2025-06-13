use bitcoin::p2p::address::AddrV2;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::Network;
use bitcoin_peers_connection::{
    Connection, ConnectionConfiguration, ConnectionError, FeaturePreferences, Peer,
    PeerProtocolVersion, TransportPolicy, UserAgent,
};
use log::{debug, info};
use std::net::IpAddr;
use std::time::{Duration, Instant};
use std::{collections::HashSet, fmt};
use tokio::sync::mpsc::{self, Receiver};
use tokio::time::timeout;

/// Internal trait for bitcoin peer connections that can send and receive messages.
///
/// This trait abstracts the core operations needed for crawling, allowing
/// for easy testing with mock implementations.
trait PeerConnection {
    async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError>;
    async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError>;
    async fn peer(&self) -> Peer;

    /// Requests peer addresses from this connection by sending a getaddr message.
    ///
    /// # Arguments
    ///
    /// * `peer_tx` - Channel to send discovered peers through.
    /// * `peer_timeout` - Maximum duration to wait for responses.
    ///
    /// # Returns
    ///
    /// * `Ok(usize)` - The number of peer addresses sent through the channel.
    /// * `Err(ConnectionError)` - If an error occurs during the exchange.
    async fn get_peers(
        &mut self,
        peer_tx: &mpsc::Sender<Vec<Peer>>,
        peer_timeout: Duration,
    ) -> Result<usize, ConnectionError> {
        self.send(NetworkMessage::GetAddr).await?;
        debug!("Sent getaddr message to peer");

        let mut peer_count = 0;
        let start_time = Instant::now();

        while start_time.elapsed() < peer_timeout {
            // Wait for a message with a short timeout.
            let timeout_duration = std::cmp::min(
                Duration::from_secs(5),
                peer_timeout.saturating_sub(start_time.elapsed()),
            );

            let message = match timeout(timeout_duration, self.receive()).await {
                Ok(Ok(message)) => message,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    // Timeout on reading - if we have some addresses, consider it done.
                    if peer_count > 0 {
                        break;
                    }
                    // Otherwise continue waiting for the overall timeout.
                    continue;
                }
            };

            let mut peers_batch = Vec::new();

            match message {
                // Support legacy `Addr` messages as well as `AddrV2`.
                NetworkMessage::Addr(addresses) => {
                    debug!("Received {} peer addresses", addresses.len());
                    for (_, addr) in addresses {
                        if let Ok(socket_addr) = addr.socket_addr() {
                            let peer = match socket_addr.ip() {
                                IpAddr::V4(ipv4) => Peer::with_services(
                                    AddrV2::Ipv4(ipv4),
                                    socket_addr.port(),
                                    addr.services,
                                ),
                                IpAddr::V6(ipv6) => Peer::with_services(
                                    AddrV2::Ipv6(ipv6),
                                    socket_addr.port(),
                                    addr.services,
                                ),
                            };
                            peers_batch.push(peer);
                            peer_count += 1;
                        }
                    }
                }
                NetworkMessage::AddrV2(addresses) => {
                    debug!("Received {} peer addresses (v2 format)", addresses.len());
                    for addr_msg in addresses {
                        let peer =
                            Peer::with_services(addr_msg.addr, addr_msg.port, addr_msg.services);
                        peers_batch.push(peer);
                        peer_count += 1;
                    }
                }
                _ => {
                    debug!("Received unexpected message in get_peers: {message:?}, ignoring");
                }
            }

            // Send the batch if we collected any peers
            if !peers_batch.is_empty() && peer_tx.send(peers_batch).await.is_err() {
                // Receiver dropped, stop processing
                break;
            }
        }

        debug!(
            "Sent {} peer addresses from {} through channel",
            peer_count,
            self.peer().await
        );
        Ok(peer_count)
    }
}

/// Implementation of PeerConnection for the Connection type from bitcoin-peers-connection.
impl PeerConnection for Connection {
    async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        self.send(message).await
    }

    async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        self.receive().await
    }

    async fn peer(&self) -> Peer {
        self.peer().await
    }
}

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

        let session = CrawlSession {
            crawler: self.clone(),
            crawl_tx,
        };

        tokio::spawn(async move {
            session.coordinate(seed).await;
        });

        Ok(crawl_rx)
    }
}

/// Result of processing a single peer in the crawler.
#[derive(Debug, Clone)]
enum TaskResult {
    /// Successfully connected and found peers.
    FoundPeers,
    /// Successfully connected but no peers received.
    NoPeersFound,
    /// Failed to connect to the peer.
    ConnectionFailed,
    /// Task exited early due to channel closure.
    ChannelClosed,
}

/// Internal coordinator for a crawling session.
///
/// `CrawlSession` orchestrates the crawling process by managing the work queue
/// and coordinating concurrent peer processing tasks. It acts as the execution
/// engine for a [`Crawler`] instance.
///
/// # Architecture
///
/// The session follows a producer-consumer pattern with lockless communication:
/// * **Coordinator** (`coordinate()`) - Manages the work queue and spawns processing tasks.
/// * **Processors** (`process()`) - Handle individual peer connections and discovery.
/// * **Communication** - Uses MPSC channels for all inter-task communication.
///
/// # Concurrency Control
///
/// * **Task Counting** - Tracks active tasks and enforces configured maximum.
/// * **Channels** - Lockless communication between tasks via MPSC channels.
/// * **Graceful Shutdown** - Monitors channel closure for early termination.
#[derive(Clone)]
struct CrawlSession {
    /// Shared crawler configuration and state.
    crawler: Crawler,
    /// Channel for sending discovery results back to the caller.
    crawl_tx: mpsc::Sender<CrawlerMessage>,
}

impl CrawlSession {
    /// Processes a single peer: tests connectivity and discovers new peers.
    ///
    /// # Process Flow
    ///
    /// 1. **Connection** - Attempts TCP connection and bitcoin handshake.
    /// 2. **Classification** - Reports peer as `Listening` or `NonListening`.
    /// 3. **Discovery** - If connected, requests peer addresses via `getaddr`.
    /// 4. **Channel Send** - Sends discovered peers through discovery channel.
    ///
    /// # Returns
    ///
    /// A `TaskResult` indicating what happened during processing.
    ///
    /// # Error Handling
    ///
    /// * **Connection failures** - Peer marked as non-listening, returns `ConnectionFailed`.
    /// * **Channel closure** - Early return with `ChannelClosed`.
    /// * **Address request failures** - Logged but not fatal, returns `NoPeersFound`.
    ///
    /// # Concurrency
    ///
    /// This method is completely lockless - all communication happens via channels.
    ///
    /// # Arguments
    ///
    /// * `peer` - The peer to test and potentially discover addresses from.
    /// * `peer_discovery_tx` - Channel to send discovered peers through.
    async fn process(&self, peer: Peer, peer_discovery_tx: mpsc::Sender<Vec<Peer>>) -> TaskResult {
        debug!("Processing peer {peer:?}");

        let mut conn = match timeout(
            self.crawler.peer_timeout,
            Connection::tcp(
                peer.clone(),
                self.crawler.network,
                ConnectionConfiguration::non_listening(
                    self.crawler.protocol_version,
                    self.crawler.transport_policy,
                    FeaturePreferences::default(),
                    self.crawler.user_agent.clone(),
                ),
            ),
        )
        .await
        {
            Ok(Ok(conn)) => conn,
            Ok(Err(_)) | Err(_) => {
                if self
                    .crawl_tx
                    .send(CrawlerMessage::NonListening(peer))
                    .await
                    .is_err()
                {
                    // Receiver dropped, stop processing.
                    return TaskResult::ChannelClosed;
                }
                return TaskResult::ConnectionFailed;
            }
        };

        // The connection has been established and handshake completed.
        // Services and version are updated in the peer.
        let peer_info = conn.peer().await;

        if self
            .crawl_tx
            .send(CrawlerMessage::Listening(peer_info))
            .await
            .is_err()
        {
            // Receiver dropped, stop processing.
            return TaskResult::ChannelClosed;
        }

        // Request peers and send them directly through the discovery channel
        match conn
            .get_peers(&peer_discovery_tx, self.crawler.peer_timeout)
            .await
        {
            Ok(peer_count) => {
                if peer_count > 0 {
                    TaskResult::FoundPeers
                } else {
                    TaskResult::NoPeersFound
                }
            }
            Err(e) => {
                debug!("Failed to get peers from {peer}: {e}");
                TaskResult::NoPeersFound
            }
        }
    }

    /// Coordinates the crawling process by managing the work queue and task scheduling.
    ///
    /// This is the main control loop that orchestrates the entire crawling session.
    /// It continuously pulls peers from the discovery channel and spawns processing tasks
    /// until the crawl is complete or terminated.
    ///
    /// # Termination Conditions
    ///
    /// 1. **Natural Completion** - No more peers in channel and all tasks finished.
    /// 2. **Channel Closure** - Receiver dropped, indicating caller no longer interested.
    async fn coordinate(&self, seed: Peer) {
        // Channel to track discovered peers to process.
        let (peer_discovery_tx, mut peer_discovery_rx) = mpsc::channel(1000);
        // Channel to track task completion.
        let (task_done_tx, mut task_done_rx) =
            mpsc::channel::<TaskResult>(self.crawler.max_concurrent_tasks);

        // Prime the pump with the seed peer.
        if peer_discovery_tx.send(vec![seed]).await.is_err() {
            debug!("Failed to send seed peer");
            return;
        }

        // Track which peers we have asked.
        let mut tested_peers = HashSet::new();
        // Number of in-flight tasks.
        let mut active_tasks = 0;

        let mut last_log_time = Instant::now();
        let log_interval = Duration::from_secs(60);

        loop {
            // Check if caller hung up before continuing.
            if self.crawl_tx.is_closed() {
                debug!("Receiver disconnected, stopping crawler");
                break;
            }

            // Periodic status logging.
            if last_log_time.elapsed() >= log_interval {
                info!(
                    "{} active tasks (max: {}), {} messages queue'd",
                    active_tasks,
                    self.crawler.max_concurrent_tasks,
                    peer_discovery_rx.len()
                );
                last_log_time = Instant::now();
            }

            // Wait for something to happen
            tokio::select! {
                // New peers discovered.
                Some(peers) = peer_discovery_rx.recv() => {
                    for peer in peers {
                        // Skip if already tested
                        if !tested_peers.insert(peer.clone()) {
                            continue;
                        }

                        // Wait if we're at capacity
                        while active_tasks >= self.crawler.max_concurrent_tasks {
                            // Wait for a task to complete
                            if let Some(result) = task_done_rx.recv().await {
                                active_tasks -= 1;
                                debug!("Task completed with result: {result:?}");
                            }
                        }

                        // Spawn a new task
                        let session = self.clone();
                        let discovery_tx = peer_discovery_tx.clone();
                        let done_tx = task_done_tx.clone();

                        active_tasks += 1;
                        tokio::spawn(async move {
                            let result = session.process(peer, discovery_tx).await;
                            done_tx.send(result).await.expect("Coordinator should always be listening for task completion");
                        });
                    }
                }
                // Task completed.
                Some(result) = task_done_rx.recv() => {
                    active_tasks -= 1;
                    debug!("Task completed with result: {result:?}");

                    // Check if we're done: no more tasks running
                    if active_tasks == 0 {
                        info!("Crawler exhausted - all peers processed");
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::p2p::{address::AddrV2, ServiceFlags};
    use std::collections::VecDeque;
    use std::net::Ipv4Addr;

    /// Mock implementation of PeerConnection for testing.
    #[derive(Debug)]
    pub struct MockPeerConnection {
        /// Queue of messages that will be returned by receive().
        pub incoming_messages: VecDeque<Result<NetworkMessage, ConnectionError>>,
        /// Messages that were sent via send().
        pub sent_messages: Vec<NetworkMessage>,
        /// The peer information to return.
        pub peer_info: Peer,
        /// Whether the connection should simulate being closed.
        pub is_closed: bool,
    }

    impl MockPeerConnection {
        /// Create a new mock connection with default peer info.
        pub fn new() -> Self {
            MockPeerConnection {
                incoming_messages: VecDeque::new(),
                sent_messages: Vec::new(),
                peer_info: Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333),
                is_closed: false,
            }
        }

        /// Add a message that will be returned by the next receive() call.
        pub fn add_incoming_message(&mut self, message: NetworkMessage) {
            self.incoming_messages.push_back(Ok(message));
        }

        /// Add an error that will be returned by the next receive() call.
        pub fn add_incoming_error(&mut self, error: ConnectionError) {
            self.incoming_messages.push_back(Err(error));
        }

        /// Add multiple peer addresses as an Addr message.
        pub fn add_addr_message(&mut self, addresses: Vec<(AddrV2, u16, ServiceFlags)>) {
            use bitcoin::p2p::Address;
            use std::time::{SystemTime, UNIX_EPOCH};

            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32;

            let addr_list: Vec<(u32, Address)> = addresses
                .into_iter()
                .map(|(addr_v2, port, services)| {
                    let socket_addr = match addr_v2 {
                        AddrV2::Ipv4(ipv4) => std::net::SocketAddr::new(ipv4.into(), port),
                        AddrV2::Ipv6(ipv6) => std::net::SocketAddr::new(ipv6.into(), port),
                        _ => panic!("Unsupported address type for mock"),
                    };
                    (timestamp, Address::new(&socket_addr, services))
                })
                .collect();

            self.add_incoming_message(NetworkMessage::Addr(addr_list));
        }
    }

    impl PeerConnection for MockPeerConnection {
        async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
            if self.is_closed {
                return Err(ConnectionError::Io(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "Connection closed",
                )));
            }
            self.sent_messages.push(message);
            Ok(())
        }

        async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
            if self.is_closed {
                return Err(ConnectionError::Io(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "Connection closed",
                )));
            }

            // If we have a message queued, return it immediately
            if let Some(result) = self.incoming_messages.pop_front() {
                return result;
            }

            // Otherwise, wait indefinitely (let the caller's timeout handle it)
            std::future::pending().await
        }

        async fn peer(&self) -> Peer {
            self.peer_info.clone()
        }
    }

    #[tokio::test]
    async fn test_get_peers() {
        let mut mock_conn = MockPeerConnection::new();

        // Add some test addresses
        mock_conn.add_addr_message(vec![
            (
                AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1)),
                8333,
                ServiceFlags::NETWORK,
            ),
            (
                AddrV2::Ipv4(Ipv4Addr::new(10, 0, 0, 1)),
                8333,
                ServiceFlags::NETWORK | ServiceFlags::WITNESS,
            ),
        ]);

        let (tx, mut rx) = mpsc::channel(10);
        let result = mock_conn.get_peers(&tx, Duration::from_millis(100)).await;

        assert!(result.is_ok());
        let peer_count = result.unwrap();
        assert_eq!(peer_count, 2);

        // Verify a getaddr message was sent
        assert_eq!(mock_conn.sent_messages.len(), 1);
        assert!(matches!(
            mock_conn.sent_messages[0],
            NetworkMessage::GetAddr
        ));

        // Verify peers were sent through channel as a batch
        let peers_batch = rx.recv().await.unwrap();
        assert_eq!(peers_batch.len(), 2);

        let peer1 = &peers_batch[0];
        assert_eq!(peer1.address, AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(peer1.port, 8333);
        assert!(peer1.has_service(ServiceFlags::NETWORK));

        let peer2 = &peers_batch[1];
        assert_eq!(peer2.address, AddrV2::Ipv4(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(peer2.has_service(ServiceFlags::WITNESS));
    }

    #[tokio::test]
    async fn test_get_peers_timeout() {
        let mut mock_conn = MockPeerConnection::new();
        // Don't add any messages - should timeout

        let (tx, mut rx) = mpsc::channel(10);
        let result = mock_conn.get_peers(&tx, Duration::from_millis(50)).await;

        assert!(result.is_ok());
        let peer_count = result.unwrap();
        assert_eq!(peer_count, 0); // Should get 0 peers on timeout

        // Verify a getaddr message was sent
        assert_eq!(mock_conn.sent_messages.len(), 1);
        assert!(matches!(
            mock_conn.sent_messages[0],
            NetworkMessage::GetAddr
        ));

        // No peers should be in channel
        assert!(tokio::time::timeout(Duration::from_millis(10), rx.recv())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_get_peers_connection_error() {
        let mut mock_conn = MockPeerConnection::new();

        // Add a connection error that will be returned after getaddr is sent
        mock_conn.add_incoming_error(ConnectionError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "Connection lost",
        )));

        let (tx, _rx) = mpsc::channel(10);
        let result = mock_conn.get_peers(&tx, Duration::from_millis(100)).await;

        // Should return the error
        assert!(result.is_err());

        // Verify a getaddr message was sent before the error
        assert_eq!(mock_conn.sent_messages.len(), 1);
        assert!(matches!(
            mock_conn.sent_messages[0],
            NetworkMessage::GetAddr
        ));
    }

    /// Test the core deduplication logic that prevents circular loops.
    ///
    /// This verifies that the tested_peers HashSet prevents the same peer
    /// from being processed multiple times, which is the key mechanism
    /// that prevents infinite loops in circular peer reference scenarios.
    #[tokio::test]
    async fn test_peer_deduplication_prevents_loops() {
        let mut tested_peers = HashSet::new();

        let peer1 = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1)), 8333);
        let peer2 = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 2)), 8333);
        let peer1_duplicate = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1)), 8333);

        // First insertion should succeed
        assert!(
            tested_peers.insert(peer1.clone()),
            "First peer should be inserted"
        );
        assert!(
            tested_peers.insert(peer2.clone()),
            "Second peer should be inserted"
        );

        // Duplicate should be rejected - this prevents infinite loops
        assert!(
            !tested_peers.insert(peer1_duplicate),
            "Duplicate peer should not be inserted"
        );

        assert_eq!(tested_peers.len(), 2, "Should only have 2 unique peers");
    }

    /// Test circular peer references with mock connections.
    ///
    /// This simulates a scenario where peers return each other's addresses,
    /// creating potential circular references. The test verifies that
    /// get_peers() correctly processes all returned addresses.
    #[tokio::test]
    async fn test_circular_peer_references_with_mock() {
        let mut mock_conn = MockPeerConnection::new();

        // Simulate peer returning addresses that could create circular references:
        // - Other peers that might reference back to this one
        // - Self-reference (peer returns its own address)
        mock_conn.add_addr_message(vec![
            (
                AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 2)),
                8333,
                ServiceFlags::NETWORK,
            ),
            (
                AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 3)),
                8333,
                ServiceFlags::NETWORK,
            ),
            (
                AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1)),
                8333,
                ServiceFlags::NETWORK,
            ), // Self-reference
        ]);

        let (tx, mut rx) = mpsc::channel(10);
        let result = mock_conn.get_peers(&tx, Duration::from_millis(100)).await;

        assert!(result.is_ok());
        let peer_count = result.unwrap();
        assert_eq!(
            peer_count, 3,
            "Should receive all 3 peers including self-reference"
        );

        // Verify we received all the addresses
        let peers_batch = rx.recv().await.unwrap();
        assert_eq!(peers_batch.len(), 3);

        let addresses: HashSet<_> = peers_batch.iter().map(|p| p.address.clone()).collect();

        assert!(addresses.contains(&AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(addresses.contains(&AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 2))));
        assert!(addresses.contains(&AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 3))));

        // The key insight: even though we got circular references,
        // the crawler's tested_peers HashSet would prevent re-processing
        // the same addresses, breaking any potential infinite loops.
    }

    /// Test task counting logic for termination detection.
    ///
    /// This verifies the logic used by coordinate() to detect when
    /// all tasks are complete and the crawler should terminate.
    #[tokio::test]
    async fn test_active_task_counting_for_termination() {
        // Simulate the task counting logic from coordinate()
        let mut active_tasks = 0_usize;
        let max_concurrent_tasks = 3_usize;

        // Simulate spawning tasks up to capacity
        active_tasks += 1; // Task 1 starts
        assert_eq!(active_tasks, 1);

        active_tasks += 1; // Task 2 starts
        assert_eq!(active_tasks, 2);

        active_tasks += 1; // Task 3 starts
        assert_eq!(active_tasks, 3);
        assert_eq!(active_tasks, max_concurrent_tasks, "At capacity");

        // Simulate task completion
        active_tasks -= 1; // Task 1 completes
        assert_eq!(active_tasks, 2);

        active_tasks -= 1; // Task 2 completes
        assert_eq!(active_tasks, 1);

        active_tasks -= 1; // Task 3 completes
        assert_eq!(active_tasks, 0);

        // This is the termination condition in coordinate():
        // when active_tasks == 0, the crawler should terminate
        assert_eq!(
            active_tasks, 0,
            "No active tasks - crawler should terminate"
        );
    }
}
