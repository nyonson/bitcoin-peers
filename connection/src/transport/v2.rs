//! Bitcoin v2 transport implementation.
//!
//! This module is a light wrapper around the encryption and serialization.

use crate::transport::TransportError;
use bip324::AsyncProtocol;
use bitcoin::p2p::message::NetworkMessage;
use std::any::type_name;
use tokio::io::{AsyncRead, AsyncWrite};

/// Implements the bitcoin v2 protocol transport using BIP-324 encryption.
///
/// This transport provides methods to send and receive bitcoin protocol messages
/// over any type that implements `AsyncRead` and `AsyncWrite`, using the
/// encrypted BIP-324 protocol.
pub struct AsyncV2Transport<R, W> {
    writer: AsyncV2TransportWriter<W>,
    reader: AsyncV2TransportReader<R>,
}

// Manual Debug implementation to delegate to the reader and writer Debug impls
impl<R, W> std::fmt::Debug for AsyncV2Transport<R, W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncV2Transport")
            .field("writer", &self.writer)
            .field("reader", &self.reader)
            .finish()
    }
}

impl<R, W> AsyncV2Transport<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Create a new [`AsyncV2Transport`] from a BIP-324 AsyncProtocol.
    pub fn new(protocol: AsyncProtocol, reader: R, writer: W) -> Self {
        let (reader_cipher, writer_cipher) = protocol.into_split();
        Self {
            writer: AsyncV2TransportWriter {
                cipher: writer_cipher,
                writer,
            },
            reader: AsyncV2TransportReader {
                cipher: reader_cipher,
                reader,
            },
        }
    }

    /// Receives a bitcoin network message from the reader.
    pub async fn read(&mut self) -> Result<NetworkMessage, TransportError> {
        self.reader.read().await
    }

    /// Sends a bitcoin network message to the writer.
    pub async fn write(&mut self, message: NetworkMessage) -> Result<(), TransportError> {
        self.writer.write(message).await
    }

    /// Split this transport into separate reader and writer halves.
    pub fn into_split(self) -> (AsyncV2TransportReader<R>, AsyncV2TransportWriter<W>) {
        (self.reader, self.writer)
    }
}

/// Implements the receiver half of the bitcoin v2 protocol transport.
pub struct AsyncV2TransportReader<R> {
    cipher: bip324::AsyncProtocolReader,
    reader: R,
}

// Manual Debug implementation because bip324::AsyncProtocolReader doesn't implement Debug
impl<R> std::fmt::Debug for AsyncV2TransportReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncV2TransportReader")
            .field("cipher", &"<bip324::AsyncProtocolReader>")
            .field("reader", &type_name::<R>())
            .finish()
    }
}

impl<R> AsyncV2TransportReader<R>
where
    R: AsyncRead + Unpin + Send,
{
    /// Creates a new receiver from a BIP-324 [`bip324::AsyncProtocolReader`].
    pub fn new(cipher: bip324::AsyncProtocolReader, reader: R) -> Self {
        Self { cipher, reader }
    }

    /// Receives a bitcoin network message from the reader.
    pub async fn read(&mut self) -> Result<NetworkMessage, TransportError> {
        let message = self
            .cipher
            .read_and_decrypt(&mut self.reader)
            .await
            .map_err(|_| TransportError::Encryption)?;

        bip324::serde::deserialize(message.contents()).map_err(|_| TransportError::Encryption)
    }
}

/// Implements the sender half of the bitcoin v2 protocol transport.
pub struct AsyncV2TransportWriter<W> {
    cipher: bip324::AsyncProtocolWriter,
    writer: W,
}

// Manual Debug implementation because bip324::AsyncProtocolWriter doesn't implement Debug
impl<W> std::fmt::Debug for AsyncV2TransportWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncV2TransportWriter")
            .field("cipher", &"<bip324::AsyncProtocolWriter>")
            .field("writer", &type_name::<W>())
            .finish()
    }
}

impl<W> AsyncV2TransportWriter<W>
where
    W: AsyncWrite + Unpin + Send,
{
    /// Creates a new sender from a BIP-324 [`bip324::AsyncProtocolWriter`].
    pub fn new(cipher: bip324::AsyncProtocolWriter, writer: W) -> Self {
        Self { cipher, writer }
    }

    /// Sends a bitcoin network message to the writer.
    pub async fn write(&mut self, message: NetworkMessage) -> Result<(), TransportError> {
        let data = bip324::serde::serialize(message).expect("infallible");

        self.cipher
            .encrypt_and_write(&data, &mut self.writer)
            .await
            .map_err(|_| TransportError::Encryption)
    }
}
