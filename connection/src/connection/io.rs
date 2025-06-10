//! I/O layer for connection handling.
//!
//! This module contains the connection types that handle I/O operations with peers.
//! These types work with AsyncRead/AsyncWrite traits to manage the byte-level
//! communication, delegating message serialization to the transport layer.

use super::configuration::ConnectionConfiguration;
use super::error::ConnectionError;
use super::state::ConnectionState;
use crate::peer::Peer;
use crate::transport::{Transport, TransportReceiver, TransportSender};
use bitcoin::p2p::message::NetworkMessage;
use log::debug;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

/// Implements the sender half of a peer connection.
///
/// This struct handles sending Bitcoin network messages to a connected peer.
/// It maintains only the necessary state for sending operations.
#[derive(Debug)]
pub struct AsyncConnectionSender<W>
where
    W: AsyncWrite + Unpin + Send,
{
    /// The peer this connection is established with.
    peer: Arc<Mutex<Peer>>,
    /// Transport handles serialization and encryption of messages.
    transport_sender: TransportSender,
    /// The writer half of the connection.
    writer: W,
}

impl<W> std::fmt::Display for AsyncConnectionSender<W>
where
    W: AsyncWrite + Unpin + Send,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let transport = match &self.transport_sender {
            TransportSender::V1(_) => "V1",
            TransportSender::V2(_) => "V2",
        };

        let peer_info = self
            .peer
            .try_lock()
            .map(|peer| peer.to_string())
            .unwrap_or_else(|_| "<peer locked>".to_string());

        write!(f, "TCP/{transport} sender to {peer_info}")
    }
}

impl<W> AsyncConnectionSender<W>
where
    W: AsyncWrite + Unpin + Send,
{
    /// Creates a new sender with the given transport sender and writer.
    pub(super) fn new(
        peer: Arc<Mutex<Peer>>,
        transport_sender: TransportSender,
        writer: W,
    ) -> Self {
        Self {
            peer,
            transport_sender,
            writer,
        }
    }

    /// Get a copy of the peer this connection is established with.
    pub async fn peer(&self) -> Peer {
        self.peer.lock().await.clone()
    }

    /// Send a message to the peer.
    pub async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        self.transport_sender
            .send(message, &mut self.writer)
            .await
            .map_err(ConnectionError::TransportFailed)
    }
}

/// Implements the receiver half of a peer connection.
///
/// This struct handles receiving bitcoin network messages from a connected peer
/// and performs automatic protocol-level responses like responding to pings.
#[derive(Debug)]
pub struct AsyncConnectionReceiver<R>
where
    R: AsyncRead + Unpin + Send,
{
    /// The peer this connection is established with.
    peer: Arc<Mutex<Peer>>,
    /// Transport handles deserialization and decryption of messages.
    transport_receiver: TransportReceiver,
    /// The reader half of the connection.
    reader: R,
    /// State related to protocol negotiation.
    state: Arc<Mutex<ConnectionState>>,
}

impl<R> std::fmt::Display for AsyncConnectionReceiver<R>
where
    R: AsyncRead + Unpin + Send,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let transport = match &self.transport_receiver {
            TransportReceiver::V1(_) => "V1",
            TransportReceiver::V2(_) => "V2",
        };

        let peer_info = self
            .peer
            .try_lock()
            .map(|peer| peer.to_string())
            .unwrap_or_else(|_| "<peer locked>".to_string());

        let state_info = self
            .state
            .try_lock()
            .map(|state| format!(" ({state})"))
            .unwrap_or_else(|_| " (state: <locked>)".to_string());

        write!(f, "TCP/{transport} receiver to {peer_info}{state_info}")
    }
}

impl<R> AsyncConnectionReceiver<R>
where
    R: AsyncRead + Unpin + Send,
{
    /// Creates a new receiver with the given transport receiver and reader.
    pub(super) fn new(
        peer: Arc<Mutex<Peer>>,
        transport_receiver: TransportReceiver,
        reader: R,
        state: Arc<Mutex<ConnectionState>>,
    ) -> Self {
        Self {
            peer,
            transport_receiver,
            reader,
            state,
        }
    }

    /// Get a copy of the peer this connection is established with.
    pub async fn peer(&self) -> Peer {
        self.peer.lock().await.clone()
    }

    /// Get a copy of the current connection state.
    ///
    /// The connection state includes information about protocol features that have been
    /// negotiated with the peer. See [`ConnectionState`] for details.
    pub async fn state(&self) -> ConnectionState {
        self.state.lock().await.clone()
    }

    /// Receive a message from the peer.
    ///
    /// This method handles certain protocol-level messages automatically,
    /// such as updating protocol negotiation state.
    pub async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        let message = self
            .transport_receiver
            .receive(&mut self.reader)
            .await
            .map_err(ConnectionError::TransportFailed)?;

        // Handle protocol-level messages that affect connection state.
        match &message {
            NetworkMessage::SendAddrV2 => {
                let mut state = self.state.lock().await;
                state.addr_v2 = state.addr_v2.on_receive();
                debug!(
                    "Received SendAddrV2 message, addrv2_state: {:?}",
                    state.addr_v2
                );
            }
            NetworkMessage::SendHeaders => {
                let mut state = self.state.lock().await;
                state.send_headers = state.send_headers.on_receive();
                debug!(
                    "Received SendHeaders message, send_headers_state: {:?}",
                    state.send_headers
                );
            }
            NetworkMessage::WtxidRelay => {
                let mut state = self.state.lock().await;
                state.wtxid_relay = state.wtxid_relay.on_receive();
                debug!(
                    "Received WtxidRelay message, wtxid_relay_state: {:?}",
                    state.wtxid_relay
                );
            }
            _ => {}
        }

        Ok(message)
    }
}

/// Represents a connection to a bitcoin peer.
///
/// This struct manages a connection to a bitcoin peer using the bitcoin p2p protocol.
/// It handles the underlying transport, serialization, protocol management,
/// and connection state (e.g. upgrades).
///
/// # Trait Bounds
///
/// * [`AsyncRead`]/[`AsyncWrite`] - Required for async I/O operations.
/// * [`Unpin`] - Required because uses `&mut self` with `.await`.
/// * [`Send`] - Allows the connection to be sent between threads/tasks.
///
/// Note that [`Sync`] is not required because this struct uses `&mut self` methods
/// which enforce exclusive access. `AsyncConnection` is designed to be used by one thread
/// at a time, but if you need to share a `AsyncConnection` between threads, wrap it in
/// an [`Arc`]<[`Mutex`]<`AsyncConnection<R, W>`>>.
///
/// [`AsyncRead`]: tokio::io::AsyncRead
/// [`AsyncWrite`]: tokio::io::AsyncWrite
/// [`Unpin`]: core::marker::Unpin
/// [`Send`]: core::marker::Send
/// [`Sync`]: core::marker::Sync
/// [`Arc`]: std::sync::Arc
/// [`Mutex`]: tokio::sync::Mutex
#[derive(Debug)]
pub struct AsyncConnection<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Configuration to build the connection.
    pub(super) configuration: ConnectionConfiguration,
    /// The peer this connection is established with.
    pub(super) peer: Arc<Mutex<Peer>>,
    /// Runtime state of the connection, shared with receiver.
    pub(super) state: Arc<Mutex<ConnectionState>>,
    /// Receiver component for incoming messages.
    receiver: AsyncConnectionReceiver<R>,
    /// Sender component for outgoing messages.
    sender: AsyncConnectionSender<W>,
}

impl<R, W> std::fmt::Display for AsyncConnection<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Get transport version from receiver
        let transport = match &self.receiver.transport_receiver {
            TransportReceiver::V1(_) => "V1",
            TransportReceiver::V2(_) => "V2",
        };

        let peer_info = self
            .peer
            .try_lock()
            .map(|peer| peer.to_string())
            .unwrap_or_else(|_| "<peer locked>".to_string());

        let state_info = self
            .state
            .try_lock()
            .map(|state| format!(" ({state})"))
            .unwrap_or_else(|_| " (state: <locked>)".to_string());

        write!(f, "TCP/{transport} connection to {peer_info}{state_info}")
    }
}

impl<R, W> AsyncConnection<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Creates a new connection with the given components.
    pub(crate) fn new(
        peer: Peer,
        configuration: ConnectionConfiguration,
        transport: Transport,
        reader: R,
        writer: W,
    ) -> Self {
        let (transport_receiver, transport_sender) = transport.into_split();
        let state = Arc::new(Mutex::new(ConnectionState::new()));
        let peer = Arc::new(Mutex::new(peer));

        let receiver =
            AsyncConnectionReceiver::new(peer.clone(), transport_receiver, reader, state.clone());
        let sender = AsyncConnectionSender::new(peer.clone(), transport_sender, writer);

        Self {
            configuration,
            peer,
            state,
            receiver,
            sender,
        }
    }

    /// Get a copy of the peer this connection is established with.
    pub async fn peer(&self) -> Peer {
        self.peer.lock().await.clone()
    }

    /// Get a copy of the current connection state.
    ///
    /// The connection state includes information about protocol features that have been
    /// negotiated with the peer. See [`ConnectionState`] for details.
    pub async fn state(&self) -> ConnectionState {
        self.state.lock().await.clone()
    }

    /// Send a message to the peer.
    pub async fn send(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        self.sender.send(message).await
    }

    /// Receive a message from the peer.
    pub async fn receive(&mut self) -> Result<NetworkMessage, ConnectionError> {
        self.receiver.receive().await
    }

    /// Split this connection into separate receiver and sender halves.
    ///
    /// This allows for independent reading and writing operations, which is useful
    /// for concurrent processing in async contexts.
    ///
    /// # Returns
    ///
    /// A tuple containing the receiver and sender halves of the connection.
    pub fn into_split(self) -> (AsyncConnectionReceiver<R>, AsyncConnectionSender<W>) {
        (self.receiver, self.sender)
    }
}
