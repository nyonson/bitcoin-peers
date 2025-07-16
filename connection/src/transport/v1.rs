//! Bitcoin v1 protocol transport implementation.
//!
//! This module implements the traditional plaintext bitcoin network protocol,
//! handling message serialization and framing.

use crate::transport::TransportError;
use bitcoin::consensus::encode;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::Magic;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Size of a bitcoin message header in bytes.
const HEADER_SIZE: usize = 24;
/// Offset in the header where the payload length is stored.
const PAYLOAD_LENGTH_OFFSET: usize = 16;

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
pub struct AsyncV1TransportWriter<W> {
    /// The bitcoin network magic bytes.
    network_magic: Magic,
    /// The IO writer.
    writer: W,
}

impl<W> AsyncV1TransportWriter<W>
where
    W: AsyncWrite + Unpin + Send,
{
    /// Create a new sender with the specified network magic.
    pub fn new(network_magic: Magic, writer: W) -> Self {
        Self {
            network_magic,
            writer,
        }
    }

    /// Write a bitcoin network message.
    pub async fn write(&mut self, message: NetworkMessage) -> Result<(), io::Error>
    where
        W: AsyncWrite + Unpin + Send,
    {
        let raw_msg = RawNetworkMessage::new(self.network_magic, message);
        let data = encode::serialize(&raw_msg);

        self.writer.write_all(&data).await?;
        self.writer.flush().await?;

        Ok(())
    }
}

/// Implements the receiver half of the bitcoin v1 protocol transport.
#[derive(Debug)]
pub struct AsyncV1TransportReader<R> {
    /// The bitcoin network magic bytes.
    network_magic: Magic,
    /// Current state of the receive operation.
    receive_state: V1ReceiveState,
    /// The IO reader.
    reader: R,
}

impl<R> AsyncV1TransportReader<R>
where
    R: AsyncRead + Unpin + Send,
{
    /// Create a new receiver with the specified network magic.
    pub fn new(network_magic: Magic, reader: R) -> Self {
        Self {
            network_magic,
            receive_state: V1ReceiveState::reading_header(),
            reader,
        }
    }

    /// Read a bitcoin network message.
    ///
    /// This function is cancellation safe, meaning it can be safely used with `tokio::select!`
    /// and similar constructs without the risk of leaving the reader in an inconsistent state.
    pub async fn read(&mut self) -> Result<NetworkMessage, TransportError>
    where
        R: AsyncRead + Unpin + Send,
    {
        loop {
            match &mut self.receive_state {
                V1ReceiveState::ReadingHeader { header, bytes_read } => {
                    while *bytes_read < HEADER_SIZE {
                        let n = self.reader.read(&mut header[*bytes_read..]).await?;
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
                        let n = self.reader.read(&mut buffer[*bytes_read..]).await?;
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

/// Implements the bitcoin v1 protocol transport.
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
/// use bitcoin_peers_connection::AsyncV1Transport;
/// use tokio::net::TcpStream;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // Connect to a bitcoin node.
/// let stream = TcpStream::connect("127.0.0.1:8333").await?;
/// let (reader, writer) = stream.into_split();
///
/// // Create a transport for mainnet.
/// let mut transport = AsyncV1Transport::new(Magic::BITCOIN, reader, writer);
///
/// // Send a message.
/// let ping_message = NetworkMessage::Ping(42);
/// transport.write(ping_message).await?;
///
/// // Receive a response.
/// let response = transport.read().await?;
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
pub struct AsyncV1Transport<R, W> {
    /// The sender component.
    writer: AsyncV1TransportWriter<W>,
    /// The receiver component.
    reader: AsyncV1TransportReader<R>,
}

impl<R, W> AsyncV1Transport<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Create a new [`AsyncV1Transport`] for the specified network magic.
    pub fn new(network_magic: Magic, reader: R, writer: W) -> Self {
        Self {
            writer: AsyncV1TransportWriter::new(network_magic, writer),
            reader: AsyncV1TransportReader::new(network_magic, reader),
        }
    }

    /// Read a bitcoin network message.
    ///
    /// This function is cancellation safe, meaning it can be safely used with `tokio::select!`
    /// and similar constructs without the risk of leaving the reader in an inconsistent state.
    pub async fn read(&mut self) -> Result<NetworkMessage, TransportError> {
        self.reader.read().await
    }

    /// Write a bitcoin network message.
    pub async fn write(&mut self, message: NetworkMessage) -> Result<(), TransportError> {
        self.writer.write(message).await.map_err(TransportError::Io)
    }

    /// Split this transport into separate reader and writer halves.
    ///
    /// This allows for independent reading and writing operations.
    pub fn into_split(self) -> (AsyncV1TransportReader<R>, AsyncV1TransportWriter<W>) {
        (self.reader, self.writer)
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
        let mock_reader = MockIoBuilder::new().read(&message_bytes).build();
        let mock_writer = Vec::new();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN, mock_reader, mock_writer);
        let received = transport.read().await.unwrap();

        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Ping message, got {received:?}"),
        }
    }

    #[tokio::test]
    async fn test_send_message() {
        let mock_reader = MockIoBuilder::new().build();
        let write_buffer = Vec::new();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN, mock_reader, write_buffer);

        let message = NetworkMessage::Ping(42);
        transport.write(message.clone()).await.unwrap();

        // Get the writer back to check the written data
        let (_, writer) = transport.into_split();
        let expected = create_test_message(Magic::BITCOIN, message);
        assert_eq!(writer.writer, expected);
    }

    #[tokio::test]
    async fn test_magic_mismatch() {
        let payload = NetworkMessage::Ping(42);
        let message_bytes = create_test_message(Magic::TESTNET4, payload);
        let mock_reader = MockIoBuilder::new().read(&message_bytes).build();
        let mock_writer = Vec::new();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN, mock_reader, mock_writer);

        let result = transport.read().await;
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
        let mock_reader = MockIoBuilder::new().read(&test_data).build();
        let mock_writer = Vec::new();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN, mock_reader, mock_writer);

        let result = transport.read().await;
        assert!(matches!(result, Err(TransportError::Deserialize(_))));
    }

    #[tokio::test]
    async fn test_unexpected_eof_during_header() {
        // Create partial message less than header size.
        let partial_data = vec![0; 10];
        let mock_reader = MockIoBuilder::new().read(&partial_data).build();
        let mock_writer = Vec::new();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN, mock_reader, mock_writer);

        let result = transport.read().await;
        assert!(matches!(result, Err(TransportError::Io(_))));
    }

    #[tokio::test]
    async fn test_unexpected_eof_during_payload() {
        let payload = NetworkMessage::Ping(42);
        let mut message_bytes = create_test_message(Magic::BITCOIN, payload);
        // Truncate the message to include header but not full payload
        message_bytes.truncate(HEADER_SIZE + 2);

        let mock_reader = MockIoBuilder::new().read(&message_bytes).build();
        let mock_writer = Vec::new();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN, mock_reader, mock_writer);

        let result = transport.read().await;
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

        let mock_reader = mock_reader.build();
        let mock_writer = Vec::new();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN, mock_reader, mock_writer);
        let received = transport.read().await.unwrap();

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

        let mock_reader = MockIoBuilder::new().read(&combined).build();
        let mock_writer = Vec::new();
        let mut transport = AsyncV1Transport::new(Magic::BITCOIN, mock_reader, mock_writer);

        let received1 = transport.read().await.unwrap();
        match received1 {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Ping message, got {received1:?}"),
        }

        let received2 = transport.read().await.unwrap();
        match received2 {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 43),
            _ => panic!("Expected Ping message, got {received2:?}"),
        }
    }

    #[tokio::test]
    async fn test_transport_split() {
        // Test sending with the split sender
        let mock_reader = MockIoBuilder::new().build();
        let write_buffer = Vec::new();
        let transport = AsyncV1Transport::new(Magic::BITCOIN, mock_reader, write_buffer);
        let (_, mut sender) = transport.into_split();

        let message = NetworkMessage::Ping(42);
        sender.write(message.clone()).await.unwrap();

        let expected = create_test_message(Magic::BITCOIN, message);
        assert_eq!(sender.writer, expected);

        // Test receiving with the split receiver
        let payload = NetworkMessage::Ping(42);
        let message_bytes = create_test_message(Magic::BITCOIN, payload.clone());
        let mock_reader = MockIoBuilder::new().read(&message_bytes).build();
        let mock_writer = Vec::new();
        let transport = AsyncV1Transport::new(Magic::BITCOIN, mock_reader, mock_writer);
        let (mut receiver, _) = transport.into_split();

        let received = receiver.read().await.unwrap();
        match received {
            NetworkMessage::Ping(nonce) => assert_eq!(nonce, 42),
            _ => panic!("Expected Ping message, got {received:?}"),
        }
    }
}
