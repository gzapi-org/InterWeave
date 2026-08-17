# transport-api

Transport/EndpointId/Connectivity neutral types and public transport contract.

**Current status:** Stage 1, active workspace member. Types and validation only — no I/O, no runtime, no backend.

## What is here

| Module | Contents |
|---|---|
| `ids` | `EndpointId`, `ChannelId`, `MessageId`, `TransportIdentity`, `DirectDestination` |
| `payload` | `Payload`, `MediaType`, and the 48 KiB ceiling |
| `status` | `TransportCapabilities`, `Health`, `ConnectivitySummary`, `TransportError` |

## Two properties worth knowing before using it

**Identifiers are parsed, never merely held.** Every newtype validates at construction and deserializes through the same parser, so holding one means its grammar already held. JSON arriving over IPC is untrusted input, and a derived `Deserialize` would happily build values the grammar never admitted.

**Absence is distinct from emptiness.** An absent media type is not an empty one — the content fingerprint distinguishes them, so collapsing them would give two different messages one dedup identity. An omitted destination endpoint means the receiver's configured default, never fan-out. Both distinctions are in the types because both are observable on the wire.

## The dependency rule

`serde` and nothing else. No libp2p, Slint, JNI, SQLite, Claude/MCP library, or platform socket type may appear here (ADR-0021, ADR-0045). A backend concept that reaches this crate has escaped the boundary that makes the backend replaceable.

## Agreement with the frozen schemas

`tests/schema_agreement.rs` reads the JSON Schemas under `architecture/contracts/schemas/` and asserts these types say the same thing — the error vocabulary member for member, the health and path enums, the grammars, and the payload bounds in both their decoded and base64url-encoded forms. It deliberately does not run a JSON Schema validator: that would prove instances conform while leaving the question that matters, *do the two definitions agree*, unasked.
