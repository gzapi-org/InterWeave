---
role: "p2p-network-dev"
class: threads
topic: "external-review-2026-10-04-stage-review"
description: "Job j15: the owner's external stage review of main 58463fb9 (2026-10-04) — the review class's verdicts on its 5 findings and what is mine to fix"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - d447d8613215a5d3
---

## Job j15: the owner's external stage review of main 58463fb9 (2026-10-04) — the review class's verdicts on its 5 findings and what is mine to fix

The owner pasted an external stage-by-stage review of main 58463fb9 on 2026-10-04. The review class judged its five findings against that commit.

**Mine (job j15), to be one batch on a fresh branch off main after #186 (merged 2561ee78):**
- **P2-1 (judged P3): an ancestor symlink defeats the key/human-dir separation.**
  - The check in `crates/config/profile-config/src/load.rs:148` is lexical, and `persist.rs` `require_private_dir` lstat's only the final directory.
  - Only the same uid can configure this, and ADR-0040 does not require the on-disk separation.
  - Fix: reject a symlink in every existing ancestor of the key path, on load and on --create (`profile-identity` load and save; the daemon's identity()). Use the review's test shape: `external/link -> human/vault/` with the key under `link/keys/`.
- **P3-2: the IPC client does not check the selected version.** `ipc-client` `connection.rs` `open()` takes `response.ipc_version` unchecked.
  - Since #186 the client remembers the minor from every admin hello, so the check is now load-bearing.
  - Refuse a different major, a minor above the offered one, or one above IPC_MAX_MINOR, with ProtocolViolation.
  - Add scripted-server vectors. This is also the risk carried from #184.
- **P3-3: identity load does not check the key directory's owner.** The writer checks it (`require_same_owner`), the reader does not, and the code says so ("NOT FULL PARITY").
  - `lock.rs` `effective_uid()` is private and /proc-based (Linux-only).
  - Fix: a shared owned-private-dir check exported from profile-config, used by load.
- **P3-1: judged NOT REAL.** Every binding (in-process, fake, IPC) errors on events(0) once the backend is dead. Only the trait doc wants a sentence: "max == 0 takes nothing" holds on a live session.

**rust-ui-dev's: P2-2, the unbounded desktop bridge** (`facade_thread.rs:85-86`, plus one Slint task per notify).
- I sent them an OBSERVATION (seq 12409). They took it as P2 onto their #187 (seq 12412). Nothing is owed from me.

Five work commits is under the 8-commit floor, so arming needs the owner's word. Related: [[stage-15-r1-r2-shapes]].

**Progress, 2026-10-05.** Branch develop-qzapp/p2p-network-dev-01/fix/external-review-2026-10-04, off 2e5f79b0, 7 work commits:
- P3-3: 9a0a9296 and 6f1fce2c. The parser and the lock share `require_owned_private_dir`. Another owner is untestable as non-root.
- P2-1: 9256936d. It resolves the existing prefix with canonicalize instead of refusing every ancestor symlink, because Silverblue's /home -> var/home would break the refuse-all version. A dangling link gives KeyFileUnresolved. Both mutations were caught.
- Prose: configuration.md:22, STATE.md:64 and the plan line ~4268 still say "lexical, symlinks not followed". I asked architect-cto for a supply or a ruling in REQUEST seq 12553 (01a10cd7-ed2b-7ea3-8313-11bae3d665dd), and the PR waits on it.
- P3-2: e7dd2d34. The major check was dropped as unreachable: the hello_response parser already refuses any major but 2, and the mutation survived.
- P3-1 doc: c9bd3a82. #190's outbox-doc P3: 8fd48ed6.

*References: stage-15-r1-r2-shapes*

*Observed 2026-10-05 (p2p-network-dev)*
