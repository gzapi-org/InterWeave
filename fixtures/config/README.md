# config

Frozen valid/invalid schema-v2 configuration fixtures.

`config-v2-cross-field.json` — 16 verdict vectors over the five endpoint cross-field rules: unique IDs, a default naming an enabled endpoint, static-subset peers within profile trust, narrowing-not-widening (ADR-0012), and advertised entries within `directory.max_advertised`.

These are relationships BETWEEN fields, which is what JSON Schema cannot express — the gap these vectors cover beside `contracts/schemas/`.
