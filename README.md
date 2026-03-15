<p align="center">
  <img src="assets/branding.svg" alt="shatters logo" width="300"/>
</p>

<p align="center">
  <strong>Zero-knowledge QUIC relay server for end-to-end encrypted messaging.</strong>
</p>

<p align="center">
  <a href="#architecture">Architecture</a> &middot;
  <a href="#features">Features</a> &middot;
  <a href="#getting-started">Getting Started</a> &middot;
  <a href="#configuration">Configuration</a> &middot;
  <a href="#license">License</a>
</p>

---

The relay server behind [shatters](https://github.com/SuperpositionLabs/shatters). Routes encrypted messages between clients without the ability to read, decrypt, or infer their contents. All payloads are opaque ciphertext. All routing keys are 32-byte hashes with no metadata attached. The server holds no user accounts, no keys, no plaintext by design, not by policy.

## Features

- **Zero-Knowledge**: the server never sees plaintext, never holds private keys, and cannot correlate users to channels. All routing keys are opaque 32-byte hashes
- **QUIC**: multiplexed bidirectional streams over [quinn](https://github.com/quinn-rs/quinn)
- **Pub/Sub Fan-Out**: real-time message delivery over 32-byte channel identifiers
- **Dead Drop**: TTL-enforced, per-channel encrypted envelope storage for asynchronous message retrieval
- **X3DH Pre-Key Bundles**: ephemeral in-memory storage for key agreement with atomic one-time pre-key consumption
- **Rate Limiting**: custom lock-free token bucket, no user tracking, differentiated cost by operation type

## Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) 1.75+ (edition 2021)

### Build

```bash
git clone https://github.com/SuperpositionLabs/shatters-relay.git
cd shatters-relay

cargo build --release
```

### Run

```bash
# with default config path (config.toml)
./target/release/shatters-relay

# with custom config
./target/release/shatters-relay --config deploy/config.toml
```

## Configuration

All settings are configured via a single TOML file. Every field has a sensible default.

```toml
[server]
listen_addr = "0.0.0.0:4433"
tls_cert_path = "certs/server.crt"
tls_key_path = "certs/server.key"
max_auth_timeout_secs = 10
max_idle_timeout_secs = 30

[deaddrop]
default_ttl_secs = 86_400       # 24 hours
max_per_drop = 1_000
cleanup_interval_secs = 60

[prekey]
bundle_ttl_secs = 604_800       # 7 days
max_prekeys_per_user = 100
max_bundle_size_bytes = 4096
cleanup_interval_secs = 300

[limits]
max_subscriptions_per_conn = 50
requests_per_second = 100
burst_size = 200

[logging]
level = "info"
format = "json"                 # "json" or "pretty"
```

## Dependencies

| Crate | Purpose |
|---|---|
| [tokio](https://tokio.rs/) | Async runtime |
| [quinn](https://github.com/quinn-rs/quinn) | QUIC |
| [rustls](https://github.com/rustls/rustls) | TLS 1.3 |
| [dashmap](https://github.com/xacrimon/dashmap) | Lock-free concurrent maps |
| [parking_lot](https://github.com/Amanieu/parking_lot) | Efficient synchronization primitives |
| [tracing](https://github.com/tokio-rs/tracing) | Structured diagnostics |
| [clap](https://github.com/clap-rs/clap) | CLI argument parsing |
| [serde](https://serde.rs/) + [toml](https://github.com/toml-rs/toml) | Configuration deserialization |

## License

GNU General Public License v3.0 — see [LICENSE](LICENSE).
