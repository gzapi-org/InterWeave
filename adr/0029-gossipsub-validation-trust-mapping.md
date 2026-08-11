# Map GossipSub validation results separately from trust admission

**Status:** Accepted

## Context

GossipSub validation results affect propagation and peer scoring, not merely local Channel delivery. Treating a locally untrusted publisher as objectively invalid would suppress forwarding **and** penalize the propagation peer that delivered the message. Accepting then dropping locally would allow an untrusted-origin message to continue propagating through this node. With asymmetric PeerId allowlists, either accidental mapping can produce surprising topology or scoring behavior.

The v1 architecture already selects signed GossipSub messages, deny-by-default PeerId trust, and trust-gated data-plane connections. A message received from a trusted direct neighbor can nevertheless carry a signed original publisher PeerId that is not locally allowlisted, so source trust still needs an explicit validation result.

## Decision

Use explicit GossipSub application validation with this v1 mapping:

- **`Reject`** — objective protocol/data invalidity: invalid or unverifiable signature/source association, malformed/version-invalid envelope, impossible/deceptive length fields, invalid message identifier encoding, or another condition that means the message must not be considered valid by conforming peers. The message is not forwarded; propagation-source scoring may be penalized according to GossipSub behavior/configuration.
- **`Ignore`** — the message is structurally/cryptographically valid but the authenticated **original publisher/source PeerId is not authorized by the local `PeerTrustPolicy`**, or an equivalent local-only admission rule says this node must not participate. The message is not delivered locally and is not forwarded by this node, but the propagation peer is not treated as having forwarded objectively invalid bytes solely because local authorization differs.
- **`Accept`** — the message is structurally/cryptographically valid and the original publisher/source is locally authorized for the v1 data plane. It may propagate and proceeds to resource limits, normalized deduplication, and local delivery eligibility.

Do not use `Accept` followed by a local trust drop for an unauthorized source in v1. Do not use `Reject` merely because allowlists differ between nodes.

## Alternatives considered

Map unauthorized source to `Reject`; map unauthorized source to `Accept` and drop only at Channel delivery; disable explicit application validation; require globally identical trust lists; use topic secrecy as authorization.

## Consequences

Asymmetric allowlists can intentionally interrupt propagation of messages from publishers a node does not trust. A downstream peer that trusts that publisher may therefore need an alternate mesh path. This is an explicit v1 consequence of local deny-by-default authorization, not a delivery guarantee or hidden GossipSub failure.

## Security implications

Untrusted-origin content is neither delivered nor relayed by the local node. Honest trusted propagation peers are not penalized merely for carrying a message whose original author is outside the local allowlist. Objective malformed/cryptographically invalid traffic can still trigger rejection/scoring behavior.

## Operational implications

Diagnostics must distinguish `validation_reject_invalid`, `validation_ignore_unauthorized`, and ordinary duplicate/resource drops. Mesh troubleshooting must account for trust asymmetry when a message does not propagate across a node.

## Implementation implications

The libp2p backend must preserve both original publisher/source identity and immediate propagation-peer context where available. Validation order is: decode/size guards -> cryptographic/source validation -> local source trust decision -> `Reject|Ignore|Accept` report -> normalized dedup/resource/local event path for accepted messages. Tests must cover a trusted relay carrying an untrusted-origin signed message and verify `Ignore` without local delivery or invalid-message penalty attribution.

## Revisit conditions

Revisit if a future group-membership protocol supplies channel-scoped authorization, if GossipSub API/scoring semantics materially change, or if deployment evidence shows asymmetric local allowlists make the selected overlay unusable.
