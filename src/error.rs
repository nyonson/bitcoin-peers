use std::fmt;
use std::io;

#[derive(Debug)]
pub enum PeersError {
    ConnectionError(io::Error),
    PeerConnectionFailed,
    UnsupportedAddressType,
    ConnectionLoop,
}

impl fmt::Display for PeersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeersError::ConnectionError(err) => write!(f, "Connection error: {err}"),
            PeersError::PeerConnectionFailed => write!(f, "Failed to connect to peer"),
            PeersError::UnsupportedAddressType => write!(f, "Unsupported address type"),
            PeersError::ConnectionLoop => {
                write!(f, "Detected connection to self (matching nonce)")
            }
        }
    }
}

impl std::error::Error for PeersError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PeersError::ConnectionError(err) => Some(err),
            PeersError::PeerConnectionFailed => None,
            PeersError::UnsupportedAddressType => None,
            PeersError::ConnectionLoop => None,
        }
    }
}

impl From<io::Error> for PeersError {
    fn from(err: io::Error) -> Self {
        PeersError::ConnectionError(err)
    }
}
