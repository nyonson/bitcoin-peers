//! Bitcoin v2 transport implementation.
//!
//! This module is a light wrapper around the encryption and serialization.

use crate::transport::TransportError;
use bip324::AsyncProtocol;
use bitcoin::p2p::message::NetworkMessage;
use tokio::io::{AsyncRead, AsyncWrite};

/// Implements the bitcoin v2 protocol transport using BIP-324 encryption.
///
/// This transport provides methods to send and receive bitcoin protocol messages
/// over any type that implements `AsyncRead` and `AsyncWrite`, using the
/// encrypted BIP-324 protocol.
#[derive(Debug)]
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

/// Implements the receiver half of the bitcoin v2 protocol transport.
pub struct AsyncV2TransportReceiver {
    reader: bip324::AsyncProtocolReader,
}

impl std::fmt::Debug for AsyncV2TransportReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncV2TransportReceiver")
            .field("reader", &"<AsyncProtocolReader>")
            .finish()
    }
}

impl AsyncV2TransportReceiver {
    /// Creates a new receiver from a BIP-324 [`bip324::AsyncProtocolReader`].
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

/// Implements the sender half of the bitcoin v2 protocol transport.
pub struct AsyncV2TransportSender {
    writer: bip324::AsyncProtocolWriter,
}

impl std::fmt::Debug for AsyncV2TransportSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncV2TransportSender")
            .field("writer", &"<AsyncProtocolWriter>")
            .finish()
    }
}

impl AsyncV2TransportSender {
    /// Creates a new sender from a BIP-324 [`bip324::AsyncProtocolWriter`].
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
