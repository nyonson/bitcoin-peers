## Bitcoin Peers Connection

* **Connection** // High level interface to establish and manage a connection to a bitcoin peer. This includes transport selection (v1 or v2) and upgrading a connection's state based on the implicit message exchanges (e.g. `SendAddrV2`). The caller's interface is a simple send or receive `NetworkMessage`.
* **Transport** // Low level interface for the encryption and serialization used in a connection. If you are connecting to nodes with some bespoke I/O implementations, perhaps this is the type for you!

Check out [examples/](examples) for usage.
