//! Bitcoin V1 protocol transport implementation.
//!
//! This module implements the traditional plaintext bitcoin network protocol,
//! handling message serialization, deserialization, and framing.

use crate::transport::TransportError;
use bitcoin::consensus::encode;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::Magic;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Size of a Bitcoin message header in bytes.
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
#[derive(Debug)]
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
}
