# Architecture amendment review — 2026-08-11

This memo records the contract amendments made after a full cross-document review of the original architecture set. It is a review aid; normative decisions live in the cited contracts/ADRs.

## Blocking issues closed

### 1. IPC max payload representability

**Finding:** a 49,152-byte payload becomes 65,536 base64url characters, so the previous 65,536-byte JSON-frame ceiling left no room for the envelope.

**Resolution:** keep the 49,152-byte transport hard ceiling; increase IPC JSON body ceiling to 131,072 bytes; bound envelope fields; require exact max-payload golden request/event fixtures.

Normative: `contracts/LOCAL-IPC.md`, ADR-0017, ADR-0026.

### 2. Trust versus broadcast confidentiality/connection participation

**Finding:** inbound-only trust allowed an untrusted-but-connected peer to participate in GossipSub and potentially receive plaintext after guessing a topic.

**Resolution:** v1 ordinary data-plane connectivity is trust-gated. ConnectionManager does not dial/retain unauthorized PeerIds; outbound `send` rejects unauthorized targets before dial; revocation evicts active data-plane connections. Discovery remains independent and may retain unauthorized candidates as bounded observations.

Normative: ADR-0011, ADR-0012, `contracts/TRANSPORT.md`.

### 3. GossipSub validation-result semantics

**Finding:** local trust rejection was not mapped to GossipSub `Accept|Ignore|Reject`, even though the result changes propagation/scoring.

**Resolution:** ADR-0029 fixes the mapping: objective invalidity -> `Reject`; valid but locally unauthorized original publisher -> `Ignore`; valid authorized publisher -> `Accept`. Authorization mismatch does not become objective invalidity.

Normative: ADR-0029, `transport/libp2p/PUBSUB.md`.

## Contract-level consistency fixes

| Finding | Resolution |
|---|---|
| `NotConnected` referenced but undefined | removed; `PeerUnknown` and `PeerUnreachable` have explicit mutually useful semantics |
| dedup cache key drift | canonical `(mode, source_peer, channel_or_none, message_id)` |
| capability max payload fixed despite config | capability reports effective profile setting, ceiling 49,152 |
| trust-change event referenced but absent | added `TrustPolicyChanged { revision, at }` plus policy disconnect semantics |
| `media_type` vs `content_type` drift | transport/libp2p use `media_type`; Claude layer uses `content_type`; mapping explicit |
| `sent_at` semantics differ | diagnostic only in both modes; not replay/order/auth input |
| MessageId width drift | exactly 128 bits in transport v1/direct wire |

## Previously underspecified behavior pinned down

| Question | v1 ruling |
|---|---|
| enabled Kademlia on a build without provider | hard config/startup failure; disabled reserved entry allowed |
| broadcast without join | fail `ChannelNotJoined`; no implicit join |
| IPC shutdown | requires `admin.shutdown`; `claude-channel` never receives it |
| DNS resolution for static bootstrap | unresolved `/dns*` hint emitted by provider; dial layer owns resolution failure diagnostics |
| broadcast reply after `leave` | `ChannelNotJoined`; reply token does not recreate subscription |
| discovery `confidence` | removed; provenance/freshness/provider priority remain explicit |

## Review impact

Phase 1 must freeze and test the payload/IPC invariant, exact 128-bit MessageId, effective capability reporting, config rejection semantics, and IPC administrative capability model. Phase 2 must test all three GossipSub validation outcomes and trust-gated data-plane connectivity before broader mesh tuning.
