<div align="center">
  <h1>Bitcoin Peers Connection</h1>
  <p>
    <strong>Peer connection and transport</strong>
  </p>

  <p>
    <a href="https://github.com/nyonson/bitcoin-peers/blob/master/LICENSE"><img alt="CC0 1.0 Universal Licensed" src="https://img.shields.io/badge/license-CC0--1.0-blue.svg"/></a>
    <a href="https://blog.rust-lang.org/2022/08/11/Rust-1.63.0/"><img alt="Rustc Version 1.63.0+" src="https://img.shields.io/badge/rustc-1.63.0%2B-lightgrey.svg"/></a>
  </p>
</div>

## About

* **Connection** // High level interface to establish and manage a connection to a bitcoin peer. This includes transport selection (v1 or v2) and upgrading a connection's state based on the implicit feature negotiation (e.g. `SendAddrV2`). The caller's interface is a simple send or receive `NetworkMessage`.
* **Transport** // Low level interface for the encryption and serialization used in a connection. If you are connecting to nodes with some bespoke I/O implementations, perhaps this is the type for you!

Check out [examples/](examples) for usage.
