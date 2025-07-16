//! Async (futures-based) connection handling.
//!
//! This module contains the async connection types that handle I/O operations with peers.
//! These types work with TCP streams to manage the byte-level communication,
//! delegating message serialization to the transport layer.
//!
//! This is the primary interface for async applications. A blocking std-only interface
//! may be added in the future for applications that don't use async.

use super::configuration::{default_user_agent, ConnectionConfiguration};
use super::error::ConnectionError;
use super::handshake::{address_to_socket, generate_nonce, unix_timestamp};
use super::state::ConnectionState;
use crate::connection::handshake::{handle_verack_message, HandshakeState};
use crate::peer::{
    Peer, PeerProtocolVersion, PeerServices, ADDRV2_MIN_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION,
    SENDHEADERS_MIN_PROTOCOL_VERSION, WTXID_RELAY_MIN_PROTOCOL_VERSION,
};
use crate::transport::{Transport, TransportReader, TransportWriter};
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::p2p::ServiceFlags;
use log::{debug, error};
use std::sync::Arc;
use tokio::io::BufReader;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::Mutex;

/// Implements the sender half of a peer connection.
///
/// This struct handles sending Bitcoin network messages to a connected peer.
/// It maintains only the necessary state for sending operations.
#[derive(Debug)]
pub struct ConnectionWriter {
    /// The peer this connection is established with.
    peer: Arc<Mutex<Peer>>,
    /// Transport handles serialization and encryption of messages, and owns the writer.
    writer: TransportWriter<OwnedWriteHalf>,
}

impl std::fmt::Display for ConnectionWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let transport = match &self.writer {
            TransportWriter::V1(..) => "V1",
            TransportWriter::V2(..) => "V2",
        };

        let peer_info = self
            .peer
            .try_lock()
            .map(|peer| peer.to_string())
            .unwrap_or_else(|_| "<peer locked>".to_string());

        write!(f, "TCP/{transport} sender to {peer_info}")
    }
}

impl ConnectionWriter {
    /// Creates a new sender with the given transport sender.
    fn new(peer: Arc<Mutex<Peer>>, writer: TransportWriter<OwnedWriteHalf>) -> Self {
        Self { peer, writer }
    }

    /// Get a copy of the peer this connection is established with.
    pub async fn peer(&self) -> Peer {
        self.peer.lock().await.clone()
    }

    /// Send a message to the peer.
    pub async fn write(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        self.writer
            .write(message)
            .await
            .map_err(ConnectionError::TransportFailed)
    }
}

/// Implements the receiver half of a peer connection.
///
/// This struct handles receiving bitcoin network messages from a connected peer
/// and performs automatic protocol-level responses like responding to pings.
#[derive(Debug)]
pub struct ConnectionReader {
    /// The peer this connection is established with.
    peer: Arc<Mutex<Peer>>,
    /// Transport handles deserialization and decryption of messages, and owns the reader.
    reader: TransportReader<BufReader<OwnedReadHalf>>,
    /// State related to protocol negotiation.
    state: Arc<Mutex<ConnectionState>>,
}

impl std::fmt::Display for ConnectionReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let transport = match &self.reader {
            TransportReader::V1(..) => "V1",
            TransportReader::V2(..) => "V2",
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

impl ConnectionReader {
    /// Creates a new receiver with the given transport receiver.
    fn new(
        peer: Arc<Mutex<Peer>>,
        reader: TransportReader<BufReader<OwnedReadHalf>>,
        state: Arc<Mutex<ConnectionState>>,
    ) -> Self {
        Self {
            peer,
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
    pub async fn read(&mut self) -> Result<NetworkMessage, ConnectionError> {
        let message = self
            .reader
            .read()
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
/// This struct manages a TCP connection to a bitcoin peer using the bitcoin p2p protocol.
/// It handles the underlying transport, serialization, protocol management,
/// and connection state (e.g. upgrades).
#[derive(Debug)]
pub struct Connection {
    /// Configuration to build the connection.
    configuration: ConnectionConfiguration,
    /// The peer this connection is established with.
    peer: Arc<Mutex<Peer>>,
    /// Runtime state of the connection, shared with reader.
    state: Arc<Mutex<ConnectionState>>,
    /// Receiver component for incoming messages.
    reader: ConnectionReader,
    /// Sender component for outgoing messages.
    writer: ConnectionWriter,
}

impl std::fmt::Display for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let transport = match &self.reader.reader {
            TransportReader::V1(..) => "V1",
            TransportReader::V2(..) => "V2",
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

impl Connection {
    /// Creates a new connection with the given components.
    fn new(
        peer: Peer,
        configuration: ConnectionConfiguration,
        transport: Transport<BufReader<OwnedReadHalf>, OwnedWriteHalf>,
    ) -> Self {
        let (transport_receiver, transport_sender) = transport.into_split();
        let state = Arc::new(Mutex::new(ConnectionState::new()));
        let peer = Arc::new(Mutex::new(peer));

        let receiver = ConnectionReader::new(peer.clone(), transport_receiver, state.clone());
        let sender = ConnectionWriter::new(peer.clone(), transport_sender);

        Self {
            configuration,
            peer,
            state,
            reader: receiver,
            writer: sender,
        }
    }

    /// Establish a TCP connection to a bitcoin peer and perform the handshake.
    ///
    /// # Arguments
    ///
    /// * `peer` - The bitcoin peer to connect to.
    /// * `network` - The bitcoin [`Network`](bitcoin::Network) to use.
    /// * `configuration` - Configuration for the connection.
    ///
    /// # Returns
    ///
    /// * `Ok(Self)` - A successfully established and handshaked connection
    /// * `Err(ConnectionError)` - If the connection attempt or handshake failed
    ///
    /// # Example
    ///
    /// ```
    /// use bitcoin::Network;
    /// use bitcoin_peers_connection::{futures::Connection, ConnectionConfiguration, Peer, PeerProtocolVersion, TransportPolicy, FeaturePreferences};
    /// use bitcoin::p2p::address::AddrV2;
    /// use std::net::Ipv4Addr;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let peer = Peer::new(
    ///     AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)),
    ///     8333,
    /// );
    ///
    /// let config = ConnectionConfiguration::non_listening(
    ///     PeerProtocolVersion::Known(70016),
    ///     TransportPolicy::V2Required,
    ///     FeaturePreferences::default(),
    ///     None,
    /// );
    ///
    /// let mut connection = Connection::connect(peer, Network::Bitcoin, config).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(
        peer: Peer,
        network: bitcoin::Network,
        configuration: ConnectionConfiguration,
    ) -> Result<Self, ConnectionError> {
        let transport = super::tcp::connect(&peer, network, configuration.transport_policy).await?;
        let mut connection = Self::new(peer, configuration, transport);
        connection.perform_handshake().await?;

        Ok(connection)
    }

    /// Accept an incoming TCP connection from a bitcoin peer and perform the handshake.
    ///
    /// This function is used for inbound connections where another peer is connecting to your node.
    /// Unlike [`Connection::connect`] which connects to a known peer, this function discovers peer
    /// information during the handshake process.
    ///
    /// # Arguments
    ///
    /// * `stream` - The incoming TCP stream from a connecting peer
    /// * `network` - The bitcoin [`Network`](bitcoin::Network) to use
    /// * `configuration` - Configuration for the connection
    ///
    /// # Returns
    ///
    /// * `Ok(Self)` - A successfully established and handshaked connection
    /// * `Err(ConnectionError)` - If the handshake failed or connection was invalid
    ///
    /// # Example
    ///
    /// ```no_run
    /// use bitcoin::Network;
    /// use bitcoin_peers_connection::{futures::Connection, ConnectionConfiguration, PeerProtocolVersion, TransportPolicy, FeaturePreferences};
    /// use tokio::net::TcpListener;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let listener = TcpListener::bind("127.0.0.1:8333").await?;
    /// let config = ConnectionConfiguration::non_listening(
    ///     PeerProtocolVersion::Known(70016),
    ///     TransportPolicy::V2Required,
    ///     FeaturePreferences::default(),
    ///     None,
    /// );
    ///
    /// let (stream, addr) = listener.accept().await?;
    /// let connection = Connection::accept(stream, Network::Bitcoin, config).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn accept(
        stream: tokio::net::TcpStream,
        network: bitcoin::Network,
        configuration: ConnectionConfiguration,
    ) -> Result<Self, ConnectionError> {
        let (transport, peer) =
            super::tcp::accept(stream, network, configuration.transport_policy).await?;
        let mut connection = Self::new(peer, configuration, transport);
        connection.perform_handshake().await?;

        Ok(connection)
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
    pub async fn write(&mut self, message: NetworkMessage) -> Result<(), ConnectionError> {
        self.writer.write(message).await
    }

    /// Receive a message from the peer.
    pub async fn read(&mut self) -> Result<NetworkMessage, ConnectionError> {
        self.reader.read().await
    }

    /// Split this connection into separate receiver and sender halves.
    ///
    /// This allows for independent reading and writing operations, which is useful
    /// for concurrent processing in async contexts.
    ///
    /// # Returns
    ///
    /// A tuple containing the receiver and sender halves of the connection.
    pub fn into_split(self) -> (ConnectionReader, ConnectionWriter) {
        (self.reader, self.writer)
    }

    /// Performs the bitcoin p2p version handshake protocol.
    ///
    /// 1. Send local version message.
    /// 2. Receive and validate peer's version.
    /// 3. Exchange verack messages.
    /// 4. Negotiate protocol features (AddrV2, SendHeaders, WtxidRelay).
    async fn perform_handshake(&mut self) -> Result<(), ConnectionError> {
        // Generate nonce for connection loop detection
        let nonce = generate_nonce();

        // Send our version message
        let version_message = self.create_version_message(nonce).await;
        self.write(version_message).await?;
        debug!("Sent version message to peer");

        let mut state = HandshakeState::VersionSent;

        // Process messages until handshake completes
        while state != HandshakeState::Complete {
            let message = self.read().await?;

            match message {
                NetworkMessage::Version(version) => {
                    state = self.handle_version_message(version, nonce, state).await?;
                }
                NetworkMessage::Verack => {
                    state = handle_verack_message(state);
                }
                _ => {
                    debug!("Received unexpected message during handshake: {message:?}, ignoring");
                }
            }
        }

        debug!("Handshake completed successfully");
        Ok(())
    }

    /// Creates a version message for the handshake.
    async fn create_version_message(&self, nonce: u64) -> NetworkMessage {
        let peer_lock = self.peer.lock().await;

        let receiver_services = match peer_lock.services {
            PeerServices::Known(flags) => flags,
            PeerServices::Unknown => ServiceFlags::NONE,
        };

        let receiver_socket_addr = address_to_socket(&peer_lock.address, peer_lock.port);
        let sender_socket_addr = address_to_socket(
            &self.configuration.sender_address,
            self.configuration.sender_port,
        );

        let user_agent_string = match &self.configuration.user_agent {
            Some(ua) => ua.as_str().to_string(),
            None => default_user_agent().to_string(),
        };

        let version = VersionMessage {
            version: self
                .configuration
                .protocol_version
                .unwrap_or(MIN_PROTOCOL_VERSION),
            services: self.configuration.services,
            timestamp: unix_timestamp(),
            receiver: Address::new(&receiver_socket_addr, receiver_services),
            sender: Address::new(&sender_socket_addr, self.configuration.services),
            nonce,
            user_agent: user_agent_string,
            start_height: self.configuration.start_height,
            relay: self.configuration.relay,
        };

        NetworkMessage::Version(version)
    }

    /// Handles received version message.
    async fn handle_version_message(
        &mut self,
        version: VersionMessage,
        our_nonce: u64,
        state: HandshakeState,
    ) -> Result<HandshakeState, ConnectionError> {
        // Check for connection loop.
        if version.nonce == our_nonce {
            error!("Connection loop detected - received same nonce");
            return Err(ConnectionError::ConnectionLoop);
        }

        match state {
            HandshakeState::VersionSent | HandshakeState::VerackReceived => {
                debug!("Received version message from peer");

                // Update peer information.
                {
                    let mut peer = self.peer.lock().await;
                    peer.services = PeerServices::Known(version.services);
                    peer.version = PeerProtocolVersion::Known(version.version);
                }

                // Calculate effective protocol version.
                let local_version = self
                    .configuration
                    .protocol_version
                    .unwrap_or(MIN_PROTOCOL_VERSION);
                let effective_version = std::cmp::min(local_version, version.version);

                // Update connection state.
                {
                    let mut state = self.state.lock().await;
                    state.effective_protocol_version =
                        PeerProtocolVersion::Known(effective_version);
                }

                // Send protocol negotiation messages.
                self.send_negotiation_messages(effective_version).await?;

                // Send verack.
                self.write(NetworkMessage::Verack).await?;
                debug!("Sent verack message to peer");

                Ok(if state == HandshakeState::VerackReceived {
                    HandshakeState::Complete
                } else {
                    HandshakeState::VersionReceived
                })
            }
            _ => {
                debug!("Received duplicate version message in state {state:?}, ignoring");
                Ok(state)
            }
        }
    }

    /// Sends protocol feature negotiation messages based on effective version and configuration.
    async fn send_negotiation_messages(
        &mut self,
        effective_version: u32,
    ) -> Result<(), ConnectionError> {
        let features = self.configuration.feature_preferences;

        // SendAddrV2 for address format support (BIP155).
        if features.enable_addrv2 && effective_version >= ADDRV2_MIN_PROTOCOL_VERSION {
            self.write(NetworkMessage::SendAddrV2).await?;
            let mut state = self.state.lock().await;
            state.addr_v2 = state.addr_v2.on_send();
            debug!("Sent SendAddrV2 message, addrv2_state: {:?}", state.addr_v2);
        }

        // SendHeaders for header announcements.
        if features.enable_sendheaders && effective_version >= SENDHEADERS_MIN_PROTOCOL_VERSION {
            self.write(NetworkMessage::SendHeaders).await?;
            let mut state = self.state.lock().await;
            state.send_headers = state.send_headers.on_send();
            debug!(
                "Sent SendHeaders message, send_headers_state: {:?}",
                state.send_headers
            );
        }

        // WtxidRelay for witness transaction ID relay.
        if features.enable_wtxidrelay && effective_version >= WTXID_RELAY_MIN_PROTOCOL_VERSION {
            self.write(NetworkMessage::WtxidRelay).await?;
            let mut state = self.state.lock().await;
            state.wtxid_relay = state.wtxid_relay.on_send();
            debug!(
                "Sent WtxidRelay message, wtxid_relay_state: {:?}",
                state.wtxid_relay
            );
        }

        Ok(())
    }
}
