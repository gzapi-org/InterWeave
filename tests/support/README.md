# support

Private test harness utilities: fake clocks, temporary profiles, peer/swarm factories, daemon runners, relay/probe harnesses, SQLite helpers and eventual assertions. Never a production dependency.

Package name `interweave-test-support`. Activated by Stage 0 with the two pieces that stage needs.

| module | what it is for |
|---|---|
| `fixtures` | loading the frozen vector files under `fixtures/`, by path or by vector name |
| `hex` | strict lower-case hex, the notation every fixture states its bytes in |

`repo_root()` is derived from this package's own location, not the current directory: `cargo test` runs each suite with its own package as the cwd, so a relative `fixtures/` path would resolve differently per suite.

## What these tests do not do

They do not recompute a vector. `tools/checks/verify_fixture_vectors.py` does that, implementing each algorithm from the specification rather than from the fixture — a second implementation here would give the repository two answers that agree right up until one of them is edited.

The question left over is whether an implementation can *read* what was frozen, which no Python checker can answer, and that is what `tests/fixture_loading.rs` covers.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for normative scenarios and exit criteria.
