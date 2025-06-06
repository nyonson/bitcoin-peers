//! Bitcoin protocol transport implementations and tools.
//!
//! This module provides transport implementations for Bitcoin network protocols:
//! - V1 protocol: The traditional plaintext Bitcoin protocol
//! - V2 protocol: The encrypted protocol specified in BIP-324
//!
//! The [`Transport`] enum provides a unified interface that abstracts over these
//! protocol implementations, allowing higher-level code to work with either transport
//! transparently.
//!
//! # Example
//!
//! ```rust
//! use bitcoin::Network;
//! use bitcoin::p2p::message::NetworkMessage;
//! use bitcoin_peers::{Transport, AsyncV1Transport};
//! use tokio::net::TcpStream;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Connect to a bitcoin node
//! let mut stream = TcpStream::connect("127.0.0.1:8333").await?;
//!
//! // Create a V1 transport for the Bitcoin mainnet
//! let v1_transport = AsyncV1Transport::new(Network::Bitcoin.magic());
//! 
//! // Wrap it in the Transport enum
//! let mut transport = Transport::V1(v1_transport);
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
//! The transport layer handles all the protocol-specific details including serialization,
//! deserialization, encryption (for V2), and network message framing.

use bip324::AsyncProtocol;
use bitcoin::consensus::encode;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::Magic;
use std::fmt;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Size of a Bitcoin message header in bytes.
const HEADER_SIZE: usize = 24;
/// Offset in the header where the payload length is stored.
const PAYLOAD_LENGTH_OFFSET: usize = 16;

/// Error types specific to the transport layer.
#[derive(Debug)]
pub enum TransportError {
    /// IO error during read/write operations.
    Io(io::Error),
    /// Failed to deserialize a message.
    Deserialize(encode::Error),
    /// Network magic in the message doesn't match the expected value.
    MagicMismatch,
    /// V2 encryption or decryption failed.
    V2,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Io(e) => write!(f, "IO error: {e}"),
            TransportError::Deserialize(e) => write!(f, "Message deserialization error: {e}"),
            TransportError::MagicMismatch => write!(f, "Network magic mismatch"),
            TransportError::V2 => write!(f, "BIP324 encryption/decryption error"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TransportError::Io(e) => Some(e),
            TransportError::Deserialize(e) => Some(e),
            TransportError::MagicMismatch => None,
            TransportError::V2 => None,
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

/// State machine for the [`AsyncV1Transport`] receive method.
/// This makes the receive method cancellation safe by tracking
/// progress across potential interruptions.
///
/// The state machine allows the `receive` method to be interrupted (e.g., by tokio::select!)
/// and resume from where it left off when called again, without losing any partially read data.
#[derive(Debug)]
enum V1ReceiveState {
    /// Reading the message header (24 bytes)
    ReadingHeader {
        header: [u8; HEADER_SIZE],
        bytes_read: usize,
    },
    /// Reading the message payload.
    ReadingPayload {
        /// Complete buffer including header and payload.
        buffer: Vec<u8>,
        bytes_read: usize,
    },
}

impl V1ReceiveState {
    /// Initialize a new state for reading the header.
    fn reading_header() -> Self {
        V1ReceiveState::ReadingHeader {
            header: [0u8; HEADER_SIZE],
            bytes_read: 0,
        }
    }

    /// Transition to reading the payload.
    fn reading_payload(header: [u8; HEADER_SIZE], payload_len: usize) -> Self {
        let mut buffer = Vec::with_capacity(HEADER_SIZE + payload_len);
        // This extend resize dance is just an efficient way to initialize the memory.
        buffer.extend_from_slice(&header);
        buffer.resize(HEADER_SIZE + payload_len, 0);

        V1ReceiveState::ReadingPayload {
            buffer,
            bytes_read: HEADER_SIZE,
        }
    }
}

/// Implements the bitcoin V1 protocol transport.
///
/// This transport provides methods to send and receive Bitcoin protocol messages
/// over any type that implements `AsyncRead` and `AsyncWrite`.
///
/// # Examples
///
/// Basic usage with a TCP connection:
///
/// ```rust
/// use bitcoin::p2p::Magic;
/// use bitcoin::p2p::message::NetworkMessage;
/// use bitcoin_peers::AsyncV1Transport;
/// use tokio::net::TcpStream;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // Connect to a bitcoin node.
/// let mut stream = TcpStream::connect("127.0.0.1:8333").await?;
///
/// // Create a transport for mainnet.
/// let mut transport = AsyncV1Transport::new(Magic::BITCOIN);
///
/// // Send a message.
/// let ping_message = NetworkMessage::Ping(42);
/// transport.send(ping_message, &mut stream).await?;
///
/// // Receive a response.
/// let response = transport.receive(&mut stream).await?;
///
/// // Handle the response.
/// match response {
///     NetworkMessage::Pong(nonce) => println!("Received pong with nonce: {}", nonce),
///     _ => println!("Received other message: {:?}", response),
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct AsyncV1Transport {
    /// The bitcoin network magic bytes.
    network_magic: Magic,
    /// Current state of the receive operation.
    receive_state: V1ReceiveState,
}

impl AsyncV1Transport {
    /// Create a new [`AsyncV1Transport`] for the specified network magic.
    pub fn new(network_magic: Magic) -> Self {
        Self {
            network_magic,
            receive_state: V1ReceiveState::reading_header(),
        }
    }

    /// Receives a bitcoin network message from the reader.
    ///
    /// This function is cancellation safe, meaning it can be safely used with `tokio::select!`
    /// and similar constructs without the risk of leaving the reader in an inconsistent state.
    pub async fn receive<R>(&mut self, reader: &mut R) -> Result<NetworkMessage, TransportError>
    where
        R: AsyncRead + Unpin + Send,
    {
        loop {
            match &mut self.receive_state {
                V1ReceiveState::ReadingHeader { header, bytes_read } => {
                    while *bytes_read < HEADER_SIZE {
                        let n = reader.read(&mut header[*bytes_read..]).await?;
                        if n == 0 {
                            return Err(TransportError::Io(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "connection closed while reading header",
                            )));
                        }
                        *bytes_read += n;
                    }

                    let payload_len = u32::from_le_bytes([
                        header[PAYLOAD_LENGTH_OFFSET],
                        header[PAYLOAD_LENGTH_OFFSET + 1],
                        header[PAYLOAD_LENGTH_OFFSET + 2],
                        header[PAYLOAD_LENGTH_OFFSET + 3],
                    ]) as usize;

                    self.receive_state = V1ReceiveState::reading_payload(*header, payload_len);
                }

                V1ReceiveState::ReadingPayload { buffer, bytes_read } => {
                    while *bytes_read < buffer.len() {
                        let n = reader.read(&mut buffer[*bytes_read..]).await?;
                        if n == 0 {
                            return Err(TransportError::Io(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "connection closed while reading payload",
                            )));
                        }
                        *bytes_read += n;
                    }

                    let raw_msg: RawNetworkMessage = encode::deserialize(buffer)?;
                    if raw_msg.magic() != &self.network_magic {
                        return Err(TransportError::MagicMismatch);
                    }

                    self.receive_state = V1ReceiveState::reading_header();
                    return Ok(raw_msg.payload().clone());
                }
            }
        }
    }

    /// Sends a bitcoin network message to the writer.
    pub async fn send<W>(&self, message: NetworkMessage, writer: &mut W) -> Result<(), io::Error>
    where
        W: AsyncWrite + Unpin + Send,
    {
        let raw_msg = RawNetworkMessage::new(self.network_magic, message);
        let data = encode::serialize(&raw_msg);

        writer.write_all(&data).await?;
        writer.flush().await?;

        Ok(())
    }
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
/// # use bitcoin_peers::{Transport, AsyncV1Transport};
/// # use tokio::net::TcpStream;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let mut stream = TcpStream::connect("127.0.0.1:8333").await?;
/// let v1 = AsyncV1Transport::new(Magic::BITCOIN);
/// let mut transport = Transport::V1(v1);
///
/// // Send and receive messages using the transport
/// transport.send(NetworkMessage::Ping(42), &mut stream).await?;
/// let response = transport.receive(&mut stream).await?;
/// # Ok(())
/// # }
/// ```
///
/// The high-level [`Connection`](crate::Connection) type manages transport selection
/// automatically based on protocol negotiation.
pub enum Transport {
    /// V2 protocol using BIP-324 encrypted transport.
    V2(AsyncProtocol),
    /// V1 protocol with plaintext messages.
    V1(AsyncV1Transport),
}

impl Transport {
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
            Transport::V2(v2) => {
                let data = bip324::serde::serialize(message).expect("infallible");

                v2.writer()
                    .encrypt_and_write(&data, writer)
                    .await
                    .map_err(|_| TransportError::V2)
            }
            Transport::V1(v1) => v1.send(message, writer).await.map_err(TransportError::Io),
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
            Transport::V2(v2) => {
                let message = v2
                    .reader()
                    .read_and_decrypt(reader)
                    .await
                    .map_err(|_| TransportError::V2)?;

                bip324::serde::deserialize(message.contents()).map_err(|_| TransportError::V2)
            }
            Transport::V1(v1) => v1.receive(reader).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::p2p::message::NetworkMessage;
    use tokio_test::io::Builder as MockIoBuilder;

    fn create_test_message(network_magic: Magic, payload: NetworkMessage) -> Vec<u8> {
        let raw_msg = RawNetworkMessage::new(network_magic, payload);
        encode::serialize(&raw_msg)
    }

    #[tokio::test]
    async fn test_basic_message_receive() {
        let payload = NetworkMessage::Ping(42);
        let message_bytes = create_test_message(Magic::BITCOIN, payload.clone());
        let mut mock_reader = MockIoBuilder::new().read(&message_bytes).build();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN);
        let received = transport.receive(&mut mock_reader).await.unwrap();

        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Ping message, got {received:?}"),
        }
    }

    #[tokio::test]
    async fn test_send_message() {
        let transport = AsyncV1Transport::new(Magic::BITCOIN);
        let mut write_buffer = Vec::new();

        let message = NetworkMessage::Ping(42);
        transport
            .send(message.clone(), &mut write_buffer)
            .await
            .unwrap();

        let expected = create_test_message(Magic::BITCOIN, message);
        assert_eq!(write_buffer, expected);
    }

    #[tokio::test]
    async fn test_magic_mismatch() {
        let payload = NetworkMessage::Ping(42);
        let message_bytes = create_test_message(Magic::TESTNET4, payload);
        let mut mock_reader = MockIoBuilder::new().read(&message_bytes).build();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN);

        let result = transport.receive(&mut mock_reader).await;
        assert!(matches!(result, Err(TransportError::MagicMismatch)));
    }

    #[tokio::test]
    async fn test_invalid_message() {
        // Construct a valid header to reach the deserialization logic,
        // but with invalid payload that will cause a deserialization error.
        let mut header = [0u8; HEADER_SIZE];
        // Set magic bytes to Bitcoin network.
        header[0..4].copy_from_slice(&Magic::BITCOIN.to_bytes());
        // Set a command name.
        header[4] = b'p';
        header[5] = b'i';
        header[6] = b'n';
        header[7] = b'g';
        // Set small invalid payload length.
        let payload_len: u32 = 6;
        header[PAYLOAD_LENGTH_OFFSET..PAYLOAD_LENGTH_OFFSET + 4]
            .copy_from_slice(&payload_len.to_le_bytes());

        // Create invalid payload data.
        let invalid_payload = vec![0xFF; payload_len as usize];

        let mut test_data = Vec::new();
        test_data.extend_from_slice(&header);
        test_data.extend_from_slice(&invalid_payload);
        let mut mock_reader = MockIoBuilder::new().read(&test_data).build();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN);

        let result = transport.receive(&mut mock_reader).await;
        assert!(matches!(result, Err(TransportError::Deserialize(_))));
    }

    #[tokio::test]
    async fn test_unexpected_eof_during_header() {
        // Create partial message less than header size.
        let partial_data = vec![0; 10];
        let mut mock_reader = MockIoBuilder::new().read(&partial_data).build();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN);

        let result = transport.receive(&mut mock_reader).await;
        assert!(matches!(result, Err(TransportError::Io(_))));
    }

    #[tokio::test]
    async fn test_unexpected_eof_during_payload() {
        let payload = NetworkMessage::Ping(42);
        let mut message_bytes = create_test_message(Magic::BITCOIN, payload);
        // Truncate the message to include header but not full payload
        message_bytes.truncate(HEADER_SIZE + 2);

        let mut mock_reader = MockIoBuilder::new().read(&message_bytes).build();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN);

        let result = transport.receive(&mut mock_reader).await;
        assert!(matches!(result, Err(TransportError::Io(_))));
    }

    #[tokio::test]
    async fn test_cancellation_safety() {
        let payload = NetworkMessage::Ping(42);
        let message_bytes = create_test_message(Magic::BITCOIN, payload.clone());

        // Create a series of MockIo readers, each returning one byte.
        // simulates reading one byte at a time.
        let mut mock_reader = MockIoBuilder::new();
        for i in 0..message_bytes.len() {
            mock_reader.read(&message_bytes[i..i + 1]);
        }

        let mut mock_reader = mock_reader.build();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN);
        let received = transport.receive(&mut mock_reader).await.unwrap();

        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Ping message, got {received:?}"),
        }
    }

    #[tokio::test]
    async fn test_multiple_messages() {
        let payload1 = NetworkMessage::Ping(42);
        let payload2 = NetworkMessage::Ping(43);
        let message1 = create_test_message(Magic::BITCOIN, payload1);
        let message2 = create_test_message(Magic::BITCOIN, payload2);

        let mut combined = Vec::new();
        combined.extend_from_slice(&message1);
        combined.extend_from_slice(&message2);

        let mut mock_reader = MockIoBuilder::new().read(&combined).build();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN);

        let received1 = transport.receive(&mut mock_reader).await.unwrap();
        match received1 {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Ping message, got {received1:?}"),
        }

        let received2 = transport.receive(&mut mock_reader).await.unwrap();
        match received2 {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 43),
            _ => panic!("Expected Ping message, got {received2:?}"),
        }
    }
}
