# ASCII ChannelId with versioned hashed wire topic

**Status:** Accepted

## Context

ASCII avoids normalization/canonicalization ambiguity; hashing prevents raw organizational/project labels from appearing directly as topic names and gives a stable namespace.

## Decision

Define ChannelId as 1..128 ASCII bytes matching `[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}` and case-sensitive. Map to SHA-256 of a domain/version prefix plus raw ID before constructing the GossipSub topic.

## Alternatives considered

raw topic names; unrestricted Unicode; random per-channel IDs requiring registry; application-specific naming syntax.

## Consequences

Human configuration remains readable locally while wire topic names are opaque. Dictionary attacks remain possible for predictable IDs.

## Security implications

Hashing is privacy hardening, not secrecy or authorization. Channel membership still comes from trust/subscription policy.

## Operational implications

Topic mapping is deterministic across peers and implementation languages. Version prefix allows incompatible future mapping.

## Implementation implications

Validate before hashing. Publish channel ID only in application/Channel metadata as locally configured/received semantics, not as discovery authority.

## Revisit conditions

Revisit if Unicode identifiers become a real interoperability requirement or stronger topic privacy requires keyed derivation.
