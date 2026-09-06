# bit-torrent

A lightweight, robust BitTorrent client implemented in Rust.

## Features
- Complete Bencode parser and encoder (`bencode.rs`)
- Torrent metainfo decoding and hashing (`torrent.rs`)
- Multi-protocol Tracker client supporting HTTP & UDP announcing (`tracker.rs`)
- Peer-to-peer wire protocol framing, handshake, bitfield, request, and piece handling (`peer.rs`)
- Concurrent piece downloading and verification pipeline (`download.rs`)

## Build & Run

```bash
cargo build --release
```

Download a torrent:
```bash
cargo run --release -- download -t sample.torrent -o output/
```
