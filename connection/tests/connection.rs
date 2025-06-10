//! Integration tests for the Connection module using a bitcoind process.
//!
//! These tests require a Bitcoin node to be available. On NixOS, you may need
//! to use the BITCOIND_EXE environment variable to point to a NixOS-compatible bitcoind
//! binary instead of the auto-downloaded one.

use bitcoin::p2p::address::AddrV2;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::Network;
use bitcoin_peers_connection::{
    Connection, ConnectionConfiguration, FeaturePreferences, Peer, PeerProtocolVersion,
    TransportPolicy,
};
use corepc_node as bitcoind;
use std::net::Ipv4Addr;
use std::time::Duration;
use tokio::time::timeout;

const NONCE: u64 = 42;

enum TransportVersion {
    V1,
    V2,
}

/// Fire up a managed regtest bitcoind process.
fn regtest(transport: TransportVersion) -> bitcoind::Node {
    // Pull executable from auto-downloaded location, unless
    // environment variable override is present.
    let exe_path = bitcoind::exe_path().unwrap();
    println!("Using bitcoind at {exe_path}");
    let mut conf = bitcoind::Conf::default();

    // Explicitly use regtest network.
    conf.args.push("-regtest");

    // Enable V2 if requested, otherwise disable.
    match transport {
        TransportVersion::V2 => conf.args.push("-v2transport=1"),
        TransportVersion::V1 => conf.args.push("-v2transport=0"),
    }

    // Enable p2p port for tests.
    conf.p2p = bitcoind::P2P::Yes;
    bitcoind::Node::with_conf(&exe_path, &conf).unwrap()
}

#[tokio::test]
async fn test_connection_v1() {
    // Start a regtest node with V1 transport only.
    let node = regtest(TransportVersion::V1);
    let socket = node.params.p2p_socket.unwrap();
    let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), socket.port());

    // Configure connection to prefer V2 but fall back to V1.
    let config = ConnectionConfiguration::non_listening(
        PeerProtocolVersion::Known(70016),
        TransportPolicy::V2Preferred,
        FeaturePreferences::default(),
        Some("bitcoin-peers-test".to_string()),
    );

    // Establish connection (should fall back to V1).
    let mut connection = Connection::tcp(peer, Network::Regtest, config)
        .await
        .expect("Failed to establish connection");

    // The connection should be established and handshake completed.
    // Send a ping message.
    connection
        .send(NetworkMessage::Ping(NONCE))
        .await
        .expect("Failed to send ping");

    // Wait for pong response.
    let response = loop {
        let msg = timeout(Duration::from_secs(5), connection.receive())
            .await
            .expect("Timeout waiting for response")
            .expect("Failed to receive message");

        if let NetworkMessage::Pong(nonce) = msg {
            break NetworkMessage::Pong(nonce);
        }
    };

    match response {
        NetworkMessage::Pong(nonce) => assert_eq!(nonce, NONCE),
        _ => panic!("Expected Pong message, got {response:?}"),
    }
}

#[tokio::test]
async fn test_connection_v2() {
    // Start a regtest node with v2 transport enabled.
    let node = regtest(TransportVersion::V2);
    let socket = node.params.p2p_socket.unwrap();
    let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), socket.port());

    // Configure connection to require v2.
    let config = ConnectionConfiguration::non_listening(
        PeerProtocolVersion::Known(70016),
        TransportPolicy::V2Required,
        FeaturePreferences::default(),
        Some("bitcoin-peers-test".to_string()),
    );

    let mut connection = Connection::tcp(peer, Network::Regtest, config)
        .await
        .expect("Failed to establish V2 connection");

    connection
        .send(NetworkMessage::Ping(NONCE))
        .await
        .expect("Failed to send ping");

    let response = loop {
        let msg = timeout(Duration::from_secs(5), connection.receive())
            .await
            .expect("Timeout waiting for response")
            .expect("Failed to receive message");

        if let NetworkMessage::Pong(nonce) = msg {
            break NetworkMessage::Pong(nonce);
        }
    };

    match response {
        NetworkMessage::Pong(nonce) => assert_eq!(nonce, NONCE),
        _ => panic!("Expected Pong message, got {response:?}"),
    }
}

#[tokio::test]
async fn test_connection_split() {
    let node = regtest(TransportVersion::V1);
    let socket = node.params.p2p_socket.unwrap();
    let peer = Peer::new(AddrV2::Ipv4(Ipv4Addr::new(127, 0, 0, 1)), socket.port());

    let config = ConnectionConfiguration::non_listening(
        PeerProtocolVersion::Known(70016),
        TransportPolicy::V2Preferred,
        FeaturePreferences::default(),
        Some("bitcoin-peers-test".to_string()),
    );

    let connection = Connection::tcp(peer, Network::Regtest, config)
        .await
        .expect("Failed to establish connection");
    let (mut receiver, mut sender) = connection.into_split();

    sender
        .send(NetworkMessage::Ping(NONCE))
        .await
        .expect("Failed to send ping");

    let mut received_pong = false;
    for _ in 0..10 {
        match receiver.receive().await {
            Ok(msg) => {
                println!("Received message: {msg:?}");
                match msg {
                    NetworkMessage::Pong(nonce) if nonce == NONCE => {
                        received_pong = true;
                        break;
                    }
                    NetworkMessage::Ping(nonce) => {
                        sender
                            .send(NetworkMessage::Pong(nonce))
                            .await
                            .expect("Failed to send pong");
                    }
                    _ => {}
                }
            }
            Err(e) => {
                if received_pong {
                    break;
                }
                panic!("Connection error before receiving pong: {e:?}");
            }
        }
    }

    assert!(received_pong, "Did not receive expected pong");
}
