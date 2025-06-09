//! Bitcoin protocol transport implementations and tools.
//!
//! This module provides transport implementations for bitcoin network protocols:
//!
//! * v1 - Traditional plaintext bitcoin protocol.
//! * v2 - Encrypted protocol specified in BIP-324.
//!
//! The [`Transport`] struct provides a unified interface that abstracts over these
//! protocol implementations, allowing higher-level code to work with either transport
//! transparently.
//!
//! # Example
//!
//! ```rust
//! use bitcoin::Network;
//! use bitcoin::p2p::message::NetworkMessage;
//! use bitcoin_peers::Transport;
//! use tokio::net::TcpStream;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Connect to a bitcoin node
//! let mut stream = TcpStream::connect("127.0.0.1:8333").await?;
//!
//! // Create a V1 transport for the Bitcoin mainnet
//! let mut transport = Transport::v1(Network::Bitcoin.magic());
//!
//! // Send a ping message
//! let ping_message = NetworkMessage::Ping(42);
//! transport.send(ping_message, &mut stream).await?;
//!
//! // Receive a response
//! let response = transport.receive(&mut stream).await?;
//!
//! // Handle the response
//! match response {
//!     NetworkMessage::Pong(nonce) => println!("Received pong with nonce: {}", nonce),
//!     _ => println!("Received other message: {:?}", response),
//! }
//! # Ok(())
//! # }
//! ```
//!
//! For more advanced use cases, the transport can be split into separate receiver and sender
//! components using the `split()` method, allowing for independent reading and writing operations.
//!
//! # Design
//!
//! This module is a bit of an exercise in type design. The lower level structs,
//! [`AsyncV1Transport `] and [`AsyncV2Transport`], handle all the bespoke
//! serialization and encryption per version. The higher level enum [`Transport`]
//! provides a single interface across both for callers with static dispatch.
//!
//! Giving the transport the ability to split into send and receive halves adds
//! boilerplate at each level, but delegation is maintained over duplicating
//! logic.

mod v1;
mod v2;

pub use v1::{AsyncV1Transport, AsyncV1TransportReceiver, AsyncV1TransportSender};
pub use v2::{AsyncV2Transport, AsyncV2TransportReceiver, AsyncV2TransportSender};

use bip324::AsyncProtocol;
use bitcoin::consensus::encode;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::Magic;
use std::fmt;
use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

/// Error types specific to the transport layer.
#[derive(Debug)]
pub enum TransportError {
    /// IO error during read/write operations.
    Io(io::Error),
    /// Failed to deserialize a message.
    Deserialize(encode::Error),
    /// Network magic in the message doesn't match the expected value.
    MagicMismatch,
    /// v2 encryption failed.
    Encryption,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Io(e) => write!(f, "IO error: {e}"),
            TransportError::Deserialize(e) => write!(f, "Message deserialization error: {e}"),
            TransportError::MagicMismatch => write!(f, "Network magic mismatch"),
            TransportError::Encryption => write!(f, "BIP324 encryption/decryption error"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TransportError::Io(e) => Some(e),
            TransportError::Deserialize(e) => Some(e),
            TransportError::MagicMismatch => None,
            TransportError::Encryption => None,
        }
    }
}

impl From<io::Error> for TransportError {
    fn from(e: io::Error) -> Self {
        TransportError::Io(e)
    }
}

impl From<encode::Error> for TransportError {
    fn from(e: encode::Error) -> Self {
        TransportError::Deserialize(e)
    }
}

/// This type handles receiving Bitcoin network messages using the appropriate
/// protocol (v1 or v2).
pub enum TransportReceiver {
    /// V1 protocol with plaintext messages receiver.
    V1(AsyncV1TransportReceiver),
    /// V2 protocol using BIP-324 encrypted transport receiver.
    V2(AsyncV2TransportReceiver),
}

/// Sender half of a split transport.
///
/// This type handles sending Bitcoin network messages using the appropriate
/// protocol (V1 or V2).
pub enum TransportSender {
    /// V1 protocol with plaintext messages sender.
    V1(AsyncV1TransportSender),
    /// V2 protocol using BIP-324 encrypted transport sender.
    V2(AsyncV2TransportSender),
}

impl TransportReceiver {
    /// Receive a Bitcoin network message from the transport.
    ///
    /// This method handles all the protocol-specific details for receiving a message,
    /// including reading, parsing, decryption (for V2), and deserialization.
    ///
    /// # Arguments
    ///
    /// * `reader` - Any readable stream implementing AsyncRead
    ///
    /// # Returns
    ///
    /// * `Ok(NetworkMessage)` - Successfully received and parsed message
    /// * `Err(TransportError)` - Error occurred during reception
    ///
    /// # Errors
    ///
    /// This method can fail if:
    /// * There's an underlying I/O error (including connection closed)
    /// * Message deserialization fails (invalid or corrupted message)
    /// * Network magic doesn't match (V1 only)
    /// * Decryption fails (V2 only)
    ///
    /// # Cancellation Safety
    ///
    /// This method is cancellation safe. If it's used with `tokio::select!` and
    /// another branch completes first, the read operation can be safely resumed later
    /// without data loss or protocol corruption.
    pub async fn receive<R>(&mut self, reader: &mut R) -> Result<NetworkMessage, TransportError>
    where
        R: AsyncRead + Unpin + Send,
    {
        match self {
            TransportReceiver::V1(v1) => v1.receive(reader).await,
            TransportReceiver::V2(v2) => v2.receive(reader).await,
        }
    }
}

impl TransportSender {
    /// Send a Bitcoin network message through the transport.
    ///
    /// This method handles all the protocol-specific details for sending a message,
    /// including serialization, framing, and encryption (for V2).
    ///
    /// # Arguments
    ///
    /// * `message` - The Bitcoin network message to send
    /// * `writer` - Any writable stream implementing AsyncWrite
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Message was successfully sent
    /// * `Err(TransportError)` - Error occurred during sending
    ///
    /// # Errors
    ///
    /// This method can fail if:
    /// * There's an underlying I/O error
    /// * Serialization fails (rare for valid messages)
    /// * Encryption fails (V2 only)
    pub async fn send<W>(
        &mut self,
        message: NetworkMessage,
        writer: &mut W,
    ) -> Result<(), TransportError>
    where
        W: AsyncWrite + Unpin + Send,
    {
        match self {
            TransportSender::V1(v1) => v1.send(message, writer).await.map_err(TransportError::Io),
            TransportSender::V2(v2) => v2.send(message, writer).await,
        }
    }
}

/// Policy for transport protocol selection.
///
/// This enum controls how the connection handles transport protocol
/// selection between v1 (plaintext) and v2 (encrypted).
#[derive(Debug, Clone, Copy)]
pub enum TransportPolicy {
    /// V2 encrypted transport is required. Connection will fail if v2 cannot be established.
    V2Required,
    /// V2 encrypted transport is preferred, but will fall back to v1 if necessary.
    V2Preferred,
}

/// Represents the transport protocol used for Bitcoin peer-to-peer communication.
///
/// This enum abstracts over the different Bitcoin network protocols,
/// providing a unified interface for sending and receiving messages. It handles
/// all the protocol-specific details like serialization, encryption, and framing.
///
/// * `V1` - The traditional plaintext Bitcoin protocol
/// * `V2` - The encrypted BIP-324 protocol with improved privacy and security
///
/// # Examples
///
/// Using a V1 transport:
///
/// ```rust,no_run
/// # use bitcoin::p2p::Magic;
/// # use bitcoin::p2p::message::NetworkMessage;
/// # use bitcoin_peers::Transport;
/// # use tokio::net::TcpStream;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let mut stream = TcpStream::connect("127.0.0.1:8333").await?;
/// let mut transport = Transport::v1(Magic::BITCOIN);
///
/// // Send and receive messages using the transport
/// transport.send(NetworkMessage::Ping(42), &mut stream).await?;
/// let response = transport.receive(&mut stream).await?;
/// # Ok(())
/// # }
/// ```
///
/// For concurrent operations, the transport can be split:
///
/// ```rust,no_run
/// # use bitcoin::p2p::Magic;
/// # use bitcoin_peers::Transport;
/// # use tokio::net::TcpStream;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let stream = TcpStream::connect("127.0.0.1:8333").await?;
/// let (read_half, write_half) = stream.into_split();
///
/// let transport = Transport::v1(Magic::BITCOIN);
/// let (mut receiver, mut sender) = transport.into_split();
///
/// // Now receiver and sender can be used in separate tasks
/// # Ok(())
/// # }
/// ```
///
/// The high-level [`Connection`](crate::Connection) type manages transport selection
/// automatically based on protocol negotiation.
pub enum Transport {
    /// V1 protocol with plaintext messages.
    V1(AsyncV1Transport),
    /// V2 protocol using BIP-324 encrypted transport.
    V2(AsyncV2Transport),
}

impl Transport {
    /// Create a new V1 transport.
    pub fn v1(network_magic: Magic) -> Self {
        Self::V1(AsyncV1Transport::new(network_magic))
    }

    /// Create a new V2 transport.
    pub fn v2(protocol: AsyncProtocol) -> Self {
        Self::V2(AsyncV2Transport::new(protocol))
    }

    /// Split this transport into separate receiver and sender halves.
    ///
    /// This allows for independent reading and writing operations, which is useful
    /// for concurrent processing in async contexts.
    ///
    /// # Returns
    ///
    /// A tuple containing the receiver and sender halves of the transport.
    pub fn into_split(self) -> (TransportReceiver, TransportSender) {
        match self {
            Self::V1(v1) => {
                let (receiver, sender) = v1.into_split();
                (TransportReceiver::V1(receiver), TransportSender::V1(sender))
            }
            Self::V2(v2) => {
                let (receiver, sender) = v2.into_split();
                (TransportReceiver::V2(receiver), TransportSender::V2(sender))
            }
        }
    }

    /// Send a Bitcoin network message through the transport.
    ///
    /// This method handles all the protocol-specific details for sending a message,
    /// including serialization, framing, and encryption (for V2).
    ///
    /// # Arguments
    ///
    /// * `message` - The Bitcoin network message to send
    /// * `writer` - Any writable stream implementing AsyncWrite
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Message was successfully sent
    /// * `Err(TransportError)` - Error occurred during sending
    ///
    /// # Errors
    ///
    /// This method can fail if:
    /// * There's an underlying I/O error
    /// * Serialization fails (rare for valid messages)
    /// * Encryption fails (V2 only)
    pub async fn send<W>(
        &mut self,
        message: NetworkMessage,
        writer: &mut W,
    ) -> Result<(), TransportError>
    where
        W: AsyncWrite + Unpin + Send,
    {
        match self {
            Self::V1(v1) => v1.send(message, writer).await,
            Self::V2(v2) => v2.send(message, writer).await,
        }
    }

    /// Receive a Bitcoin network message from the transport.
    ///
    /// This method handles all the protocol-specific details for receiving a message,
    /// including reading, parsing, decryption (for V2), and deserialization.
    ///
    /// # Arguments
    ///
    /// * `reader` - Any readable stream implementing AsyncRead
    ///
    /// # Returns
    ///
    /// * `Ok(NetworkMessage)` - Successfully received and parsed message
    /// * `Err(TransportError)` - Error occurred during reception
    ///
    /// # Errors
    ///
    /// This method can fail if:
    /// * There's an underlying I/O error (including connection closed)
    /// * Message deserialization fails (invalid or corrupted message)
    /// * Network magic doesn't match (V1 only)
    /// * Decryption fails (V2 only)
    ///
    /// # Cancellation Safety
    ///
    /// This method is cancellation safe. If it's used with `tokio::select!` and
    /// another branch completes first, the read operation can be safely resumed later
    /// without data loss or protocol corruption.
    pub async fn receive<R>(&mut self, reader: &mut R) -> Result<NetworkMessage, TransportError>
    where
        R: AsyncRead + Unpin + Send,
    {
        match self {
            Self::V1(v1) => v1.receive(reader).await,
            Self::V2(v2) => v2.receive(reader).await,
        }
    }
}

// Add conversion From<AsyncV1Transport> for Transport
impl From<AsyncV1Transport> for Transport {
    fn from(transport: AsyncV1Transport) -> Self {
        Self::V1(transport)
    }
}

// Add conversion From<AsyncProtocol> for Transport
impl From<AsyncProtocol> for Transport {
    fn from(protocol: AsyncProtocol) -> Self {
        Self::V2(AsyncV2Transport::new(protocol))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::p2p::message::NetworkMessage;
    use tokio_test::io::Builder as MockIoBuilder;

    fn create_test_message(network_magic: Magic, payload: NetworkMessage) -> Vec<u8> {
        use bitcoin::consensus::encode;
        use bitcoin::p2p::message::RawNetworkMessage;

        let raw_msg = RawNetworkMessage::new(network_magic, payload);
        encode::serialize(&raw_msg)
    }

    #[tokio::test]
    async fn test_transport_enum_split() {
        // Test the V1 variant of Transport
        let transport = Transport::v1(Magic::BITCOIN);

        let (mut receiver, mut sender) = transport.into_split();

        // Test sending with the sender
        let mut write_buffer = Vec::new();
        let message = NetworkMessage::Ping(42);
        sender
            .send(message.clone(), &mut write_buffer)
            .await
            .unwrap();

        let expected = create_test_message(Magic::BITCOIN, message);
        assert_eq!(write_buffer, expected);

        // Test receiving with the receiver
        let payload = NetworkMessage::Ping(42);
        let message_bytes = create_test_message(Magic::BITCOIN, payload.clone());
        let mut mock_reader = MockIoBuilder::new().read(&message_bytes).build();

        let received = receiver.receive(&mut mock_reader).await.unwrap();
        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Ping message, got {received:?}"),
        }
    }

    #[tokio::test]
    async fn test_transport_delegation() {
        // Test that the Transport properly delegates to its components
        let mut transport = Transport::v1(Magic::BITCOIN);

        // Test sending
        let mut write_buffer = Vec::new();
        let message = NetworkMessage::Ping(42);
        transport
            .send(message.clone(), &mut write_buffer)
            .await
            .unwrap();

        let expected = create_test_message(Magic::BITCOIN, message);
        assert_eq!(write_buffer, expected);

        // Test receiving
        let payload = NetworkMessage::Ping(42);
        let message_bytes = create_test_message(Magic::BITCOIN, payload.clone());
        let mut mock_reader = MockIoBuilder::new().read(&message_bytes).build();

        let received = transport.receive(&mut mock_reader).await.unwrap();
        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Ping message, got {received:?}"),
        }
    }

    #[tokio::test]
    async fn test_transport_from_impl() {
        // Test the From implementation for AsyncV1Transport
        let v1_transport = AsyncV1Transport::new(Magic::BITCOIN);
        let mut transport: Transport = v1_transport.into();

        // Verify it works correctly
        let mut write_buffer = Vec::new();
        let message = NetworkMessage::Ping(42);
        transport
            .send(message.clone(), &mut write_buffer)
            .await
            .unwrap();

        let expected = create_test_message(Magic::BITCOIN, message);
        assert_eq!(write_buffer, expected);

        // Verify the type is correct
        match transport {
            Transport::V1(_) => { /* expected */ }
            Transport::V2(_) => panic!("Expected V1 transport"),
        }
    }

    #[tokio::test]
    async fn test_transport_pattern_matching() {
        // Test that we can pattern match on the Transport enum
        let transport = Transport::v1(Magic::BITCOIN);

        match transport {
            Transport::V1(_) => { /* expected */ }
            Transport::V2(_) => panic!("Expected V1 transport"),
        }

        // We can also use if let for more targeted matching
        let transport = Transport::v1(Magic::BITCOIN);
        if let Transport::V1(_) = transport {
            // This branch should be taken
        } else {
            panic!("Pattern matching failed");
        }
    }
}
