<div align="center">
  <h1>Bitcoin Peers Crawler</h1>
  <p>
    <strong>Network peer discovery</strong>
  </p>

  <p>
    <a href="https://crates.io/crates/bitcoin-peers-crawler"><img alt="crates.io" src="https://img.shields.io/crates/v/bitcoin-peers-crawler.svg"/></a>
    <a href="https://docs.rs/bitcoin-peers-crawler"><img alt="Docs" src="https://img.shields.io/badge/docs-docs.rs-4d76ae"/></a>
    <a href="https://github.com/nyonson/bitcoin-peers/blob/master/LICENSE"><img alt="CC0 1.0 Universal Licensed" src="https://img.shields.io/badge/license-CC0--1.0-blue.svg"/></a>
    <a href="https://blog.rust-lang.org/2023/12/28/Rust-1.75.0/"><img alt="Rustc Version 1.75.0+" src="https://img.shields.io/badge/rustc-1.75.0%2B-lightgrey.svg"/></a>
  </p>
</div>

## About

Crawl across the bitcoin network beginning from a seed peer. Check out [the example](examples/crawler.rs) for usage.

## Performance

On a consumer-grade laptop, memory usage remains relatively constant no matter the `max_concurrent_tasks` setting, but CPU usage spikes do occur as you turn that knob up. 
