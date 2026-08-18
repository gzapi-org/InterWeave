# transport-contract

Backend-neutral Transport API conformance: capabilities, payload limits, trust errors, events, broadcast/direct/status semantics.

**Current status:** Stage 1, active test-only workspace member. No backend, no networking, no runtime.

## What this suite adds over the per-crate ones

Each contract crate carries a suite comparing **definitions** — that the Rust enum members *are* the schema's enum members, that a constant equals a `maxLength`. This one asks the other half, about **instances**:

- a value serialized from a Rust type validates against its schema;
- a schema-valid instance deserializes into that type.

Neither direction implies the other. A type can emit conforming JSON while refusing to parse a legal document (an over-strict `deny_unknown_fields`, a missing default), and a type can parse everything while emitting a shape no schema accepts — which is exactly the defect review found in `Payload`, where a derived impl emitted an array of integers.

It also covers claims no single crate can make alone. `a_data_session_never_carries_an_admin_capability` needs both `ipc/capability.schema.json` (one closed vocabulary, `admin.*` included) and `local-client-api` (a data session's grant never contains one) to be true at once.

## Why a real validator

Hand-written assertions check what the author remembered to check. My first agreement suite compared only which connectivity members were *required*, and six value-level disagreements passed it. This package is test-only and nothing depends on it, so it may take the `jsonschema` dependency a production crate must not.

Verified by mutation: changing `PreferredPathPolicy` from `snake_case` to `kebab-case` — precisely the class of bug that shipped past the first suite — fails this one.

The `$ref`s here are `urn:interweave:…`, which no resolver can fetch, so every schema in the tree is loaded and registered under its own `$id` in a `OnceLock` shared across tests.
