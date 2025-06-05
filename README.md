<div align="center">
  <h1>Bitcoin Peers</h1>
  <p>
    <strong>Tooling for the bitcoin p2p network</strong>
  </p>

  <p>
    <a href="https://github.com/nyonson/bitcoin-peers/blob/master/LICENSE"><img alt="CC0 1.0 Universal Licensed" src="https://img.shields.io/badge/license-CC0--1.0-blue.svg"/></a>
    <a href="https://github.com/nyonson/bitcoin-peers/actions?query=workflow%3ACI"><img alt="CI Status" src="https://github.com/nyonson/bitcoin-peers/actions/workflows/ci.yml/badge.svg"/></a>
    <a href="https://blog.rust-lang.org/2023/12/28/Rust-1.75.0/"><img alt="Rustc Version 1.75.0+" src="https://img.shields.io/badge/rustc-1.75.0%2B-lightgrey.svg"/></a>
  </p>
</div>

## About

Bitcoin Peers is a library containing general tooling for interacting with and analyzing peers of a bitcoin network. The library takes a semi-opinionated approach by requiring the use of the *tokio* asynchronous runtime. However, the library's modules are designed to compose into higher level utilities.

Check out [examples](examples) for usage and [doc/DESIGN.md](doc/DESIGN.md) for the gnarly details.

## Contributing

Guidelines are codified in the [justfile](justfile).

## License

The code in this project is licensed under [Creative Commons CC0 1.0](LICENSE).
