<div align="center">
  <h1>Bitcoin Peers</h1>
  <p>
    <strong>Tooling for the bitcoin p2p network</strong>
  </p>

  <p>
    <a href="https://github.com/nyonson/bitcoin-peers/blob/master/LICENSE"><img alt="CC0 1.0 Universal Licensed" src="https://img.shields.io/badge/license-CC0--1.0-blue.svg"/></a>
    <a href="https://github.com/nyonson/bitcoin-peers/actions?query=workflow%3ACI"><img alt="CI Status" src="https://github.com/nyonson/bitcoin-peers/actions/workflows/ci.yml/badge.svg"/></a>
  </p>
</div>

## About

Bitcoin Peers is a set of libraries for interacting with and analyzing peers of a bitcoin network. The libraries take a semi-opinionated approach by requiring the use of the *tokio* asynchronous runtime. However, they have minimal dependencies beyond that and are designed to compose into higher level utilities.

### Modules

* [connection](connection) // Connect to a peer on the bitcoin network.
* [crawler](crawler) // Discover peers on the bitcoin network.

## Contributing

Guidelines are codified in the [justfile](justfile).

## License

The code in this project is licensed under [Creative Commons CC0 1.0](LICENSE).
