# Adopt InterWeave as the project and wire namespace

**Status:** Accepted

## Context

The architecture was developed under a descriptive pre-implementation working namespace. That working namespace escaped into machine-facing contracts before implementation began: libp2p protocol IDs, domain-separation prefixes, the private Kademlia namespace, recovery/application tags, local profile paths, planned workspace metadata, and future binary names.

The canonical product name is now **InterWeave**. This repository still has no production Rust implementation, no deployed wire compatibility obligation, and no installed profile format that must retain the working identifier. Renaming after Stage 0/Stages 6-11 would instead become a compatibility event because the old identifier participates directly in hashes and protocol negotiation.

## Decision

The project display name is **InterWeave**. Machine-facing project identifiers use lowercase `interweave`.

The canonical network identifiers are:

```text
/interweave/direct/2.0.0
/interweave/endpoints/1.0.0
/interweave/kad/1.0.0/<network-hash>

interweave/direct-content-fingerprint/v1\0
interweave/gossipsub-message-id/v1\0
interweave/topic/v1\0
interweave/kad-network/v1\0
```

The canonical application/local identifiers are:

```text
application/vnd.interweave-human-chat+json;v=1
interweave.app-message/1
interweave-ed25519-bip39-entropy-v1
workspace.metadata.interweave
$XDG_*/*/interweave/...
interweave-transportd
interweave-transportctl
```

Claude-specific integration names remain Claude-specific where they describe the actual integration rather than project branding. Examples include `CLAUDE.md`, `apps/claude-channel`, `crates/claude/channel-core`, and the Claude Code Channel protocol/research terminology.

Because the namespace strings participate in deterministic hashing, the affected goldens are re-frozen as:

```text
DirectContentFingerprintV1
  media_type = "text/plain"
  payload    = UTF8("hello")
  SHA-256    = d73342f033f00fca9c4ffcced6f9e6debaeb53e3743049ee9aaf227a55f9bf15

GossipSub topic key
  ChannelId  = "general"
  SHA-256    = 82695daad230a8a8ddb6e43aae1063e4f611ded53d710f48b2ed3d206211c3bc

GossipSubMessageIdV1
  PeerId          = 12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN
  sequence_number = 0
  SHA-256         = 7f037dd538d9cccfb1949ca26b875c469173e6b248f1b68553ccaeb16bf9cf89

Kademlia network namespace
  network_id   = example-private-network
  network-hash = ssbtblqj7mexczivog5qfbfjvi
  protocol     = /interweave/kad/1.0.0/ssbtblqj7mexczivog5qfbfjvi
```

The former pre-InterWeave project identifiers are not compatibility aliases in the first production release. Git history remains the record of the pre-implementation working namespace.

## Alternatives considered

### Keep the old protocol namespace while branding the product InterWeave

Rejected. Product branding and protocol identifiers can legitimately differ, but there is no deployed compatibility benefit here. Retaining the working identifier would create permanent conceptual debt at the last point where changing it is free.

### Support both old and new wire identifiers from the first release

Rejected. There is no deployed peer population to interoperate with, and dual protocol/domain support would create unnecessary code paths, test surface, downgrade questions, and ambiguous fixture ownership.

### Delay the rename until implementation

Rejected. Stage 0 will materialize fixtures and Stages 6-11 will bind protocol/domain strings into executable behavior. Delaying converts a documentation-only decision into a compatibility migration.

## Consequences

Positive:

- product, wire, local storage, application tags, binary names, and workspace metadata share one canonical namespace;
- Stage 0 begins with final identifiers rather than transitional values;
- all domain-separated hash vectors are re-frozen before production code exists;
- Claude remains an integration rather than being embedded in the project identity.

Costs:

- all pre-implementation golden values derived from the old namespace change;
- historical commits and prior architecture ZIP names retain the old working identifier;
- any private experimental code written outside this repository against the old strings must be updated rather than treated as compatible.

## Security implications

Domain-separation strings are cryptographic/protocol inputs, not cosmetic labels. Implementations must use the exact InterWeave byte strings including the terminating NUL where specified. Supporting an undocumented old-name alias would create parallel compatibility surfaces and is forbidden unless a later ADR explicitly defines them.

The rename does not alter the trust model, key material, PeerId derivation, message-retention semantics, EndpointId semantics, or transport confidentiality properties.

## Operational implications

Profile paths, future executable names, configuration examples, diagnostics, packaging, and operator documentation should display/use InterWeave consistently. Existing architecture archives may still be named with the former working identifier; they are historical artifacts, not current package naming guidance.

Network peers using the former libp2p protocol IDs or hash domains are intentionally incompatible with the canonical first production implementation.

## Implementation implications

- Stage 0 fixture materialization must use the InterWeave values above and the current contracts.
- No production constant may contain the former project namespace.
- Protocol-ID tests must compare exact `/interweave/...` strings.
- Domain/hash tests must compare exact `interweave/...\0` bytes and the new goldens.
- Future Cargo package/binary/display metadata should use InterWeave naming except for explicitly Claude-specific integration packages.
- Repository/package handoffs should use `interweave` as the machine-facing basename.

## Revisit conditions

Revisit only if a future compatibility requirement intentionally introduces a second protocol namespace, a formal standards allocation requires a different identifier, or project ownership creates a justified machine-namespace migration. A marketing-only rename is not sufficient to silently change deployed InterWeave wire/domain identifiers.
