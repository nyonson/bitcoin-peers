# Changelog

## v0.1.6

* Bump connection to v0.2.0.

## v0.1.5

* Fix initial race condition. Usually only caught in tests where mocks finish much quicker than real life connections.

## v0.1.4

* Fix deadlock issue when using a high concurrency setting.

## v0.1.3

* Support `Ping` messages in case it helps gather more addresses from peers.
* Fix minimum dependency break on tokio.

## v0.1.2

* Add periodic logging to coordinator.

## v0.1.1

* Add library documentation.

## v0.1.0

* Initial release of the `Crawler` module.
