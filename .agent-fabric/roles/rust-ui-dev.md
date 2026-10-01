---
role: rust-ui-dev
class: remit
project: interweave
description: "What the native-client role covers in InterWeave: the human client on Slint, desktop and Android, from Stage 15."
origin:
  - agent: user
    host: develop-qzapp
  - agent: architect-cto-01
    host: develop-qzapp
---

# rust-ui-dev — remit in InterWeave

The charter (agent-fabric `identities/roles/rust-ui-dev/charter.md`) is
the function; this is what it covers in this repository. Written by
fabric-coordinator from architect-cto's proposal (2026-10-01), which
the owner confirmed: p2p-network-dev owns Stage 14's code batches, and
this role is bound before Stage 15 opens. A role that wants its remit
changed proposes it.

**Yours here.** The human client:
- `crates/human/*`: core, store, ui-model, ui-slint, android-platform;
- `apps/human-desktop` and `apps/human-android`;
- `tests/human-chat`, `tests/human-retention`, and the desktop and
  Android end-to-end suites;
- the acceptance tests in
  `architecture/docs/architecture/human-client-ui.md`;
- accessibility, the Slint views;
- the human store's schema and its shape guard, and its retention
  (architecture/clients/human/RETENTION.md).

Slint is the reference first-party GUI on Windows, macOS, Linux and
Android, over one shared Rust core (ADR-0039). A second toolkit is a
revisit of that record, which only accessibility, platform,
performance, licensing or store requirements may trigger. It is never a
dependency you add.

**The seam.** The client reaches the network through the facade it
composes (`crates/human/transport-client` when it exists, ADR-0040 for
the desktop IPC). p2p-network-dev owns what that facade binds to. A
view never reaches past it to the transport, the daemon or IPC.

**Not yours here.**
- The transport, the IPC server and client, the daemon, and the
  neutral API crates: p2p-network-dev's.
- The contracts (`architecture/contracts/`) and the decision records
  and stage gates (`architecture/`): architect-cto's to write, and the
  owner's to close. You report in a stage's record what it did and did
  not prove, and never flip its status.
- `.github/`, `.claude/` and `tools/gh/`: devex-tooling's.
- `crates/human/chat-protocol` implements a contract
  (`architecture/contracts/schemas/human-chat/`). Its code is yours,
  its schema is not.

The merge queue stays on for this repository.
