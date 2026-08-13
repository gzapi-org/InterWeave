# Machine-readable wire contracts with lifecycle status

**Status:** Accepted

## Context

The nine contracts under `architecture/contracts/` are prose. Prose is the right medium for behaviour — routing order, what an acceptance proves, which failure stays coarse on the wire — and it will remain normative for all of that. It is the wrong medium for **shape**. A prose field list cannot be validated against an instance, cannot be diffed for compatibility when a version changes, and cannot generate the conformance vectors that independent implementations agree on.

Two consequences are already visible. `fixtures/` contains nine README files and **zero vectors**, so `CLAUDE.md` §9's requirement that "frozen fixture checks still pass where applicable" currently checks nothing — the goldens ADR-0047 re-froze live only in ADR prose, where nothing recomputes them. And ADR-0046 places `foundation/fixtures` and `neutral contracts/config` at the very bottom of the construction order, which means these are the next artifacts due, not a later concern.

There is a second problem specific to this repository's state. Nothing is implemented. Every contract here describes what an implementation must do, not what any running code does. Prose cannot express that distinction, so a reader has no way to tell a specification of current behaviour from a specification of intended behaviour — and in a repository where *none* of it is current, that ambiguity is total.

## Decision

### Wire shapes get JSON Schema definitions beside the prose

Machine-readable contracts live under `architecture/contracts/schemas/`, organised as one directory per **family** (a coherent domain area) and one file per **concept** (one wire shape):

```text
architecture/contracts/schemas/
├── manifest.json                      root index of families
├── _meta/contract.meta.schema.json    the schema every contract validates against
├── common/                            shapes referenced across families
│   ├── manifest.json
│   └── <concept>.schema.json
└── endpoints/
    ├── manifest.json
    └── <concept>.schema.json
```

Schemas are JSON Schema **Draft 2020-12**, one dialect across the corpus, because a mixed-dialect set changes validation semantics between files without saying so.

The prose contract remains normative for **behaviour**; the schema is normative for **shape**. Neither supersedes the other, and where they describe the same field they must agree — the schema carries a `specification` pointer so the pair is always locatable.

### Identity is a URN, and it is checked against the tree

Every contract declares `$id` of the form `urn:interweave:schemas:<family>:<concept>`, with `x-contract.name` as the dotted form of the same pair. Both halves must match the file's own directory and filename. An identifier that can drift from its location is not an identifier.

### Lifecycle status says what a contract is authoritative about

Every contract declares `x-contract.status`:

- **`active`** / **`deprecated`** — describes the **current wire**.
- **`approved`** — an authoritative **implementation target**. It is never a claim that anything implements it.
- **`proposed`** — a review artifact. It MUST NOT drive implementation.

Every contract in this repository is `approved` until a corresponding implementation exists. That is the distinction prose could not express, and it is the one that matters most in a pre-implementation tree.

### Provenance is mandatory, and it points both ways

A contract declares the ADR(s) that decided its shape and the prose specification it formalises; both are existence-checked, as is any `x-contract.fixtures` entry. A contract that cannot be traced to a decision is unreviewable — a reader confronted with a field cannot tell whether it is load-bearing or incidental.

The prose specification carries the return link: adding a family means adding a line to its specification pointing at `schemas/<family>/`. Only the schema-to-prose direction can be machine-checked, so the other one is a propagation step — without it a reader of the specification never learns the schemas exist, which is the audience the schemas were written for.

### Frozen vectors are recomputed, not stored and trusted

`fixtures/` vector files declare their `algorithm.id`, and `tools/checks/verify_fixture_vectors.py` implements that algorithm **from the specification** — never by reading the fixture's own description — and recomputes every vector. An algorithm the verifier does not know is a **failure, not a skip**: a vector file nothing can verify is precisely the artifact this guards against.

Vectors carrying `frozen_by` are goldens re-frozen by an ADR. All vectors in a file must hash distinctly, because collisions between the edge cases are the bug those edge cases exist to catch.

### The corpus is validated as a whole

`tools/checks/validate_contracts.py` enforces meta-schema conformance, legal Draft 2020-12 syntax, identity-against-location, manifest completeness in **both** directions, agreement between a manifest row's status and the schema's own, provenance existence, and `$ref` resolvability. Both checks join the repository-wide verification list.

## Alternatives considered

**Prose only, as today** — cheapest, and adequate while nothing is built. Rejected because the next construction stage is exactly the one that needs vectors, and because a frozen golden that lives only in ADR prose is a number nobody recomputes; ADR-0047's re-freeze already proved these values change under edits nobody thinks of as protocol changes.

**Generate schemas from Rust types once they exist** — attractive, and wrong at this stage: it inverts ADR-0046 by making the contract a product of the implementation rather than its gate, and it cannot produce a contract for a shape that has not been built.

**A single flat schema directory** — no family manifests, no root index. Fine at seven contracts, unnavigable at seventy, and it offers no place to record what a coherent domain area is or which prose specification governs it.

**Protobuf or CDDL as the schema language** — better fits for byte-level framing, and the direct v2 wire may yet want one. Rejected as the *first* step because the shapes needing definition now are JSON-shaped (IPC v2 bodies, the directory response, the delivered event), and JSON Schema validates them with tooling already present.

**A generated machine index of contracts** — a third artifact to keep in sync, where two manifests and the checker already carry the facts.

## Consequences

Shape becomes testable. A conformance test can validate an instance rather than a human comparing a struct against a paragraph, and a compatibility change becomes a diff instead of a reading.

`fixtures/` stops being empty scaffolding: `fixtures/direct-v2/direct-content-fingerprint-v1.json` now carries the ADR-0047 golden plus six derived edge vectors — media absent versus present, empty payload under each, a 128-byte media type at the ceiling, non-ASCII payload bytes — all recomputed on every run. §9's fixture requirement now has something to check.

The cost is duplication: a field described in prose and defined in a schema can disagree. That is why provenance pointers are mandatory in both directions, and why the pair is reviewed together. The corpus also acquires a dependency on `python3` with `jsonschema` for validation.

Writing a contract is now more work than writing a paragraph — envelope, manifest row, provenance, and a validator run. That cost is deliberate and falls on the artifact that most deserves review.

## Security implications

A schema is a **shape** check and never an authorization boundary. `endpoint-config` can express that a policy is a narrowing subset; it cannot enforce that the subset is applied, and nothing in a schema substitutes for the runtime checks in ADR-0011, ADR-0012, and ADR-0030. Treating validation as admission would be a serious misreading.

Two security-relevant properties do become mechanically enforced. The endpoint-directory response schema caps `endpoints` at 32 with unique items and clamps `ttl_ms` to the five-minute ceiling, so a malformed or hostile directory reply fails validation before it can reach a cache or a UI. And the fingerprint verifier makes a change to the fingerprint domain — which would silently break cross-implementation dedup agreement and could be used to slip a duplicate past a peer — a failing check rather than an invisible edit.

The schemas describe shapes only. No key material, no capability grant, and no trust record is ever expressed as a contract instance.

## Operational implications

`tools/checks/validate_contracts.py` and `tools/checks/verify_fixture_vectors.py` run before pushing and are listed in `CLAUDE.md` §9. Both take `--root` so they can be run against an extracted archive, which is how a handoff recipient confirms the vectors they received.

## Implementation implications

No production code. When crates are activated, the neutral contract crates are the natural consumers: shapes generated from or validated against these schemas belong under `crates/api/*`, which ADR-0045 keeps free of libp2p, UI, and platform types.

The `endpoints` family is deliberately the first and only one populated. Remaining families — `ipc`, `direct`, `discovery`, `identity`, `connectivity`, `human-chat` — are added as their stages come up under ADR-0046, not up front, exactly as the Cargo workspace gains members.

## Revisit conditions

Revisit if byte-level framing needs a schema language JSON Schema cannot express, in which case the direct v2 wire moves to Protobuf or CDDL while the JSON-shaped contracts stay; if generated schemas from implementation types become preferable once implementations are the source of truth for shape rather than its consumer; or if the prose and schema pair proves to drift in practice despite the provenance pointers, which would argue for generating one from the other rather than maintaining both.
