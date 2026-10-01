# transport-client

The human client's neutral `LocalDataSession` facade (plan §17 (1); the blueprint's `human-transport-client`): generic over `DataSessionBinding`, it owns the client's half of retention -- commit-pending, send, transport-terminal; drain, commit-unread, present; re-open with backoff when a binding's connection has ended; the degraded-storage reaction; the byte-identical retry with the same transport `MessageId`. It depends on `local-client-api`, `transport-api`, `human-core`, `human-store` and `chat-protocol`, and names nothing under `crates/transport/*`, no libp2p and no Slint.

**Current status:** planned crate boundary only; no `Cargo.toml` or Rust source yet. Stage 14 batch 5 builds it, against `tests/local-client-fake`.
