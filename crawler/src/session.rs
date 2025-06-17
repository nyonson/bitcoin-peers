//! Internal session coordination for crawling operations.
//!
//! This module contains the [`CrawlSession`] which orchestrates the crawling process
//! by managing the work queue and coordinating concurrent peer processing tasks.

use crate::connection::{Connector, PeerConnection};
use crate::crawler::CrawlerMessage;
use bitcoin_peers_connection::Peer;
use log::{debug, info};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Configuration for a crawl session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub max_concurrent_tasks: usize,
    pub peer_timeout: Duration,
}

/// Result of processing a single peer in the crawler.
#[derive(Debug, Clone, PartialEq)]
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
/// The session follows a producer-consumer pattern with lockless communication.
///
/// * **Coordinator** (`coordinate()`) - Manages the work queue and spawns processing tasks.
/// * **Processors** (`process()`) - Handle individual peer connections and discovery.
#[derive(Clone)]
pub struct CrawlSession<C: Connector> {
    /// Crawler configuration.
    config: SessionConfig,
    /// Channel for sending discovery results back to the caller.
    crawl_tx: mpsc::Sender<CrawlerMessage>,
    /// Shared set of peers that have already been tested, used for deduplication.
    tested_peers: Arc<tokio::sync::RwLock<HashSet<Peer>>>,
    /// Connector for creating peer connections.
    connector: C,
}

impl<C: Connector> CrawlSession<C> {
    /// Create a new crawl session.
    pub fn new(
        config: SessionConfig,
        crawl_tx: mpsc::Sender<CrawlerMessage>,
        connector: C,
    ) -> Self {
        Self {
            config,
            crawl_tx,
            tested_peers: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            connector,
        }
    }

    /// Processes a single peer: establishes connection and discovers new peers.
    ///
    /// This method handles connection establishment and peer discovery.
    ///
    /// # Returns
    ///
    /// A `TaskResult` indicating what happened during processing.
    ///
    /// # Arguments
    ///
    /// * `peer` - The peer to test and potentially discover addresses from.
    /// * `peer_discovery_tx` - Channel to send discovered peers through.
    async fn process(
        &self,
        peer: Peer,
        peer_discovery_tx: mpsc::UnboundedSender<Vec<Peer>>,
    ) -> TaskResult {
        debug!("Establishing connection to peer {peer:?}");

        // First, establish the connection
        let mut connection =
            match timeout(self.config.peer_timeout, self.connector.connect(&peer)).await {
                Ok(Ok(connection)) => connection,
                Ok(Err(_)) | Err(_) => {
                    if self
                        .crawl_tx
                        .send(CrawlerMessage::NonListening(peer))
                        .await
                        .is_err()
                    {
                        return TaskResult::ChannelClosed;
                    }
                    return TaskResult::ConnectionFailed;
                }
            };

        // Connection established, now discover peers
        let peer_info = connection.peer().await;
        debug!("Processing peer {peer_info:?}");

        if self
            .crawl_tx
            .send(CrawlerMessage::Listening(peer_info.clone()))
            .await
            .is_err()
        {
            debug!("Failed to send Listening for {peer_info:?}");
            return TaskResult::ChannelClosed;
        }
        debug!("Sent Listening for {peer_info:?}");

        match connection.get_peers(self.config.peer_timeout).await {
            Ok(discovered_peers) => {
                if discovered_peers.is_empty() {
                    return TaskResult::NoPeersFound;
                }

                // Filter out peers that have already been tested.
                // This is simply a performance optimization, deduplication
                // is ensured at the coordinator level.
                let mut filtered_peers = Vec::new();
                {
                    let tested = self.tested_peers.read().await;
                    for peer in discovered_peers {
                        if !tested.contains(&peer) {
                            filtered_peers.push(peer);
                        }
                    }
                }

                if !filtered_peers.is_empty() {
                    if peer_discovery_tx.send(filtered_peers).is_err() {
                        return TaskResult::ChannelClosed;
                    }
                    debug!("Sent peers for {peer_info:?}");
                    TaskResult::FoundPeers
                } else {
                    TaskResult::NoPeersFound
                }
            }
            Err(e) => {
                debug!("Failed to get peers from {peer_info}: {e}");
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
    pub async fn coordinate(&self, seed: Peer) {
        // Channel to track discovered peers to process.
        // Use unbounded channel to prevent tasks from blocking on peer discovery
        let (peer_discovery_tx, mut peer_discovery_rx) = mpsc::unbounded_channel();
        // Channel to track task completion.
        let (task_done_tx, mut task_done_rx) =
            mpsc::channel::<TaskResult>(self.config.max_concurrent_tasks);

        // Prime the pump with the seed peer.
        if peer_discovery_tx.send(vec![seed]).is_err() {
            debug!("Failed to send seed peer");
            return;
        }

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

            // Periodic status logging, but can be crowded out by large peer batches.
            if last_log_time.elapsed() >= log_interval {
                let tested_count = self.tested_peers.read().await.len();
                info!(
                    "{} active tasks (max: {}), {} unique peers tested",
                    active_tasks, self.config.max_concurrent_tasks, tested_count
                );
                last_log_time = Instant::now();
            }

            tokio::select! {
                // New peers discovered.
                Some(peers) = peer_discovery_rx.recv() => {
                    for peer in peers {
                        // Skip if already tested
                        if !self.tested_peers.write().await.insert(peer.clone()) {
                            continue;
                        }

                        // Wait if we're at capacity.
                        while active_tasks >= self.config.max_concurrent_tasks {
                            if let Some(result) = task_done_rx.recv().await {
                                active_tasks -= 1;
                                debug!("Task completed with result: {result:?}");
                            }
                        }

                        let session = self.clone();
                        let discovery_tx = peer_discovery_tx.clone();
                        let done_tx = task_done_tx.clone();

                        active_tasks += 1;

                        // Note: This is why PeerConnection methods need desugared syntax with + Send bounds.
                        // The process() method is called within a spawned task, so all futures it awaits
                        // (including those from connection.peer() and connection.get_peers()) must be Send.
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

                    if active_tasks == 0 {
                        // Having separate channels for peer discovery and task completion
                        // allows for easy to reason about code, as well as avoiding
                        // re-queueing in the "normal" state of max concurrency. But it
                        // does introduce a race condition where a task done event is acted
                        // upon before its peers are processed. This check re-queues those
                        // peers if that happened and continues the processing loop.
                        if matches!(result, TaskResult::FoundPeers) {
                            if let Ok(peers) = peer_discovery_rx.try_recv() {
                                debug!("Re-queuing peers after task completion");
                                peer_discovery_tx.send(peers).expect("Cooridinator should always have receiever of channel");
                                continue;
                            }
                        }

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
    use crate::connection::test_utils::{MockConnector, MockPeerConnection};
    use bitcoin::p2p::address::AddrV2;
    use bitcoin_peers_connection::ConnectionError;
    use std::net::Ipv4Addr;
    use tokio::sync::mpsc;

    fn create_test_session() -> (CrawlSession<MockConnector>, mpsc::Receiver<CrawlerMessage>) {
        let (crawl_tx, crawl_rx) = mpsc::channel(10);
        let config = SessionConfig {
            max_concurrent_tasks: 8,
            peer_timeout: Duration::from_millis(50),
        };
        let connector = MockConnector::new();
        let session = CrawlSession::new(config, crawl_tx, connector);
        (session, crawl_rx)
    }

    #[tokio::test]
    async fn test_process_with_successful_connection() {
        let (session, mut crawl_rx) = create_test_session();
        let (discovery_tx, mut discovery_rx) = mpsc::unbounded_channel();

        // Create a mock connection with pre-configured peer addresses
        let mut mock_conn = MockPeerConnection::new();
        mock_conn.add_addr_message(vec![
            (
                AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1)),
                8333,
                bitcoin::p2p::ServiceFlags::NETWORK,
            ),
            (
                AddrV2::Ipv4(Ipv4Addr::new(10, 0, 0, 1)),
                8333,
                bitcoin::p2p::ServiceFlags::NETWORK | bitcoin::p2p::ServiceFlags::WITNESS,
            ),
        ]);

        // Configure the connector to return our mock connection
        session.connector.add_connection(mock_conn);

        // Test the process method with a peer
        let test_peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);
        let result = session.process(test_peer, discovery_tx).await;

        // Verify the result
        assert_eq!(result, TaskResult::FoundPeers);

        // Verify that a Listening message was sent
        let crawl_message = crawl_rx
            .try_recv()
            .expect("Should have received a crawler message");
        match crawl_message {
            CrawlerMessage::Listening(peer) => {
                assert_eq!(peer.address, AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)));
                assert_eq!(peer.port, 8333);
            }
            CrawlerMessage::NonListening(_) => panic!("Expected Listening message"),
        }

        // Verify that discovered peers were sent
        let discovered_peers = discovery_rx
            .try_recv()
            .expect("Should have received discovered peers");
        assert_eq!(discovered_peers.len(), 2);

        // Check the first discovered peer
        assert_eq!(
            discovered_peers[0].address,
            AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1))
        );
        assert_eq!(discovered_peers[0].port, 8333);
        assert!(discovered_peers[0].has_service(bitcoin::p2p::ServiceFlags::NETWORK));

        // Check the second discovered peer
        assert_eq!(
            discovered_peers[1].address,
            AddrV2::Ipv4(Ipv4Addr::new(10, 0, 0, 1))
        );
        assert_eq!(discovered_peers[1].port, 8333);
        assert!(discovered_peers[1].has_service(bitcoin::p2p::ServiceFlags::WITNESS));
    }

    #[tokio::test]
    async fn test_process_with_no_peers_found() {
        let (session, mut crawl_rx) = create_test_session();
        let (discovery_tx, mut discovery_rx) = mpsc::unbounded_channel();

        // Create a mock connection that returns no peers
        let mock_conn = MockPeerConnection::new();
        // Don't add any address messages - should timeout and return empty

        // Configure the connector to return our mock connection
        session.connector.add_connection(mock_conn);

        // Test the process method with a peer
        let test_peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);
        let result = session.process(test_peer, discovery_tx).await;

        // Should get NoPeersFound since no addresses were added to the mock
        assert_eq!(result, TaskResult::NoPeersFound);

        // Should still send a Listening message
        let crawl_message = crawl_rx
            .try_recv()
            .expect("Should have received a crawler message");
        match crawl_message {
            CrawlerMessage::Listening(peer) => {
                assert_eq!(peer.address, AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)));
            }
            CrawlerMessage::NonListening(_) => panic!("Expected Listening message"),
        }

        // Should not send any discovered peers
        assert!(discovery_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_process_with_connection_error() {
        let (session, mut crawl_rx) = create_test_session();
        let (discovery_tx, mut discovery_rx) = mpsc::unbounded_channel();

        // Create a mock connection that returns an error when getting peers
        let mut mock_conn = MockPeerConnection::new();
        mock_conn.add_incoming_error(ConnectionError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "Connection lost",
        )));

        // Configure the connector to return our mock connection
        session.connector.add_connection(mock_conn);

        // Test the process method with a peer
        let test_peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);
        let result = session.process(test_peer, discovery_tx).await;

        // Should get NoPeersFound when connection fails during get_peers
        assert_eq!(result, TaskResult::NoPeersFound);

        // Should still send a Listening message (connection was established)
        let crawl_message = crawl_rx
            .try_recv()
            .expect("Should have received a crawler message");
        match crawl_message {
            CrawlerMessage::Listening(_) => {} // Expected
            CrawlerMessage::NonListening(_) => panic!("Expected Listening message"),
        }

        // Should not send any discovered peers
        assert!(discovery_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_process_with_connection_failure() {
        let (session, mut crawl_rx) = create_test_session();
        let (discovery_tx, mut discovery_rx) = mpsc::unbounded_channel();

        // Don't add any mock connections - connector will fail

        // Test the process method with a peer
        let test_peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);
        let result = session.process(test_peer.clone(), discovery_tx).await;

        // Should get ConnectionFailed when connector fails
        assert_eq!(result, TaskResult::ConnectionFailed);

        // Should send a NonListening message
        let crawl_message = crawl_rx
            .try_recv()
            .expect("Should have received a crawler message");
        match crawl_message {
            CrawlerMessage::NonListening(peer) => {
                assert_eq!(peer.address, test_peer.address);
                assert_eq!(peer.port, test_peer.port);
            }
            CrawlerMessage::Listening(_) => panic!("Expected NonListening message"),
        }

        // Should not send any discovered peers
        assert!(discovery_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_coordinate_terminates_with_circular_references() {
        // Initialize logger for this test.
        let _ = fern::Dispatch::new()
            .format(|out, message, record| {
                out.finish(format_args!(
                    "[{}] {} - {}",
                    record.level(),
                    record.target(),
                    message
                ))
            })
            .level(log::LevelFilter::Debug)
            .chain(std::io::stderr())
            .apply();

        let (session, mut crawl_rx) = create_test_session();

        // Create three peers that will reference each other in a circle.
        let peer_a = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1)), 8333);
        let peer_b = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 2)), 8333);
        let peer_c = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 3)), 8333);

        // Mock connection for peer A that returns peer B and C.
        let mut mock_conn_a = MockPeerConnection::new();
        mock_conn_a.peer_info = peer_a.clone();
        mock_conn_a.add_addr_message(vec![
            (
                peer_b.address.clone(),
                peer_b.port,
                bitcoin::p2p::ServiceFlags::NETWORK,
            ),
            (
                peer_c.address.clone(),
                peer_c.port,
                bitcoin::p2p::ServiceFlags::NETWORK,
            ),
        ]);

        // Mock connection for peer B that returns peer A and C (circular reference).
        let mut mock_conn_b = MockPeerConnection::new();
        mock_conn_b.peer_info = peer_b.clone();
        mock_conn_b.add_addr_message(vec![
            (
                peer_a.address.clone(),
                peer_a.port,
                bitcoin::p2p::ServiceFlags::NETWORK,
            ),
            (
                peer_c.address.clone(),
                peer_c.port,
                bitcoin::p2p::ServiceFlags::NETWORK,
            ),
        ]);

        // Mock connection for peer C that returns peer A and B (circular reference).
        let mut mock_conn_c = MockPeerConnection::new();
        mock_conn_c.peer_info = peer_c.clone();
        mock_conn_c.add_addr_message(vec![
            (
                peer_a.address.clone(),
                peer_a.port,
                bitcoin::p2p::ServiceFlags::NETWORK,
            ),
            (
                peer_b.address.clone(),
                peer_b.port,
                bitcoin::p2p::ServiceFlags::NETWORK,
            ),
        ]);

        // Add connections to the connector in the order they'll be requested, A is the seed.
        session.connector.add_connection(mock_conn_a);
        session.connector.add_connection(mock_conn_b);
        session.connector.add_connection(mock_conn_c);

        let mut seen_peers = HashSet::new();

        // Run coordinate in the background - move session to drop it when done.
        let peer_a_clone = peer_a.clone();
        tokio::spawn(async move {
            session.coordinate(peer_a_clone).await;
        });

        loop {
            match crawl_rx.recv().await {
                Some(CrawlerMessage::Listening(peer)) => {
                    seen_peers.insert((peer.address, peer.port));
                }
                Some(CrawlerMessage::NonListening(_)) => {
                    // Ignore non-listening messages.
                }
                None => {
                    break;
                }
            }
        }

        assert_eq!(seen_peers.len(), 3, "Should have seen 3 unique peers");

        // Verify all three peers were discovered.
        assert!(seen_peers.contains(&(peer_a.address, peer_a.port)));
        assert!(seen_peers.contains(&(peer_b.address, peer_b.port)));
        assert!(seen_peers.contains(&(peer_c.address, peer_c.port)));
    }
}
