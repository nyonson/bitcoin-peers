//! Internal session coordination for crawling operations.
//!
//! This module contains the [`CrawlSession`] which orchestrates the crawling process
//! by managing the work queue and coordinating concurrent peer processing tasks.

use crate::connection::PeerConnection;
use crate::crawler::CrawlerMessage;
use bitcoin::Network;
use bitcoin_peers_connection::{
    Connection, ConnectionConfiguration, FeaturePreferences, Peer, PeerProtocolVersion,
    TransportPolicy, UserAgent,
};
use log::{debug, info};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Configuration for a crawl session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub network: Network,
    pub user_agent: Option<UserAgent>,
    pub transport_policy: TransportPolicy,
    pub protocol_version: PeerProtocolVersion,
    pub max_concurrent_tasks: usize,
    pub peer_timeout: Duration,
}

/// Result of processing a single peer in the crawler.
#[derive(Debug, Clone)]
enum TaskResult {
    /// Successfully connected and found peers.
    FoundPeers,
    /// Successfully connected but no peers received.
    NoPeersFound,
    /// Failed to connect to the peer.
    ConnectionFailed,
    /// Task exited early due to channel closure.
    ChannelClosed,
}

/// Internal coordinator for a crawling session.
///
/// `CrawlSession` orchestrates the crawling process by managing the work queue
/// and coordinating concurrent peer processing tasks. It acts as the execution
/// engine for a [`Crawler`] instance.
///
/// # Architecture
///
/// The session follows a producer-consumer pattern with lockless communication.
///
/// * **Coordinator** (`coordinate()`) - Manages the work queue and spawns processing tasks.
/// * **Processors** (`process()`) - Handle individual peer connections and discovery.
#[derive(Clone)]
pub struct CrawlSession {
    /// Crawler configuration.
    config: SessionConfig,
    /// Channel for sending discovery results back to the caller.
    crawl_tx: mpsc::Sender<CrawlerMessage>,
    /// Shared set of peers that have already been tested, used for deduplication.
    tested_peers: Arc<tokio::sync::RwLock<HashSet<Peer>>>,
}

impl CrawlSession {
    /// Create a new crawl session.
    pub fn new(config: SessionConfig, crawl_tx: mpsc::Sender<CrawlerMessage>) -> Self {
        Self {
            config,
            crawl_tx,
            tested_peers: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
        }
    }

    /// Processes a single peer: tests connectivity and discovers new peers.
    ///
    /// # Returns
    ///
    /// A `TaskResult` indicating what happened during processing.
    ///
    /// # Arguments
    ///
    /// * `peer` - The peer to test and potentially discover addresses from.
    /// * `peer_discovery_tx` - Channel to send discovered peers through.
    async fn process(
        &self,
        peer: Peer,
        peer_discovery_tx: mpsc::UnboundedSender<Vec<Peer>>,
    ) -> TaskResult {
        debug!("Processing peer {peer:?}");

        let mut conn = match timeout(
            self.config.peer_timeout,
            Connection::tcp(
                peer.clone(),
                self.config.network,
                ConnectionConfiguration::non_listening(
                    self.config.protocol_version,
                    self.config.transport_policy,
                    FeaturePreferences::default(),
                    self.config.user_agent.clone(),
                ),
            ),
        )
        .await
        {
            Ok(Ok(conn)) => conn,
            Ok(Err(_)) | Err(_) => {
                if self
                    .crawl_tx
                    .send(CrawlerMessage::NonListening(peer))
                    .await
                    .is_err()
                {
                    // Receiver dropped, stop processing.
                    return TaskResult::ChannelClosed;
                }
                return TaskResult::ConnectionFailed;
            }
        };

        // The connection has been established and handshake completed.
        // Services and version are updated in the peer.
        let peer_info = conn.peer().await;

        if self
            .crawl_tx
            .send(CrawlerMessage::Listening(peer_info))
            .await
            .is_err()
        {
            // Receiver dropped, stop processing.
            return TaskResult::ChannelClosed;
        }

        // Request peers from the connection
        match conn.get_peers(self.config.peer_timeout).await {
            Ok(discovered_peers) => {
                if discovered_peers.is_empty() {
                    return TaskResult::NoPeersFound;
                }

                // Filter out peers that have already been tested.
                // This is simply a performance optimization, deduplication
                // is ensured at the coordinator level.
                let mut filtered_peers = Vec::new();
                {
                    let tested = self.tested_peers.read().await;
                    for peer in discovered_peers {
                        if !tested.contains(&peer) {
                            filtered_peers.push(peer);
                        }
                    }
                }

                if !filtered_peers.is_empty() {
                    if peer_discovery_tx.send(filtered_peers).is_err() {
                        // Receiver dropped, stop processing
                        return TaskResult::ChannelClosed;
                    }
                    TaskResult::FoundPeers
                } else {
                    TaskResult::NoPeersFound
                }
            }
            Err(e) => {
                debug!("Failed to get peers from {peer}: {e}");
                TaskResult::NoPeersFound
            }
        }
    }

    /// Coordinates the crawling process by managing the work queue and task scheduling.
    ///
    /// This is the main control loop that orchestrates the entire crawling session.
    /// It continuously pulls peers from the discovery channel and spawns processing tasks
    /// until the crawl is complete or terminated.
    ///
    /// # Termination Conditions
    ///
    /// 1. **Natural Completion** - No more peers in channel and all tasks finished.
    /// 2. **Channel Closure** - Receiver dropped, indicating caller no longer interested.
    pub async fn coordinate(&self, seed: Peer) {
        // Channel to track discovered peers to process.
        // Use unbounded channel to prevent tasks from blocking on peer discovery
        let (peer_discovery_tx, mut peer_discovery_rx) = mpsc::unbounded_channel();
        // Channel to track task completion.
        let (task_done_tx, mut task_done_rx) =
            mpsc::channel::<TaskResult>(self.config.max_concurrent_tasks);

        // Prime the pump with the seed peer.
        if peer_discovery_tx.send(vec![seed]).is_err() {
            debug!("Failed to send seed peer");
            return;
        }

        // Number of in-flight tasks.
        let mut active_tasks = 0;

        let mut last_log_time = Instant::now();
        let log_interval = Duration::from_secs(60);

        loop {
            // Check if caller hung up before continuing.
            if self.crawl_tx.is_closed() {
                debug!("Receiver disconnected, stopping crawler");
                break;
            }

            // Periodic status logging, but can be crowded out by large peer batches.
            if last_log_time.elapsed() >= log_interval {
                let tested_count = self.tested_peers.read().await.len();
                info!(
                    "{} active tasks (max: {}), {} unique peers tested",
                    active_tasks, self.config.max_concurrent_tasks, tested_count
                );
                last_log_time = Instant::now();
            }

            // Wait for something to happen
            tokio::select! {
                // New peers discovered, can be up to 1,000 in a batch.
                Some(peers) = peer_discovery_rx.recv() => {
                    for peer in peers {
                        // Skip if already tested
                        if !self.tested_peers.write().await.insert(peer.clone()) {
                            continue;
                        }

                        // Wait if we're at capacity.
                        while active_tasks >= self.config.max_concurrent_tasks {
                            if let Some(result) = task_done_rx.recv().await {
                                active_tasks -= 1;
                                debug!("Task completed with result: {result:?}");
                            }
                        }

                        let session = self.clone();
                        let discovery_tx = peer_discovery_tx.clone();
                        let done_tx = task_done_tx.clone();

                        active_tasks += 1;
                        tokio::spawn(async move {
                            let result = session.process(peer, discovery_tx).await;
                            done_tx.send(result).await.expect("Coordinator should always be listening for task completion");
                        });
                    }
                }
                // Task completed.
                Some(result) = task_done_rx.recv() => {
                    active_tasks -= 1;
                    debug!("Task completed with result: {result:?}");

                    // Check if we're done: no more tasks running.
                    if active_tasks == 0 {
                        info!("Crawler exhausted - all peers processed");
                        break;
                    }
                }
            }
        }
    }
}
