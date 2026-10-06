---
role: "rust-ui-dev"
class: solution
topic: "stage-15-b2-pr178"
description: "Stage 15 B2+B3 = PR #178 (branch develop-qzapp/rust-ui-dev-01/feat/stage-15-desktop): app start-up/running + store v7 read_pairs; review history and SQLite lessons"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 26344b31162fcabe
---

## Stage 15 B2+B3 = PR #178 (branch develop-qzapp/rust-ui-dev-01/feat/stage-15-desktop): app start-up/running + store v7 read_pairs; review history and SQLite lessons

PR #178 (2026-10-04): apps/human-desktop (startup with sysexits codes, HumanClientLock, store classification, facade thread over IPC, App::pump, NoDaemon via ProfileLock::is_held as Option<bool>), store v7 read_pairs (4096 cap, AlreadyRead = duplicate), needs_recovery, header_user_version pre-check. Folded p2p supplies R4 (human_dir), R5 (HumanClientLock), fcb859e5 (lock judges before creating). 8 work commits + many review fixes; gate met at d5969059.

SQLite lessons (verified by tests):
- The store leaves files in WAL. A read-only WAL file passes the journal pragma and fails at migration DDL with READONLY → must stay StoreError::Sql (migration_error keeps Busy/Locked/Full/CannotOpen/IoErr/OOM/ReadOnly as Sql). My first test used DELETE mode and wrongly "proved" the arm unreachable — test in the mode production uses.
- A read-only open creates -wal/-shm with the db's mode (0400): fixing only the main file isn't enough.
- journal_mode=WAL rewrites a rollback-mode header: read user_version from header offset 60 before any connection. Un-checkpointed WAL from a crashed newer build is NOT covered (stated).
- Promise only "never renamed, moved or deleted" for a refused store, not "left byte-for-byte".

Related: [[stage-15-b1-pr176]], [[stage-15-rulings]]

*References: stage-15-b1-pr176, stage-15-rulings*

*Observed 2026-10-04 (rust-ui-dev)*
