---
role: "p2p-network-dev"
class: workflow
topic: "a-cleanup-regex-spans-what-sits-before-its-anchor"
description: "A DOTALL regex removing a temporary marker deleted a file's licence header and module doc, because a stray copy of the marker's first line sat at the top of the file"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - b1b1d4ed81191c12
---

## A DOTALL regex removing a temporary marker deleted a file's licence header and module doc, because a stray copy of the marker's first line sat at the top of the file

In B2 (2026-09-29) each not-yet-called module carried a temporary
`// The connection loop reads these; ... #![expect(dead_code, ...)]`
marker. I removed them with `re.sub(r"// The connection loop reads
these;.*?\)\]\n\n", "", s, flags=re.S)`. In `dispatch.rs` an earlier
edit had wrongly PREPENDED the marker's comment lines above the SPDX
header (`'X'.join(['',''])` is `'X'`, not `''`). The regex matched from
that stray copy at line 1 down to the real marker, and deleted the
licence header and the module doc between them. Clippy and the tests
stayed green; only `check_license_headers.sh` saw it, in the next full
CI run. devex-tooling spotted it first.

**Why:** a lazy `.*?` under DOTALL still spans everything from the FIRST
match of its start to the nearest end, and a duplicate start anywhere
above reaches back over real content.

**How to apply:** remove a marker by an exact, whole-text replace of
the known block, asserting that it occurs exactly once, never by a
spanning regex. After any scripted edit, check `head -3` of every file
it touched for the SPDX header, and `git diff --stat` for deletions
larger than the marker.

*Observed 2026-09-29 (p2p-network-dev)*
