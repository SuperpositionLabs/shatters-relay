<p align="center">
  <img src="assets/branding.svg" alt="shatters relay" width="300"/>
</p>

<p align="center">
  <strong>Zero-knowledge QUIC relay for end-to-end encrypted messaging.</strong>
</p>

<p align="center">
  <a href="#features">Features</a>
  <a href="#getting-started">Getting Started</a>
  <a href="#configuration">Configuration</a>
  <a href="#license">License</a>
</p>

---

Relay server for the [Shatters](https://github.com/SuperpositionLabs/shatters) communication system. Routes opaque ciphertext between clients over QUIC. Privacy by architecture, not by policy.

## Features

| Feature | Description |
|---|---|
| **Zero-Knowledge** | No plaintext, no private keys, no user accounts. Channels are opaque 32-byte hashes |
| **QUIC Transport** | Multiplexed bidirectional streams via [quinn](https://github.com/quinn-rs/quinn) + TLS 1.3 |
| **Ed25519 Auth** | Per-connection authentication with channel-scoped proof signatures |
| **Pub/Sub** | Real-time fan-out delivery over channel identifiers |
| **Dead Drop** | TTL-enforced encrypted envelope storage for offline message retrieval |
| **Pre-Key Bundles** | Ephemeral in-memory X3DH bundle storage with atomic one-time key consumption |
| **Rate Limiting** | Lock-free token bucket, no user tracking, differentiated cost by operation |

## Getting Started

### Prerequisites

| Tool | Version | Install |
|---|---|---|
| Rust | >= 1.75 | [rustup.rs](https://rustup.rs/) |

No other system dependencies — all crates are pure Rust.

### Build

```bash
git clone https://github.com/SuperpositionLabs/shatters-relay.git
cd shatters-relay

cargo build --release
```

### Run

```bash
# default configuration
./target/release/shatters-relay

# custom config file
./target/release/shatters-relay --config deploy/config.toml
```

### Development TLS

For local testing, generate self-signed TLS certificates with the `dev-tls` feature:

```bash
cargo run --features dev-tls
```

### Audit

Supply-chain auditing is configured via [cargo-deny](https://github.com/EmbarkStudios/cargo-deny):

```bash
cargo install cargo-deny
cargo deny check
```

## Dependencies

| Crate | Purpose |
|---|---|
| [tokio](https://tokio.rs/) | Async runtime |
| [quinn](https://github.com/quinn-rs/quinn) | QUIC transport |
| [rustls](https://github.com/rustls/rustls) | TLS 1.3 |
| [ed25519-dalek](https://github.com/dalek-cryptography/curve25519-dalek) | Signature verification |
| [dashmap](https://github.com/xacrimon/dashmap) | Lock-free concurrent maps |
| [parking_lot](https://github.com/Amanieu/parking_lot) | Synchronization primitives |
| [tracing](https://github.com/tokio-rs/tracing) | Structured diagnostics |
| [clap](https://github.com/clap-rs/clap) | CLI parsing |
| [serde](https://serde.rs/) + [toml](https://github.com/toml-rs/toml) | Configuration |

## License

GPLv3 - see [LICENSE](LICENSE).
