//! Example of using the bitcoin-peers crawler.

use bitcoin::p2p::{address::AddrV2, ServiceFlags};
use bitcoin::Network;
use bitcoin_peers::{CrawlerBuilder, Peer};
use clap::Parser;
use log::LevelFilter;
use simplelog::{ColorChoice, Config, TermLogger, TerminalMode};
use std::net::IpAddr;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// IP address of the seed node to connect to.
    #[arg(short, long)]
    address: String,

    /// Port number of the seed node.
    #[arg(short, long, default_value_t = 8333)]
    port: u16,

    /// Custom user agent (optional).
    #[arg(short, long)]
    user_agent: Option<String>,

    /// Log level.
    #[arg(short, long, default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

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

    log::info!("CRAWLING THE BITCOIN NETWORK");

    let ip_addr = args
        .address
        .parse::<IpAddr>()
        .map_err(|_| format!("Invalid IP address: {}", args.address))?;

    let addr = match ip_addr {
        IpAddr::V4(ipv4) => AddrV2::Ipv4(ipv4),
        IpAddr::V6(ipv6) => AddrV2::Ipv6(ipv6),
    };

    log::debug!("Connecting to seed peer at {}:{}", args.address, args.port);

    let mut builder = CrawlerBuilder::new(Network::Bitcoin);
    if let Some(user_agent) = args.user_agent.clone() {
        log::debug!("Using custom user agent: {user_agent}");
        builder = builder.with_user_agent(user_agent)?;
    }
    let crawler = builder.build();

    let seed = Peer {
        address: addr,
        port: args.port,
        services: ServiceFlags::NONE,
    };

    let mut peers_rx = crawler
        .crawl(seed)
        .await
        .map_err(|e| format!("Crawler error: {e}"))?;

    while let Some(peer_msg) = peers_rx.recv().await {
        log::info!("{peer_msg}");
    }

    Ok(())
}
