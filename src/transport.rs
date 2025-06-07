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
//! provides a single interface across both for callers.
//!
//! Giving the transport the ability to split into send and receive halves adds
//! boilerplate at each level, but delegation is maintained over duplicating
//! logic.

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

/// Implements the sender half of the bitcoin v1 protocol transport.
#[derive(Debug, Clone)]
pub struct AsyncV1TransportSender {
    /// The bitcoin network magic bytes.
    network_magic: Magic,
}

impl AsyncV1TransportSender {
    /// Create a new sender with the specified network magic.
    pub fn new(network_magic: Magic) -> Self {
        Self { network_magic }
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

/// Implements the receiver half of the bitcoin V1 protocol transport.
#[derive(Debug)]
pub struct AsyncV1TransportReceiver {
    /// The bitcoin network magic bytes.
    network_magic: Magic,
    /// Current state of the receive operation.
    receive_state: V1ReceiveState,
}

impl AsyncV1TransportReceiver {
    /// Create a new receiver with the specified network magic.
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
}

/// Implements the bitcoin V1 protocol transport.
///
/// This transport provides methods to send and receive bitcoin protocol messages
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
pub struct AsyncV1Transport {
    /// The sender component.
    sender: AsyncV1TransportSender,
    /// The receiver component.
    receiver: AsyncV1TransportReceiver,
}

impl AsyncV1Transport {
    /// Create a new [`AsyncV1Transport`] for the specified network magic.
    pub fn new(network_magic: Magic) -> Self {
        Self {
            sender: AsyncV1TransportSender::new(network_magic),
            receiver: AsyncV1TransportReceiver::new(network_magic),
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
        self.receiver.receive(reader).await
    }

    /// Sends a bitcoin network message to the writer.
    pub async fn send<W>(
        &self,
        message: NetworkMessage,
        writer: &mut W,
    ) -> Result<(), TransportError>
    where
        W: AsyncWrite + Unpin + Send,
    {
        self.sender
            .send(message, writer)
            .await
            .map_err(TransportError::Io)
    }

    /// Split this transport into separate reader and writer halves.
    ///
    /// This allows for independent reading and writing operations.
    pub fn into_split(self) -> (AsyncV1TransportReceiver, AsyncV1TransportSender) {
        (self.receiver, self.sender)
    }
}

/// Implements the bitcoin v2 protocol transport using BIP-324 encryption.
///
/// This transport provides methods to send and receive bitcoin protocol messages
/// over any type that implements `AsyncRead` and `AsyncWrite`, using the
/// encrypted BIP-324 protocol.
pub struct AsyncV2Transport {
    sender: AsyncV2TransportSender,
    receiver: AsyncV2TransportReceiver,
}

impl AsyncV2Transport {
    /// Create a new [`AsyncV2Transport`] from a BIP-324 AsyncProtocol.
    pub fn new(protocol: AsyncProtocol) -> Self {
        let (reader, writer) = protocol.into_split();
        Self {
            sender: AsyncV2TransportSender { writer },
            receiver: AsyncV2TransportReceiver { reader },
        }
    }

    /// Receives a bitcoin network message from the reader.
    pub async fn receive<R>(&mut self, reader: &mut R) -> Result<NetworkMessage, TransportError>
    where
        R: AsyncRead + Unpin + Send,
    {
        self.receiver.receive(reader).await
    }

    /// Sends a bitcoin network message to the writer.
    pub async fn send<W>(
        &mut self,
        message: NetworkMessage,
        writer: &mut W,
    ) -> Result<(), TransportError>
    where
        W: AsyncWrite + Unpin + Send,
    {
        self.sender.send(message, writer).await
    }

    /// Split this transport into separate reader and writer halves.
    pub fn into_split(self) -> (AsyncV2TransportReceiver, AsyncV2TransportSender) {
        (self.receiver, self.sender)
    }
}

/// Implements the receiver half of the bitcoin V2 protocol transport.
pub struct AsyncV2TransportReceiver {
    reader: bip324::AsyncProtocolReader,
}

impl AsyncV2TransportReceiver {
    /// Creates a new receiver from a BIP-324 AsyncProtocolReader.
    pub fn new(reader: bip324::AsyncProtocolReader) -> Self {
        Self { reader }
    }

    /// Receives a bitcoin network message from the reader.
    pub async fn receive<R>(&mut self, reader: &mut R) -> Result<NetworkMessage, TransportError>
    where
        R: AsyncRead + Unpin + Send,
    {
        let message = self
            .reader
            .read_and_decrypt(reader)
            .await
            .map_err(|_| TransportError::Encryption)?;

        bip324::serde::deserialize(message.contents()).map_err(|_| TransportError::Encryption)
    }
}

/// Implements the sender half of the bitcoin V2 protocol transport.
pub struct AsyncV2TransportSender {
    writer: bip324::AsyncProtocolWriter,
}

impl AsyncV2TransportSender {
    /// Creates a new sender from a BIP-324 AsyncProtocolWriter.
    pub fn new(writer: bip324::AsyncProtocolWriter) -> Self {
        Self { writer }
    }

    /// Sends a bitcoin network message to the writer.
    pub async fn send<W>(
        &mut self,
        message: NetworkMessage,
        writer: &mut W,
    ) -> Result<(), TransportError>
    where
        W: AsyncWrite + Unpin + Send,
    {
        let data = bip324::serde::serialize(message).expect("infallible");

        self.writer
            .encrypt_and_write(&data, writer)
            .await
            .map_err(|_| TransportError::Encryption)
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
/// let (mut receiver, mut sender) = transport.split();
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
    pub fn split(self) -> (TransportReceiver, TransportSender) {
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

    #[tokio::test]
    async fn test_transport_split() {
        // Test sending with the split sender
        let transport = AsyncV1Transport::new(Magic::BITCOIN);
        let (_, sender) = transport.into_split();
        let mut write_buffer = Vec::new();

        let message = NetworkMessage::Ping(42);
        sender
            .send(message.clone(), &mut write_buffer)
            .await
            .unwrap();

        let expected = create_test_message(Magic::BITCOIN, message);
        assert_eq!(write_buffer, expected);

        // Test receiving with the split receiver
        let transport = AsyncV1Transport::new(Magic::BITCOIN);
        let (mut receiver, _) = transport.into_split();

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
    async fn test_transport_enum_split() {
        // Test the V1 variant of Transport
        let transport = Transport::v1(Magic::BITCOIN);

        let (mut receiver, mut sender) = transport.split();

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
