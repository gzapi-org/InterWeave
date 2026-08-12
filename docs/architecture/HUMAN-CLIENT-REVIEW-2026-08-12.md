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

## W-review closure

### W1 — Android recovery-phrase UI surface

Closed. Recovery display/import uses secure-window protection for the full sensitive-screen lifetime; standard v1 has no mnemonic clipboard path and no normal free-text 24-word entry. Import uses an in-app BIP-39 word-list picker. Any temporary IME-assisted filtering requests no suggestions/personalized learning but is defense in depth only. Phrase material is prohibited from recents/task snapshots, saved state, logs, analytics, crash artifacts and notifications; SPIKE-008/009 verify the platform binding.

### W2 — Android Auto Backup/device transfer

Closed. Standard-v1 Android packaging does not use system backup as recovery. Application backup is explicitly disabled and Android 12+ data-extraction plus supported pre-12 backup rules explicitly exclude the wrapped identity envelope, transport/trust configuration, recovery temporary state and human SQLite database from cloud backup and device transfer. A future history backup/sync is a separate opt-in application-security design.

### W3 — user-presence + stay-reachable

Closed. The combination remains valid, but status/UI must expose `background_restart_requires_user_authentication=true`; it cannot be described as automatically reachable after process/service restart.

### W4 — resource-limit scoping

Closed. Resource-limit documentation now distinguishes daemon-IPC-only rows from deployment-neutral LocalDataSession/transport limits. Android retains the same bounded queues, commands, direct in-flight/rate/dedup and network ceilings.

### W5 — HumanChatV1 fixture precision

Closed. `app_message_id` and `reply_to` are exactly 16-byte IDs rendered as 32 lowercase hex characters; `sent_at_ms` is bounded to `0..253402300799999` and remains diagnostic only; unknown `reply_to` is valid and renders without network lookup or rejection.
