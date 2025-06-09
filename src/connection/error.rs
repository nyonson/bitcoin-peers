//! Error types for connection handling.

use crate::transport::TransportError;
use std::error::Error;
use std::fmt;
use std::io;

#[derive(Debug)]
pub enum ConnectionError {
    Io(io::Error),
    TransportFailed(TransportError),
    ProtocolFailed,
    UnsupportedAddressType,
    ConnectionLoop,
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