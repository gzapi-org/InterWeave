# Machine-readable wire contracts

JSON Schema definitions for the shapes that cross the network and the local IPC boundary. The model is [ADR-0049](../../adr/0049-machine-readable-wire-contracts.md).

**The prose contracts beside this directory stay normative for behaviour** — routing order, what an acceptance proves, which failures stay coarse on the wire. These schemas are normative for **shape**. Where both describe the same field they must agree, which is why every schema carries a pointer to the prose specification it formalises.

## Layout

```
schemas/
├── manifest.json                      root index of families
├── _meta/contract.meta.schema.json    the schema every contract validates against
├── common/                            shapes referenced across families
└── endpoints/                         Model B endpoint addressing
```

One directory per **family** (a coherent domain area), one file per **concept** (one wire shape). Only `endpoints` and `common` exist today — remaining families (`ipc`, `direct`, `discovery`, `identity`, `connectivity`, `human-chat`) are added as their stages come up under ADR-0046, not up front.

## Conventions

| Element | Pattern | Example |
|---|---|---|
| File | `<concept>.schema.json` | `endpoint-id.schema.json` |
| `$id` | `urn:interweave:schemas:<family>:<concept>` | `urn:interweave:schemas:endpoints:endpoint-id` |
| `x-contract.name` | `<family>.<concept>` | `endpoints.endpoint-id` |
| Dialect | JSON Schema Draft 2020-12, corpus-wide | |

Both halves of `$id` must match the file's own directory and name; the validator enforces it, because an identifier that can drift from its location is not an identifier.

## Status — read this before trusting a contract

`x-contract.status` says what a contract is authoritative **about**:

- **`active`** / **`deprecated`** — describes the **current wire**.
- **`approved`** — an authoritative **implementation target**. Never a claim that anything implements it.
- **`proposed`** — a review artifact. Must not drive implementation.

**Every contract here is `approved`.** Nothing in this repository is implemented, so no contract describes current behaviour.

## Adding a contract

1. Write `<family>/<concept>.schema.json` with a complete `x-contract` block: `name`, `status`, `version`, the deciding `adr` numbers, and the prose `specification` path.
2. Add its row to `<family>/manifest.json` with a `status` matching the schema's own.
3. For a new family, add the directory row to `manifest.json` here.
4. If it has frozen vectors, list them in `x-contract.fixtures` and make sure `verify_fixture_vectors.py` knows the algorithm.

```
python3 tools/checks/validate_contracts.py
python3 tools/checks/verify_fixture_vectors.py
```

The validator checks manifests in both directions, so a schema nobody listed and a row naming nothing both fail.
