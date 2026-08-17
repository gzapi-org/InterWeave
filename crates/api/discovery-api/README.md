# discovery-api

Discovery provider/candidate/event contract types.

**Current status:** Stage 1, active workspace member. Types and validation only.

## What discovery answers

One question: *which peers might be reachable, and at which addresses?* Not whether to connect to them, not whether they are trusted, not what their traffic means (ADR-0006, ADR-0011, ADR-0012).

## Why this crate does not depend on `trust-api`

Deliberate, and the most important line in the manifest. An observation is evidence, never authority — so the types a provider produces should not be able to reach a trust decision at all. If the trust types are not in scope, a provider cannot consult or mutate them even by accident. The direction is one-way: `DiscoveryManager` consults trust; providers never do.

`DiscoveryEvent` likewise has no variant meaning "connect to this peer". Providers do not dial.

## Shapes worth knowing

- **Addresses are opaque strings.** A multiaddr is a backend concept; parsing one here would put libp2p's address grammar into a neutral contract.
- **Both collections are sets**, matching the schemas. Duplicates would consume the 64-address and 16-observation caps while adding no information — and those caps exist to bound how much state one peer can grow in another's cache.
- **`expires_at` is optional and `None` does not mean "never".** It means the provider does not express expiry; `DiscoveryManager` applies its own bound. `is_fresh_at` answers only what the provider actually said.
- **An expiry earlier than its observation is rejected.** Otherwise a provider bug renders as a peer that is simply never reachable.
- **`ProtocolObservation` is not a capability grant.** "Seen speaking protocol X" is not "may use protocol X here"; treating it as authorization would let a peer advertise its way into a role.

`tests/schema_agreement.rs` holds these types to the frozen schemas — enum membership, caps, and set semantics, not merely which members are required.
