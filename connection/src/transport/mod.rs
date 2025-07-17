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
//! ```
//! use bitcoin::Network;
//! use bitcoin::p2p::message::NetworkMessage;
//! use bitcoin_peers_connection::transport::Transport;
//! use tokio::net::TcpStream;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Connect to a bitcoin node
//! let stream = TcpStream::connect("127.0.0.1:8333").await?;
//! let (reader, writer) = stream.into_split();
//!
//! // Create a V1 transport for the Bitcoin mainnet
//! let mut transport = Transport::v1(Network::Bitcoin.magic(), reader, writer);
//!
//! // Write a ping message
//! let ping_message = NetworkMessage::Ping(42);
//! transport.write(ping_message).await?;
//!
//! // Read a response
//! let response = transport.read().await?;
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

pub use v1::{AsyncV1Transport, AsyncV1TransportReader, AsyncV1TransportWriter};
pub use v2::{AsyncV2Transport, AsyncV2TransportReader, AsyncV2TransportWriter};

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

/// Reader half of a split transport.
///
/// This type handles receiving bitcoin network messages using the appropriate
/// protocol (v1 or v2).
#[derive(Debug)]
pub enum TransportReader<R> {
    /// V1 protocol with plaintext messages receiver.
    V1(AsyncV1TransportReader<R>),
    /// V2 protocol using BIP-324 encrypted transport receiver.
    V2(AsyncV2TransportReader<R>),
}

/// Writer half of a split transport.
///
/// This type handles sending bitcoin network messages using the appropriate
/// protocol (V1 or V2).
#[derive(Debug)]
pub enum TransportWriter<W> {
    /// V1 protocol with plaintext messages sender.
    V1(AsyncV1TransportWriter<W>),
    /// V2 protocol using BIP-324 encrypted transport sender.
    V2(AsyncV2TransportWriter<W>),
}

impl<R> TransportReader<R>
where
    R: AsyncRead + Unpin + Send,
{
    /// Read a bitcoin network message from the transport.
    ///
    /// This method handles all the protocol-specific details for receiving a message,
    /// including reading, parsing, decryption (for V2), and deserialization.
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
    pub async fn read(&mut self) -> Result<NetworkMessage, TransportError> {
        match self {
            TransportReader::V1(v1) => v1.read().await,
            TransportReader::V2(v2) => v2.read().await,
        }
    }
}

impl<W> TransportWriter<W>
where
    W: AsyncWrite + Unpin + Send,
{
    /// Write a bitcoin network message through the transport.
    ///
    /// This method handles all the protocol-specific details for sending a message,
    /// including serialization, framing, and encryption (for V2).
    ///
    /// # Arguments
    ///
    /// * `message` - The Bitcoin network message to send
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
    pub async fn write(&mut self, message: NetworkMessage) -> Result<(), TransportError> {
        match self {
            TransportWriter::V1(v1) => v1.write(message).await.map_err(TransportError::Io),
            TransportWriter::V2(v2) => v2.write(message).await,
        }
    }
}

/// Represents the transport protocol used for bitcoin p2p communication.
///
/// This enum abstracts over the different bitcoin network protocols,
/// providing a unified interface for sending and receiving messages. It handles
/// all the protocol-specific details like serialization, encryption, and framing.
///
/// * `V1` - The traditional plaintext bitcoin protocol
/// * `V2` - The encrypted BIP-324 protocol with improved privacy and security
///
/// # Examples
///
/// Using a V1 transport:
///
/// ```
/// # use bitcoin::p2p::Magic;
/// # use bitcoin::p2p::message::NetworkMessage;
/// # use bitcoin_peers_connection::transport::Transport;
/// # use tokio::net::TcpStream;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let stream = TcpStream::connect("127.0.0.1:8333").await?;
/// let (reader, writer) = stream.into_split();
/// let mut transport = Transport::v1(Magic::BITCOIN, reader, writer);
///
/// // Send and receive messages using the transport
/// transport.write(NetworkMessage::Ping(42)).await?;
/// let response = transport.read().await?;
/// # Ok(())
/// # }
/// ```
///
/// For concurrent operations, the transport can be split:
///
/// ```rust,no_run
/// # use bitcoin::p2p::Magic;
/// # use bitcoin_peers_connection::transport::Transport;
/// # use tokio::net::TcpStream;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let stream = TcpStream::connect("127.0.0.1:8333").await?;
/// let (reader, writer) = stream.into_split();
///
/// let transport = Transport::v1(Magic::BITCOIN, reader, writer);
/// let (mut receiver, mut sender) = transport.into_split();
///
/// // Now receiver and sender can be used in separate tasks
/// # Ok(())
/// # }
/// ```
///
/// The high-level [`Connection`](crate::futures::Connection) type manages transport selection
/// automatically based on protocol negotiation.
#[derive(Debug)]
pub enum Transport<R, W> {
    /// V1 protocol with plaintext messages.
    V1(AsyncV1Transport<R, W>),
    /// V2 protocol using BIP-324 encrypted transport.
    V2(AsyncV2Transport<R, W>),
}

impl<R, W> Transport<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Create a new V1 transport.
    pub fn v1(network_magic: Magic, reader: R, writer: W) -> Self {
        Self::V1(AsyncV1Transport::new(network_magic, reader, writer))
    }

    /// Create a new V2 transport.
    pub fn v2(protocol: bip324::futures::Protocol<R, W>) -> Self {
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
    pub fn into_split(self) -> (TransportReader<R>, TransportWriter<W>) {
        match self {
            Self::V1(v1) => {
                let (receiver, sender) = v1.into_split();
                (TransportReader::V1(receiver), TransportWriter::V1(sender))
            }
            Self::V2(v2) => {
                let (receiver, sender) = v2.into_split();
                (TransportReader::V2(receiver), TransportWriter::V2(sender))
            }
        }
    }

    /// Write a Bitcoin network message through the transport.
    ///
    /// This method handles all the protocol-specific details for sending a message,
    /// including serialization, framing, and encryption (for V2).
    ///
    /// # Arguments
    ///
    /// * `message` - The Bitcoin network message to send
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
    pub async fn write(&mut self, message: NetworkMessage) -> Result<(), TransportError> {
        match self {
            Self::V1(v1) => v1.write(message).await,
            Self::V2(v2) => v2.write(message).await,
        }
    }

    /// Read a Bitcoin network message from the transport.
    ///
    /// This method handles all the protocol-specific details for receiving a message,
    /// including reading, parsing, decryption (for V2), and deserialization.
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
    pub async fn read(&mut self) -> Result<NetworkMessage, TransportError> {
        match self {
            Self::V1(v1) => v1.read().await,
            Self::V2(v2) => v2.read().await,
        }
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
        let message_bytes = create_test_message(Magic::BITCOIN, NetworkMessage::Ping(42));
        let mock_reader = MockIoBuilder::new().read(&message_bytes).build();
        let mock_writer = Vec::new();

        let transport = Transport::v1(Magic::BITCOIN, mock_reader, mock_writer);
        let (mut receiver, mut sender) = transport.into_split();

        // Test reading with the receiver
        let received = receiver.read().await.unwrap();
        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Ping message, got {received:?}"),
        }

        // Test writing with the sender
        let message = NetworkMessage::Ping(42);
        sender.write(message.clone()).await.unwrap();
    }

    #[tokio::test]
    async fn test_transport_delegation() {
        // Test that the Transport properly delegates to its components
        let message_bytes = create_test_message(Magic::BITCOIN, NetworkMessage::Ping(42));
        let mock_reader = MockIoBuilder::new().read(&message_bytes).build();
        let mock_writer = Vec::new();

        let mut transport = Transport::v1(Magic::BITCOIN, mock_reader, mock_writer);

        // Test reading
        let received = transport.read().await.unwrap();
        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Ping message, got {received:?}"),
        }

        // Test writing
        let message = NetworkMessage::Ping(42);
        transport.write(message.clone()).await.unwrap();
    }

    #[tokio::test]
    async fn test_transport_from_impl() {
        // Test the From implementation for AsyncV1Transport
        // Note: We can't test the From impl anymore since Transport now takes ownership
        // of reader/writer in the constructor. Let's test the creation instead.
        let mock_reader = MockIoBuilder::new().build();
        let mock_writer = Vec::new();

        let transport = Transport::v1(Magic::BITCOIN, mock_reader, mock_writer);

        // Verify the type is correct
        match transport {
            Transport::V1(..) => { /* expected */ }
            Transport::V2(..) => panic!("Expected V1 transport"),
        }
    }

    #[tokio::test]
    async fn test_transport_pattern_matching() {
        // Test that we can pattern match on the Transport enum
        let mock_reader = MockIoBuilder::new().build();
        let mock_writer = Vec::new();

        let transport = Transport::v1(Magic::BITCOIN, mock_reader, mock_writer);

        match transport {
            Transport::V1(..) => { /* expected */ }
            Transport::V2(..) => panic!("Expected V1 transport"),
        }

        // We can also use if let for more targeted matching
        let mock_reader2 = MockIoBuilder::new().build();
        let mock_writer2 = Vec::new();
        let transport = Transport::v1(Magic::BITCOIN, mock_reader2, mock_writer2);
        if let Transport::V1(..) = transport {
            // This branch should be taken
        } else {
            panic!("Pattern matching failed");
        }
    }
}
