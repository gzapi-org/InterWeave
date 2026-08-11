# Defer application/group encryption above GossipSub

**Status:** Accepted

## Context

Secure dynamic group encryption needs membership, key distribution, rotation, compromise recovery, and replay rules. Those semantics are not safely improvised in a generic transport phase. The confidentiality statement must also match the v1 connection/trust policy.

## Decision

v1 uses a **trust-gated data-plane overlay** plus Noise-encrypted peer links and explicitly does not promise end-to-end secrecy from trusted GossipSub forwarding peers. Ordinary data-plane connections are limited to locally allowlisted PeerIds by ADR-0011/0012, and GossipSub source authorization follows ADR-0029. The payload remains opaque so higher layers or a future transport extension can encrypt it.

## Alternatives considered

Custom shared channel password; bespoke group key protocol; MLS-style system immediately; claim Noise is end-to-end for GossipSub; allow arbitrary untrusted data-plane mesh peers and rely on topic-name secrecy.

## Consequences

Sensitive channels must ensure every trusted forwarding peer is within the accepted confidentiality boundary or encrypt payloads at a higher layer. A locally unauthorized message source is not forwarded by this node, but a trusted forwarding peer can still read any plaintext it does relay.

## Security implications

Residual risk is explicit: an authorized/trusted forwarding peer can read pub/sub plaintext. Topic hashing is not message encryption and low-entropy channel names remain dictionary-guessable. Data-plane trust reduces exposure to arbitrary discovered peers; it does not create group E2EE.

## Operational implications

Operators must understand the profile's peer allowlist as part of the plaintext forwarding boundary. Diagnostics must never imply E2EE or claim that topic hashing is confidentiality.

## Implementation implications

Reserve envelope versioning/content opacity; do not add key material to discovery or Channel metadata. Enforce trust before data-plane connectivity and explicit GossipSub validation-result mapping rather than assuming the mesh itself enforces authorization.

## Revisit conditions

Revisit when a concrete deployment requires confidentiality from trusted intermediate peers and can define membership/key lifecycle requirements.
