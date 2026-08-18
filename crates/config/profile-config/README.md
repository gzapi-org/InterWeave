# profile-config

Configuration-v2 structures and cross-field validation.

**Current status:** Stage 1, active workspace member. Structures and rules only — nothing here reads a file.

## What is actually hard here

Not the shapes. JSON Schema already describes those and `validate_contracts.py` checks them. The work is the rules that relate one field to *another*, which a schema cannot express — and which are the errors an operator actually makes:

1. endpoint ids are unique;
2. a set `default_direct_endpoint` names an **enabled** entry;
3. a `static_subset` is a genuine subset of `trust.allowed_peers`;
4. endpoint policy narrows but never widens (ADR-0012);
5. enabled advertised entries fit `directory.max_advertised`.

## Written against frozen vectors, not against a reading of the schema

`tests/frozen_vectors.rs` runs all 16 vectors from `fixtures/config/config-v2-cross-field.json`. Those verdicts are recomputed independently by `verify_fixture_vectors.py` from the specification, so they are not this crate's opinion written down — which is what makes them worth testing against.

The suite checks two agreements, and the second catches the drift: every vector must **deserialize** into these types, and every verdict must match. A type that could not parse the frozen configurations would be a contract failure even if its rules were right. It also asserts the fixture is two-sided — a suite that only saw valid configurations would pass with a validator that never returns an error — and that the widening vectors are rejected *for widening*, not for some unrelated reason that happens to produce the same verdict.

Writing this crate is what surfaced that the fixture's policy shape had never matched `endpoint-config.schema.json`, corrected in the commit before this one.

## Details that are decisions

- **Every violation is reported, not the first.** Fixing a configuration one error per restart is the experience this avoids; the cost is a `Vec`.
- **Errors carry the offending value.** "Duplicate endpoint id" without saying *which* sends an operator with sixty endpoints looking.
- **Unknown and disabled defaults are different errors.** One is a typo; the other is a deliberate change with a forgotten consequence, and they need different fixes.
- **A disabled advertised entry does not count against the bound.** It advertises nothing, so counting it would reject a profile that behaves correctly.
- **No file, no path, no format.** The rules are identical whether the profile arrived as YAML on disk, JSON over an admin socket, or a literal in a test.
