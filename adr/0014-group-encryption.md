# Defer application/group encryption above GossipSub

**Status:** Accepted

## Context

Secure dynamic group encryption needs membership, key distribution, rotation, compromise recovery, and replay rules. Those semantics are not safely improvised in a generic transport phase.

## Decision

v1 uses trusted data-plane peers plus Noise-encrypted links and explicitly does not promise end-to-end secrecy from GossipSub forwarding peers. The payload remains opaque so higher layers or a future transport extension can encrypt it.

## Alternatives considered

Custom shared channel password; bespoke group key protocol; MLS-style system immediately; claim Noise is end-to-end for GossipSub.

## Consequences

Sensitive channels must ensure forwarding peers are within the accepted confidentiality boundary or encrypt payloads at a higher layer.

## Security implications

Residual risk is explicit: an authorized forwarding peer can read pub/sub plaintext. Topic hashing is not message encryption.

## Operational implications

Operators must understand channel peer trust scope. Diagnostics must never imply E2EE.

## Implementation implications

Reserve envelope versioning/content opacity; do not add key material to discovery or Channel metadata.

## Revisit conditions

Revisit when a concrete deployment requires confidentiality from intermediate peers and can define membership/key lifecycle requirements.
