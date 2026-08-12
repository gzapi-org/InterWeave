# Model B Phase-1 freeze review — 2026-08-12

Scope: closure of the final non-blocking review findings plus optional human-recoverable transport identity backup.

## Contract precision closures

| Finding | Frozen decision |
|---|---|
| endpoint handshake mapping | malformed=`InvalidArgument`; absent=`EndpointUnknown`; disabled=`EndpointDisabled`; client-kind mismatch=`EndpointClientKindDenied`; collision=`EndpointInUse`; unauthorized capability=`CapabilityDenied` |
| `endpoints.query` grants | human-client gets it by default only when directory enabled; claude-channel does not; admin clients only by explicit policy |
| direct dedup reservation bound | 128 global / 8 per source PeerId defaults; 512 / 32 ceilings; overflow=`overloaded`/`Overloaded` |
| fingerprint canonicalization | DirectContentFingerprintV1 fixed binary framing + SHA-256 golden fixture |
| IPC version in config | removed; IPC major/minor is hello negotiation, not operator profile config |
| client slot accounting | every IPC connection counts; human data + admin sessions consume two slots |
| wedged lease liveness | negotiated ping/pong keepalive; defaults 30s/10s/3 misses; EndpointId leases require negotiation by default |
| broadcast endpoint authorship | remains transport PeerId-only; non-normative first-party app envelope guidance added |
| Claude `peer_endpoints` | explicitly deferred; no v2 tool and no default `endpoints.query` grant |
| request-response dedup race | SPIKE-002 must exercise concurrent same-key retransmissions on real rust-libp2p scheduler |

## Identity recovery decision

ADR-0033 fixes the initial software identity algorithm to Ed25519 and defines `cp2p-ed25519-bip39-entropy-v1`:

- 24 English words encode the exact 32-byte Ed25519 secret seed using BIP-39 entropy/checksum mapping;
- BIP-39 PBKDF2 wallet seed derivation and passphrase semantics are not used;
- recovery record carries public expected PeerId for strong exact-identity verification;
- export/restore is offline local identity-file tooling, never daemon IPC/MCP/Channel/network;
- phrase theft is private-key compromise;
- future SLIP-0039 may threshold-share the same 32-byte secret without changing PeerId.

## Freeze posture

These historical freeze changes did not alter the then-current Kademlia default-off posture. **ADR-0034 subsequently supersedes only that rollout/default posture by making Kademlia enabled by default in the standard v1 build/profile composition.** Endpoint-routing and identity-recovery boundaries remain unchanged.
