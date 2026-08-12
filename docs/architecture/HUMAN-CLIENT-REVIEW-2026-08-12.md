# Human desktop + Android architecture / V-review closure — 2026-08-12

Status: design closure memo.

## V1 — AutoNAT server SSRF / dial-back scope

Closed. AutoNAT server dial-back accepts only literal-IP candidates whose IP equals the requester's observed transport source IP; loopback, unspecified, multicast, link-local, private/ULA and other non-global/special-use destinations are rejected under the default Internet service policy. DNS candidates are not resolved for server dial-back. Phase-9 conformance includes authorized-client attempts to request internal/loopback/other-public-IP targets.

## V2 — implicit Identify infrastructure promotion

Closed by default posture change: `use_authorized_identify_servers=false` and `use_authorized_identify_relays=false`. Static configured infrastructure is the default source. Explicit opt-in may add fresh Identify-learned candidates from already-authorized infrastructure/data peers, but statically configured candidates are selected first until their targets cannot be met.

## V3 — relayed pre-Noise accounting

Closed. When original source IP is unavailable for a circuit-borne inbound handshake, pre-auth accounting buckets by the authenticated relay transport connection/relay PeerId plus the global pending/rate caps. Relay server circuit-per-source quotas are complementary and do not replace destination-side pre-auth limits.

## V4 — relay service admission

Closed. Standard project relay servers accept reservations/circuits only from explicitly admitted `DataPlaneTrusted` or `ConnectivityInfrastructureOnly` peers. Open anonymous relay service requires a separate deployment policy/ADR and stronger abuse controls.

## V5 — connectivity() capability

Closed. `connectivity()` and normalized `server_state.connectivity` require ordinary `commands`; human and Claude data clients already receive it. Raw infrastructure topology remains diagnostics/admin only.

## V6 — DCUtR path event

Closed. A successful hole punch does not emit a second logical `PeerConnected` for an already-connected peer. Runtime emits `PeerPathChanged { peer, previous: relayed, current: direct, reason: dcutr, observed_at }` after the direct path satisfies the stability gate, plus any coalesced `ConnectivityChanged`. `PeerConnected` remains logical peer transition from no usable application connection to usable connection.

## Cross-platform human client decisions

- first-party human client logic/UI is Rust; Slint selected as reference desktop/Android UI;
- desktop uses daemon + IPC v2 + split admin socket;
- Android embeds the same Rust runtime in a foreground service and uses a neutral in-process local-session adapter;
- Android stay-reachable mode is user-visible/user-opt-in; no hidden FCM/cloud wake-up dependency;
- Android wraps the exact portable Ed25519 seed with an Android-Keystore AES-GCM key;
- desktop and Android devices use distinct PeerIds when concurrently active;
- application SQLite/history and HumanChatV1 remain above transport.
