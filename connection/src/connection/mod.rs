//! Bitcoin p2p protocol connection.
//!
//! This module provides connection handling for the bitcoin peer-to-peer network.
//! It implements the bitcoin p2p protocol, including version handshake, message
//! serialization/deserialization, and protocol-level behaviors like automatic
//! ping-pong responses.
//!
//! # Examples
//!
//! Creating a TCP connection to a bitcoin peer:
//!
//! ```
//! use bitcoin::Network;
//! use bitcoin_peers::{Connection, ConnectionConfiguration, Peer, PeerProtocolVersion};
//! use bitcoin::p2p::address::AddrV2;
//! use bitcoin::p2p::message::NetworkMessage;
//! use std::net::Ipv4Addr;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let peer = Peer::new(
//!     AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)),
//!     8333,
//! );
//!
//! // Configure the connection as non-listening (appropriate for light client software).
//! // Set the required protocol version and use the default user agent.
//! let config = ConnectionConfiguration::non_listening(
//!     PeerProtocolVersion::Known(70016),
//!     None,
//! );
//!
//! // Establish the connection with automatic handshake.
//! let mut connection = Connection::tcp(peer, Network::Bitcoin, config).await?;
//!
//! // Send a getaddr message to request peer addresses
//! connection.send(NetworkMessage::GetAddr).await?;
//!
//! // Receive a response
//! let response = connection.receive().await?;
//! println!("Received: {:?}", response);
//! # Ok(())
//! # }
//! ```
//!
//! The [`Connection`] type is the recommended high-level API for most applications.
//!
//! See more [lower level documentation](https://developer.bitcoin.org/reference/p2p_networking.html) on the bitcoin p2p protocol.

mod configuration;
mod error;
mod handshake;
mod io;
mod state;
mod tcp;

pub use configuration::ConnectionConfiguration;
pub use error::ConnectionError;
pub use io::{AsyncConnection, AsyncConnectionReceiver, AsyncConnectionSender};
pub use tcp::{TcpConnection, TcpConnectionReceiver, TcpConnectionSender};

use crate::peer::Peer;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::Network;

/// Receiver half of a split connection.
#[derive(Debug)]
pub enum ConnectionReceiver {
    /// TCP connection receiver.
    Tcp(TcpConnectionReceiver),
}

impl ConnectionReceiver {
    /// Get a copy of the peer this connection is established with.
    pub async fn peer(&self) -> Peer {
        match self {
            ConnectionReceiver::Tcp(tcp) => tcp.peer().await,
        }
    }

    /// Receive a message from the peer.
    ///
    /// This method handles certain protocol-level messages automatically,
    /// such as updating protocol negotiation state.
    ///
    /// # Returns
    ///
    /// * `Ok(NetworkMessage)` - Successfully received and parsed message
    /// * `Err(ConnectionError)` - Error occurred during reception
    pub async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        match self {
            ConnectionReceiver::Tcp(tcp) => tcp.receive().await,
        }
    }
}

/// Sender half of a split connection.
#[derive(Debug)]
pub enum ConnectionSender {
    /// TCP connection sender.
    Tcp(TcpConnectionSender),
}

impl ConnectionSender {
    /// Get a copy of the peer this connection is established with.
    pub async fn peer(&self) -> Peer {
        match self {
            ConnectionSender::Tcp(tcp) => tcp.peer().await,
        }
    }

    /// Send a message to the peer.
    ///
    /// # Arguments
    ///
    /// * `message` - The Bitcoin network message to send
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Message was successfully sent
    /// * `Err(ConnectionError)` - Error occurred during sending
    pub async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        match self {
            ConnectionSender::Tcp(tcp) => tcp.send(message).await,
        }
    }
}

impl std::fmt::Display for ConnectionReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionReceiver::Tcp(tcp) => tcp.fmt(f),
        }
    }
}

impl std::fmt::Display for ConnectionSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionSender::Tcp(tcp) => tcp.fmt(f),
        }
    }
}

/// Provides a unified interface to different types of bitcoin peer connections.
///
/// This enum is the primary API for interacting with bitcoin peers. It abstracts over
/// different connection transport types (TCP, WebSocket, Tor, etc.) and provides a
/// consistent interface for sending and receiving messages.
///
/// # Trait Implementations
///
/// - **Display**: Provides human-readable connection information including peer address and protocol state.
/// - **Debug**: Provides detailed debug information about the connection.
/// - **Send**: The connection can be safely moved between threads.
///
/// Note that `Connection` does *not* implement `Copy` or `Clone` as it owns I/O resources
/// that cannot be duplicated.
///
/// # Example
///
/// ```
/// # use bitcoin::Network;
/// # use bitcoin_peers::{Connection, ConnectionConfiguration, Peer, PeerProtocolVersion};
/// # use bitcoin::p2p::address::AddrV2;
/// # use std::net::Ipv4Addr;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);
/// let config = ConnectionConfiguration::non_listening(PeerProtocolVersion::Known(70016), None);
/// let connection = Connection::tcp(peer, Network::Bitcoin, config).await?;
///
/// // Display connection info for debugging
/// println!("Connection: {}", connection);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub enum Connection {
    Tcp(TcpConnection),
    // WebSocket(WebSocketConnection),
    // Tor(TorConnection),
    // etc.
}

impl Connection {
    /// Get a copy of the peer this connection is established with.
    pub async fn peer(&self) -> Peer {
        match self {
            Connection::Tcp(conn) => conn.peer().await,
        }
    }

    /// Send a message to the peer.
    ///
    /// # Arguments
    ///
    /// * `message` - The Bitcoin network message to send.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or the specific error that occurred.
    pub async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        match self {
            Connection::Tcp(conn) => conn.send(message).await,
        }
    }

    /// Receive a message from the peer.
    ///
    /// This method handles certain protocol-level messages automatically,
    /// such as responding to pings and updating protocol negotiation state.
    ///
    /// # Returns
    ///
    /// * `Ok(`[`NetworkMessage`]`)` - The received message
    /// * `Err(`[`ConnectionError`]`)` - If an error occurred during message reception
    pub async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        match self {
            Connection::Tcp(conn) => conn.receive().await,
        }
    }

    /// Split this connection into separate receiver and sender halves.
    ///
    /// This allows for independent reading and writing operations, which is useful
    /// for concurrent processing in async contexts.
    ///
    /// # Example
    ///
    /// ```
    /// # use bitcoin::Network;
    /// # use bitcoin_peers::{Connection, ConnectionConfiguration, Peer, PeerProtocolVersion};
    /// # use bitcoin::p2p::address::AddrV2;
    /// # use bitcoin::p2p::message::NetworkMessage;
    /// # use std::net::Ipv4Addr;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let peer = Peer::new(
    ///     AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)),
    ///     8333,
    /// );
    ///
    /// let config = ConnectionConfiguration::non_listening(
    ///     PeerProtocolVersion::Known(70016),
    ///     None,
    /// );
    ///
    /// let connection = Connection::tcp(peer, Network::Bitcoin, config).await?;
    /// let (mut receiver, mut sender) = connection.into_split();
    ///
    /// // Now receiver and sender can be used in separate tasks
    /// tokio::spawn(async move {
    ///     while let Ok(msg) = receiver.receive().await {
    ///         // Handle received messages
    ///         match msg {
    ///             NetworkMessage::Ping(nonce) => {
    ///                 // Must manually handle ping/pong in split mode
    ///             }
    ///             _ => {}
    ///         }
    ///     }
    /// });
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Returns
    ///
    /// A tuple containing the receiver and sender halves of the connection.
    pub fn into_split(self) -> (ConnectionReceiver, ConnectionSender) {
        match self {
            Connection::Tcp(tcp) => {
                let (receiver, sender) = tcp.into_split();
                (
                    ConnectionReceiver::Tcp(receiver),
                    ConnectionSender::Tcp(sender),
                )
            }
        }
    }

    /// Establish a TCP connection to a bitcoin peer and perform the handshake.
    ///
    /// # Arguments
    ///
    /// * `peer` - The bitcoin peer to connect to.
    /// * `network` - The bitcoin [`Network`] to use.
    /// * `configuration` - Configuration for the connection.
    ///
    /// # Returns
    ///
    /// * `Ok(`[`Self`]`)` - A successfully established and handshaked connection
    /// * `Err(`[`ConnectionError`]`)` - If the connection attempt or handshake failed
    pub async fn tcp(
        peer: Peer,
        network: Network,
        configuration: ConnectionConfiguration,
    ) -> Result<Self, ConnectionError> {
        Ok(Connection::Tcp(
            tcp::connect(peer, network, configuration).await?,
        ))
    }
}

impl std::fmt::Display for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Connection::Tcp(tcp) => tcp.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::PeerProtocolVersion;
    use crate::transport::{AsyncV1Transport, Transport};
    use bitcoin::consensus::encode;
    use bitcoin::p2p::address::AddrV2;
    use bitcoin::p2p::message::NetworkMessage;
    use std::net::Ipv4Addr;
    use tokio_test::io::Builder as MockIoBuilder;

    // Helper function to create a test AsyncConnection
    fn create_test_connection<R, W>(
        config: ConnectionConfiguration,
        peer: Peer,
        reader: R,
        writer: W,
    ) -> AsyncConnection<R, W>
    where
        R: tokio::io::AsyncRead + Unpin + Send,
        W: tokio::io::AsyncWrite + Unpin + Send,
    {
        let transport = Transport::V1(AsyncV1Transport::new(bitcoin::p2p::Magic::BITCOIN));
        AsyncConnection::new(peer, config, transport, reader, writer)
    }

    fn create_raw_message(magic: bitcoin::p2p::Magic, message: NetworkMessage) -> Vec<u8> {
        let raw_msg = bitcoin::p2p::message::RawNetworkMessage::new(magic, message);
        encode::serialize(&raw_msg)
    }

    #[tokio::test]
    async fn test_async_connection_receive_message() {
        let pong_message = NetworkMessage::Pong(42);
        let raw_msg = bitcoin::p2p::message::RawNetworkMessage::new(
            bitcoin::p2p::Magic::BITCOIN,
            pong_message.clone(),
        );
        let message_bytes = encode::serialize(&raw_msg);

        let mock_reader = MockIoBuilder::new().read(&message_bytes).build();
        let mock_writer = Vec::new();

        let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);

        let config =
            ConnectionConfiguration::non_listening(PeerProtocolVersion::Known(70016), None);

        let mut connection = create_test_connection(config, peer, mock_reader, mock_writer);

        let received = connection.receive().await.unwrap();

        match received {
            NetworkMessage::Pong(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Pong message, got {received:?}"),
        }
    }

    #[tokio::test]
    async fn test_connection_split() {
        // Test that split connections work correctly
        let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), 8333);
        let config =
            ConnectionConfiguration::non_listening(PeerProtocolVersion::Known(70016), None);

        // Create test messages
        let ping_message = NetworkMessage::Ping(456);
        let ping_bytes = create_raw_message(bitcoin::p2p::Magic::BITCOIN, ping_message);

        let pong_message = NetworkMessage::Pong(789);
        let pong_bytes = create_raw_message(bitcoin::p2p::Magic::BITCOIN, pong_message);

        // Set up mock reader/writer
        let mock_reader = MockIoBuilder::new()
            .read(&ping_bytes)
            .read(&pong_bytes)
            .build();
        let mock_writer = Vec::new();

        let connection = create_test_connection(config, peer, mock_reader, mock_writer);

        // Split the connection
        let (mut receiver, mut sender) = connection.into_split();

        // Test receiving with the split receiver
        let received = receiver.receive().await.unwrap();
        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 456),
            _ => panic!("Expected Ping message, got {received:?}"),
        }

        // Test that split receiver doesn't automatically respond to pings
        // Now manually send a pong using the sender
        sender.send(NetworkMessage::Pong(456)).await.unwrap();

        // Receive the next message
        let received = receiver.receive().await.unwrap();
        match received {
            NetworkMessage::Pong(nonce) => assert_eq!(nonce, 789),
            _ => panic!("Expected Pong message, got {received:?}"),
        }
    }
}
