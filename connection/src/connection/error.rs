//! Error types for connection handling.

use crate::transport::TransportError;
use std::error::Error;
use std::fmt;
use std::io;

/// Errors that can occur during peer connection establishment and communication.
#[derive(Debug)]
pub enum ConnectionError {
    /// An I/O error occurred during network operations.
    Io(io::Error),
    /// The transport layer (encryption and serialization) failed.
    TransportFailed(TransportError),
    /// Bitcoin p2p protocol handling failed.
    ProtocolFailed,
    /// Remote peer's address type is not supported for connections.
    UnsupportedAddressType,
    /// Detected a connection loop (attempting to connect to ourselves).
    ///
    /// Possible causes:
    ///
    /// * Local node's address appears in a peer's address list.
    /// * Port forwarding issues cause external connections to loopback.
    /// * Running multiple nodes on the same machine with shared address lists.
    ConnectionLoop,
    /// V2 encrypted transport required, but could not be established.
    V2TransportRequired,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionError::Io(err) => write!(f, "Connection error: {err}"),
            ConnectionError::TransportFailed(err) => {
                write!(f, "Transport layer failed in peer connection: {err}")
            }
            ConnectionError::ProtocolFailed => {
                write!(f, "Protocol handling failed in peer communication")
            }
            ConnectionError::UnsupportedAddressType => write!(f, "Unsupported address type"),
            ConnectionError::ConnectionLoop => {
                write!(f, "Detected connection to self (matching nonce)")
            }
            ConnectionError::V2TransportRequired => {
                write!(f, "V2 encrypted transport required but could not be established. To allow plaintext fallback, use TransportPolicy::V2Preferred")
            }
        }
    }
}

impl Error for ConnectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ConnectionError::Io(err) => Some(err),
            ConnectionError::TransportFailed(err) => Some(err),
            ConnectionError::ProtocolFailed => None,
            ConnectionError::UnsupportedAddressType => None,
            ConnectionError::ConnectionLoop => None,
            ConnectionError::V2TransportRequired => None,
        }
    }
}

impl From<io::Error> for ConnectionError {
    fn from(err: io::Error) -> Self {
        ConnectionError::Io(err)
    }
}

impl From<TransportError> for ConnectionError {
    fn from(err: TransportError) -> Self {
        ConnectionError::TransportFailed(err)
    }
}
