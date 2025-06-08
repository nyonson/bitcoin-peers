//! Example demonstrating split connection functionality with ping/pong.
//!
//! This example shows how to split a connection into separate receiver and sender
//! halves, allowing for concurrent reading and writing operations. It connects
//! to a Bitcoin peer and demonstrates sending pings and receiving pongs.

use bitcoin::p2p::address::AddrV2;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::Network;
use bitcoin_peers::{Connection, ConnectionConfiguration, Peer, PeerProtocolVersion};
use clap::Parser;
use log::{debug, error, info, LevelFilter};
use simplelog::{ColorChoice, Config, TermLogger, TerminalMode};
use std::net::IpAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;

/// Command line arguments for the split connection example
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// IP address of the seed node to connect to.
    #[arg(short, long)]
    address: String,

    /// Port number of the seed node.
    #[arg(short, long, default_value_t = 8333)]
    port: u16,

    /// Network to use (bitcoin (mainnet), testnet, regtest, signet)
    #[arg(short, long, default_value = "bitcoin")]
    network: String,

    /// Protocol version to advertise
    #[arg(short = 'v', long, default_value_t = 70016)]
    protocol_version: u32,

    /// User agent string to advertise
    #[arg(short, long)]
    user_agent: Option<String>,

    /// Log level.
    #[arg(short, long, default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let network = match args.network.to_lowercase().as_str() {
        "bitcoin" => Network::Bitcoin,
        "testnet" => Network::Testnet,
        "regtest" => Network::Regtest,
        "signet" => Network::Signet,
        _ => {
            error!(
                "Invalid network: {}. Use bitcoin, testnet, regtest, or signet",
                args.network
            );
            return Ok(());
        }
    };

    let log_level = match args.log_level.to_lowercase().as_str() {
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "info" => LevelFilter::Info,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        _ => LevelFilter::Info,
    };

    TermLogger::init(
        log_level,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )
    .unwrap();

    info!("PING AND PONGS");
    info!("Network: {}", network);

    let ip_addr = args
        .address
        .parse::<IpAddr>()
        .map_err(|_| format!("Invalid IP address: {}", args.address))?;

    let addr_v2 = match ip_addr {
        IpAddr::V4(ipv4) => AddrV2::Ipv4(ipv4),
        IpAddr::V6(ipv6) => AddrV2::Ipv6(ipv6),
    };

    let peer = Peer::new(addr_v2, args.port);

    let config = ConnectionConfiguration::non_listening(
        PeerProtocolVersion::Known(args.protocol_version),
        args.user_agent.clone(),
    );

    let connection = match Connection::tcp(peer, network, config).await {
        Ok(conn) => {
            info!("Successfully connected and completed handshake");
            conn
        }
        Err(e) => {
            error!("Failed to connect: {}", e);
            return Ok(());
        }
    };

    let (mut receiver, mut sender) = connection.split();
    info!("Connection split into receiver and sender");

    // Set up shutdown signal using broadcast channel for multiple receivers
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let mut shutdown_rx1 = shutdown_tx.subscribe();
    let mut shutdown_rx2 = shutdown_tx.subscribe();

    // Channel for communicating between tasks.
    let (ping_tx, mut ping_rx) = mpsc::channel::<u64>(10);

    // Spawn a task to handle receiving messages.
    let receive_handle = tokio::spawn(async move {
        info!("Receiver task started");

        loop {
            tokio::select! {
                // Handle shutdown signal
                _ = shutdown_rx1.recv() => {
                    info!("Receiver shutting down");
                    break;
                }
                // Handle incoming messages
                result = receiver.receive() => {
                    match result {
                        Ok(msg) => match msg {
                            NetworkMessage::Ping(nonce) => {
                                // Received a Ping from peer, tell the send task to Pong them.
                                info!("Received Ping with nonce: {}", nonce);
                                if let Err(e) = ping_tx.send(nonce).await {
                                    error!("Failed to send ping nonce to sender task: {}", e);
                                    break;
                                }
                            }
                            NetworkMessage::Pong(nonce) => {
                                // Received a Pong response from our Ping.
                                info!("Received Pong with nonce: {}", nonce);
                            }
                            _ => {
                                debug!("Received message: {:?}", msg);
                            }
                        },
                        Err(e) => {
                            error!("Error receiving message: {}", e);
                            break;
                        }
                    }
                }
            }
        }

        info!("Receiver task ended");
    });

    // Spawn a task to send pings and respond to incoming pings with pongs.
    let send_handle = tokio::spawn(async move {
        info!("Sender task started");

        // Set up ping interval.
        let mut ping_interval = interval(Duration::from_secs(5));
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut ping_count = 0u32;
        let mut nonce = 1u64;

        loop {
            tokio::select! {
                // Handle shutdown signal.
                _ = shutdown_rx2.recv() => {
                    info!("Sender shutting down");
                    break;
                }
                // Handle incoming ping requests that need pong responses.
                Some(ping_nonce) = ping_rx.recv() => {
                    if let Err(e) = sender.send(NetworkMessage::Pong(ping_nonce)).await {
                        error!("Failed to send Pong: {}", e);
                        break;
                    }
                    info!("Sent Pong response with nonce: {}", ping_nonce);
                }
                // Send periodic pings.
                _ = ping_interval.tick() => {
                    // Send a ping
                    if let Err(e) = sender.send(NetworkMessage::Ping(nonce)).await {
                        error!("Failed to send Ping: {}", e);
                        break;
                    }
                    ping_count += 1;
                    info!("Sent Ping #{} with nonce: {}", ping_count, nonce);
                    nonce = nonce.wrapping_add(1);
                }
            }
        }

        info!("Sender task ended");
    });

    // Set up Ctrl+C handler.
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                info!("Received Ctrl+C, shutting down...");
                let _ = shutdown_tx.send(());
            }
            Err(err) => {
                error!("Unable to listen for shutdown signal: {}", err);
            }
        }
    });

    let (receiver_result, sender_result) = tokio::join!(receive_handle, send_handle);

    if let Err(e) = receiver_result {
        error!("Receiver task panicked: {}", e);
    }
    if let Err(e) = sender_result {
        error!("Sender task panicked: {}", e);
    }

    info!("Connection closed");
    Ok(())
}
