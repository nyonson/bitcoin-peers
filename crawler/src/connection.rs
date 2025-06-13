//! Internal peer connection abstractions for testing and mocking.
//!
//! This module provides the [`PeerConnection`] trait that abstracts bitcoin peer
//! connections, enabling dependency injection for testing without modifying
//! the core crawler logic.

use bitcoin::p2p::message::NetworkMessage;
use bitcoin_peers_connection::{Connection, ConnectionError, Peer};
use log::debug;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// Internal trait for bitcoin peer connections that can send and receive messages.
///
/// This trait abstracts the core operations needed for crawling, allowing
/// for easy testing with mock implementations.
pub trait PeerConnection {
    async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError>;
    async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError>;
    async fn peer(&self) -> Peer;

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
    async fn get_peers(&mut self, peer_timeout: Duration) -> Result<Vec<Peer>, ConnectionError> {
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
                        let peer =
                            Peer::with_services(addr_msg.addr, addr_msg.port, addr_msg.services);
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
