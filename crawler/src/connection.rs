//! Internal peer connection abstractions for testing and mocking.
//!
//! This module provides the [`PeerConnection`] trait that abstracts bitcoin peer
//! connections, enabling dependency injection for testing without modifying
//! the core crawler logic.

use bitcoin::p2p::message::NetworkMessage;
use bitcoin_peers_connection::{Connection, ConnectionConfiguration, ConnectionError, Peer};
use log::debug;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// Internal trait for bitcoin peer connections that can send and receive messages.
///
/// This trait abstracts the core operations needed for crawling, allowing
/// for easy testing with mock implementations.
pub trait PeerConnection: Send {
    fn send(
        &mut self,
        message: NetworkMessage,
    ) -> impl std::future::Future<Output = Result<(), ConnectionError>> + Send;
    fn receive(
        &mut self,
    ) -> impl std::future::Future<Output = Result<NetworkMessage, ConnectionError>> + Send;
    fn peer(&self) -> impl std::future::Future<Output = Peer> + Send;

    /// Requests peer addresses from this connection by sending a getaddr message.
    ///
    /// # Arguments
    ///
    /// * `peer_timeout` - Maximum duration to wait for responses.
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Peer>)` - The discovered peer addresses.
    /// * `Err(ConnectionError)` - If an error occurs during the exchange.
    fn get_peers(
        &mut self,
        peer_timeout: Duration,
    ) -> impl std::future::Future<Output = Result<Vec<Peer>, ConnectionError>> + Send {
        async move {
            self.send(NetworkMessage::GetAddr).await?;
            debug!("Sent getaddr message to peer");

            let mut all_peers = Vec::new();
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
                        if !all_peers.is_empty() {
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
                                        bitcoin::p2p::address::AddrV2::Ipv4(ipv4),
                                        socket_addr.port(),
                                        addr.services,
                                    ),
                                    IpAddr::V6(ipv6) => Peer::with_services(
                                        bitcoin::p2p::address::AddrV2::Ipv6(ipv6),
                                        socket_addr.port(),
                                        addr.services,
                                    ),
                                };
                                peers_batch.push(peer);
                            }
                        }
                    }
                    NetworkMessage::AddrV2(addresses) => {
                        debug!("Received {} peer addresses (v2 format)", addresses.len());
                        for addr_msg in addresses {
                            let peer = Peer::with_services(
                                addr_msg.addr,
                                addr_msg.port,
                                addr_msg.services,
                            );
                            peers_batch.push(peer);
                        }
                    }
                    // Handling Ping's just in case it improves odds of getting more addresses.
                    NetworkMessage::Ping(nonce) => {
                        debug!("Received ping during get_peers, responding with pong");
                        self.send(NetworkMessage::Pong(nonce)).await?
                    }
                    _ => {
                        debug!("Received unexpected message in get_peers: {message:?}, ignoring");
                    }
                }

                // Collect all peers from this message
                all_peers.extend(peers_batch);
            }

            debug!(
                "Collected {} peer addresses from {}",
                all_peers.len(),
                self.peer().await
            );
            Ok(all_peers)
        }
    }
}

/// Implementation of PeerConnection for the Connection type from bitcoin-peers-connection.
impl PeerConnection for Connection {
    fn send(
        &mut self,
        message: NetworkMessage,
    ) -> impl std::future::Future<Output = Result<(), ConnectionError>> + Send {
        self.send(message)
    }

    fn receive(
        &mut self,
    ) -> impl std::future::Future<Output = Result<NetworkMessage, ConnectionError>> + Send {
        self.receive()
    }

    fn peer(&self) -> impl std::future::Future<Output = Peer> + Send {
        self.peer()
    }
}

/// Factory trait for creating peer connections.
///
/// This trait enables dependency injection for connection creation,
/// allowing different implementations for production and testing.
pub trait Connector: Clone + Send + Sync + 'static {
    type Connection: PeerConnection + Send;

    /// Create a connection to the specified peer.
    fn connect(
        &self,
        peer: &Peer,
    ) -> impl std::future::Future<Output = Result<Self::Connection, ConnectionError>> + Send;
}

/// Standard connector that creates real TCP connections.
#[derive(Debug, Clone)]
pub struct PeerConnector {
    network: bitcoin::Network,
    config: ConnectionConfiguration,
}

impl PeerConnector {
    /// Create a new connector with the given network and configuration.
    pub fn new(network: bitcoin::Network, config: ConnectionConfiguration) -> Self {
        Self { network, config }
    }
}

impl Connector for PeerConnector {
    type Connection = Connection;

    fn connect(
        &self,
        peer: &Peer,
    ) -> impl std::future::Future<Output = Result<Self::Connection, ConnectionError>> + Send {
        let peer = peer.clone();
        let network = self.network;
        let config = self.config.clone();
        async move { Connection::tcp(peer, network, config).await }
    }
}

#[cfg(test)]
pub mod test_utils {
    //! Test utilities for testing PeerConnection implementations.

    use super::*;
    use std::collections::VecDeque;
    use std::net::Ipv4Addr;
    use std::sync::{Arc, Mutex};

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
                peer_info: Peer::new(
                    bitcoin::p2p::address::AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)),
                    8333,
                ),
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
        pub fn add_addr_message(
            &mut self,
            addresses: Vec<(
                bitcoin::p2p::address::AddrV2,
                u16,
                bitcoin::p2p::ServiceFlags,
            )>,
        ) {
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
                        bitcoin::p2p::address::AddrV2::Ipv4(ipv4) => {
                            std::net::SocketAddr::new(ipv4.into(), port)
                        }
                        bitcoin::p2p::address::AddrV2::Ipv6(ipv6) => {
                            std::net::SocketAddr::new(ipv6.into(), port)
                        }
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

        fn peer(&self) -> impl std::future::Future<Output = Peer> + Send {
            let peer = self.peer_info.clone();
            async move { peer }
        }
    }

    /// Mock connector for testing.
    #[derive(Debug, Clone)]
    pub struct MockConnector {
        connections: Arc<Mutex<VecDeque<MockPeerConnection>>>,
    }

    impl MockConnector {
        /// Create a new mock connector.
        pub fn new() -> Self {
            Self {
                connections: Arc::new(Mutex::new(VecDeque::new())),
            }
        }

        /// Add a connection that will be returned by the next connect() call.
        pub fn add_connection(&self, conn: MockPeerConnection) {
            self.connections.lock().unwrap().push_back(conn);
        }
    }

    impl Connector for MockConnector {
        type Connection = MockPeerConnection;

        fn connect(
            &self,
            _peer: &Peer,
        ) -> impl std::future::Future<Output = Result<Self::Connection, ConnectionError>> + Send
        {
            let connections = self.connections.clone();
            async move {
                connections.lock().unwrap().pop_front().ok_or_else(|| {
                    ConnectionError::Io(std::io::Error::other("No mock connection available"))
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_utils::MockPeerConnection;
    use super::*;
    use bitcoin_peers_connection::ConnectionError;
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn test_get_peers() {
        let mut mock_conn = MockPeerConnection::new();

        // Add some test addresses
        mock_conn.add_addr_message(vec![
            (
                bitcoin::p2p::address::AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1)),
                8333,
                bitcoin::p2p::ServiceFlags::NETWORK,
            ),
            (
                bitcoin::p2p::address::AddrV2::Ipv4(Ipv4Addr::new(10, 0, 0, 1)),
                8333,
                bitcoin::p2p::ServiceFlags::NETWORK | bitcoin::p2p::ServiceFlags::WITNESS,
            ),
        ]);

        let result = mock_conn
            .get_peers(std::time::Duration::from_millis(100))
            .await;

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
        assert_eq!(
            peer1.address,
            bitcoin::p2p::address::AddrV2::Ipv4(Ipv4Addr::new(192, 168, 1, 1))
        );
        assert_eq!(peer1.port, 8333);
        assert!(peer1.has_service(bitcoin::p2p::ServiceFlags::NETWORK));

        let peer2 = &peers[1];
        assert_eq!(
            peer2.address,
            bitcoin::p2p::address::AddrV2::Ipv4(Ipv4Addr::new(10, 0, 0, 1))
        );
        assert!(peer2.has_service(bitcoin::p2p::ServiceFlags::WITNESS));
    }

    #[tokio::test]
    async fn test_get_peers_timeout() {
        let mut mock_conn = MockPeerConnection::new();
        // Don't add any messages - should timeout

        let result = mock_conn
            .get_peers(std::time::Duration::from_millis(50))
            .await;

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

        let result = mock_conn
            .get_peers(std::time::Duration::from_millis(100))
            .await;

        // Should return the error
        assert!(result.is_err());

        // Verify a getaddr message was sent before the error
        assert_eq!(mock_conn.sent_messages.len(), 1);
        assert!(matches!(
            mock_conn.sent_messages[0],
            NetworkMessage::GetAddr
        ));
    }
}
