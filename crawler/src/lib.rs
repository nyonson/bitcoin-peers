mod crawler;

pub use crawler::{Crawler, CrawlerBuilder, CrawlerBuilderError, CrawlerMessage};

// Re-export commonly used types from the connection crate for convenience
pub use bitcoin_peers_connection::{
    Connection, ConnectionConfiguration, ConnectionError, ConnectionReceiver, ConnectionSender,
    Peer, PeerProtocolVersion, PeerServices,
};
