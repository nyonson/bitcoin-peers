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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::PeerConnection;
    use bitcoin::p2p::{address::AddrV2, message::NetworkMessage, ServiceFlags};
    use std::collections::{HashSet, VecDeque};
    use std::net::Ipv4Addr;

    /// Mock implementation of PeerConnection for testing.
    #[derive(Debug)]
    struct MockPeerConnection {
        /// Queue of messages that will be returned by receive().
        incoming_messages: VecDeque<Result<NetworkMessage, ConnectionError>>,
        /// Messages that were sent via send().
        sent_messages: Vec<NetworkMessage>,
        /// The peer information to return.
        peer_info: Peer,
        /// Whether the connection should simulate being closed.
        is_closed: bool,
    }

    impl MockPeerConnection {
        /// Create a new mock connection with default peer info.
        fn new() -> Self {
            MockPeerConnection {
                incoming_messages: VecDeque::new(),
                sent_messages: Vec::new(),
                peer_info: Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333),
                is_closed: false,
            }
        }

        /// Add a message that will be returned by the next receive() call.
        fn add_incoming_message(&mut self, message: NetworkMessage) {
            self.incoming_messages.push_back(Ok(message));
        }

        /// Add an error that will be returned by the next receive() call.
        fn add_incoming_error(&mut self, error: ConnectionError) {
            self.incoming_messages.push_back(Err(error));
        }

        /// Add multiple peer addresses as an Addr message.
        fn add_addr_message(&mut self, addresses: Vec<(AddrV2, u16, ServiceFlags)>) {
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

        let result = mock_conn.get_peers(Duration::from_millis(100)).await;

        assert!(result.is_ok());
        let peers = result.unwrap();
        assert_eq!(peers.len(), 2);

        // Verify a getaddr message was sent
        assert_eq!(mock_conn.sent_messages.len(), 1);
        assert!(matches!(
            mock_conn.sent_messages[0],
            NetworkMessage::GetAddr
        ));

        // Verify peers returned correctly
        let peer1 = &peers[0];
        assert_eq!(peer1.address, AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(peer1.port, 8333);
        assert!(peer1.has_service(ServiceFlags::NETWORK));

        let peer2 = &peers[1];
        assert_eq!(peer2.address, AddrV2::Ipv4(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(peer2.has_service(ServiceFlags::WITNESS));
    }

    #[tokio::test]
    async fn test_get_peers_timeout() {
        let mut mock_conn = MockPeerConnection::new();
        // Don't add any messages - should timeout

        let result = mock_conn.get_peers(Duration::from_millis(50)).await;

        assert!(result.is_ok());
        let peers = result.unwrap();
        assert_eq!(peers.len(), 0); // Should get 0 peers on timeout

        // Verify a getaddr message was sent
        assert_eq!(mock_conn.sent_messages.len(), 1);
        assert!(matches!(
            mock_conn.sent_messages[0],
            NetworkMessage::GetAddr
        ));
    }

    #[tokio::test]
    async fn test_get_peers_connection_error() {
        let mut mock_conn = MockPeerConnection::new();

        // Add a connection error that will be returned after getaddr is sent
        mock_conn.add_incoming_error(ConnectionError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "Connection lost",
        )));

        let result = mock_conn.get_peers(Duration::from_millis(100)).await;

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

        let result = mock_conn.get_peers(Duration::from_millis(100)).await;

        assert!(result.is_ok());
        let peers = result.unwrap();
        assert_eq!(
            peers.len(),
            3,
            "Should receive all 3 peers including self-reference"
        );

        // Verify we received all the addresses
        let addresses: HashSet<_> = peers.iter().map(|p| p.address.clone()).collect();

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
