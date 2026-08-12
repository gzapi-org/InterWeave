# Adversarial security review closure - 2026-08-12

This memo records the repository changes made after the full frozen-set adversarial review. It is explanatory; the referenced contracts/ADRs are normative.

## Baseline/export correction

The Phase-9 architecture is present in commit `ea8ca00`; this revision is built from that committed archive baseline and then amended. The handoff package must be generated from the clean committed tree and independently unzipped/verified before delivery.

## Closed security findings

### S1 - GossipSub mesh duplicate identity

Closed in `transport/libp2p/PUBSUB.md` and ADR-0004. `GossipSubMessageIdV1` is the full SHA-256 of a domain-separated canonical tuple containing the signed source PeerId raw bytes plus the GossipSub wire sequence number. The application envelope ID is deliberately excluded. A golden fixture, a Phase-2 cross-publisher same-envelope-ID test, and a pre-cache signature/source validation-order test are mandatory.

### S2 - admin/data-plane authority enforcement

Closed by ADR-0037 and `contracts/LOCAL-IPC.md`. IPC v2 has separate data and admin sockets. The data socket can never grant `admin.*` regardless of `client.kind`; the admin socket cannot acquire EndpointId leases or ordinary application messaging authority. Default same-UID access to the admin socket remains residual and is the target of SPIKE-005.

### S3 - unauthenticated pre-Noise handshake flood

Closed at architecture level with 64 global/8 per-source pending defaults, 10-second handshake timeout, per-source/global start-rate budgets, failure/observability rows, and deployment firewall/eBPF residual guidance. No PeerId state is created before successful authentication.

### S4 - trusted-PeerId address/backoff pollution

Closed in ADR-0011. Failure/backoff is address-scoped where appropriate; identity mismatch quarantines the address (30 minutes default) without advancing expected trusted PeerId punitive backoff while a known-good address remains eligible.

### S5 - remote-supplied endpoint metadata validation

Closed in DIRECT/ENDPOINTS contracts. AcceptedV2 response message ID/resolved EndpointId are validated before cache/tool/UI use. Explicit destination acceptance must echo the exact requested endpoint. Endpoint-directory responses reject >32/invalid/duplicate entries, accept-but-sort valid unsorted entries, clamp TTL to local/hard ceiling, and age from local receipt time.

### S6 - no_route timing oracle

The architecture accepts timing as residual rather than claiming constant-time route evaluation. All denial branches share the same code/shape/encoder and expose no detailed wire reason; mandatory per-PeerId direct token buckets bound probing. Artificial fixed sleeps are not required.

### S7 - keepalive nonce

IPC keepalive uses a 128-bit CSPRNG nonce, one outstanding probe per connection, exact-current-nonce pong matching, and ignores stale/duplicate/wrong pongs. It remains liveness only, not authentication.

## Carry-over verification

The reconstructed Phase-9 baseline already contains the earlier requested freeze fixes: `media_type_len=0` means absent; endpoint leases require keepalive by default with an explicit policy option; `transportctl identity verify` is read-only; and full disaster recovery is the 24-word phrase plus separate `config.yaml` backup.

## Additional hardening decisions

- Phase 7 includes per-trusted-PeerId direct ingress token buckets (120/minute, burst 32) plus global 1200/minute, burst 256 defaults.
- ADR-0038 promotes encrypted exportable software-key storage from an unnamed revisit to an explicit optional v2.x direction. Standard v1 remains filesystem-only. SPIKE-007 must select an audited maintained password-encryption envelope; the project will not design bespoke cryptography.
- SPIKE-005 now evaluates residual same-UID access specifically against the split admin socket boundary.
- Direct ingress rate limiting is explicitly ordered before dedup lookup; a rate-limited retry may receive `overloaded`, but cannot re-enqueue and cannot erase an already-positive dedup entry. A later admitted retry still receives the stored acceptance route.

## Freeze impact

S1 changes network compatibility behavior and therefore belongs to the freeze. S2 changes the IPC authority topology and belongs before Phase 5 scaffolding. S3/S4/direct-rate hardening are Phase 4/7 normative design. The encrypted key envelope is not a v1 release gate.
