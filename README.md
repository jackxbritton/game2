# Code scrap for a multiplayer game in Rust.

This was a small multiplayer game I worked on for a little more than a weekend. May pick it up again in the future. The code is pretty messy, you have been warned.

Objectives were:

* Shared structs for talking between client and server.
* [bincode](https://docs.rs/bincode/1.3.1/bincode/index.html) for lightweight serialization over the wire.
* [tokio](https://tokio.rs/) for easy concurrency.
* Get at least a little bit into clientside interpolation and prediction.
* UDP for quickness.

For latency reasons you probably don't want to use tokio for a realtime game, but it was fun to learn tokio.

