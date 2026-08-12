# Architecture/specification tree

This directory is the architectural source of truth for the implementation workspace at repository root.

Existing internal path references such as `contracts/TRANSPORT.md`, `transport/libp2p/DIRECT.md`, or `roadmap/SPIKES.md` are **architecture-root-relative** unless a document explicitly says repository-root-relative. This preserves the original specification vocabulary after the implementation landing zones were introduced.

Key entry points:

- [`adr/README.md`](./adr/README.md)
- [`contracts/TRANSPORT.md`](./contracts/TRANSPORT.md)
- [`contracts/LOCAL-CLIENT.md`](./contracts/LOCAL-CLIENT.md)
- [`docs/architecture/overview.md`](./docs/architecture/overview.md)
- [`docs/architecture/rust-blueprint.md`](./docs/architecture/rust-blueprint.md)
- [`docs/architecture/testing.md`](./docs/architecture/testing.md)
- [`docs/architecture/implementation-repository-layout.md`](./docs/architecture/implementation-repository-layout.md)
- [`roadmap/PHASES.md`](./roadmap/PHASES.md)
- [`roadmap/SPIKES.md`](./roadmap/SPIKES.md)
