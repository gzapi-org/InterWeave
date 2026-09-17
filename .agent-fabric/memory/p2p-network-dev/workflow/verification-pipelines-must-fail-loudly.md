---
role: "p2p-network-dev"
class: workflow
description: "Never end a verification command in | head/tail — it masks the exit code and set -e; two broken states were committed (once pushed) because the pipeline's tail exited 0"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - f1ea2f4ef6612704
---

## Never end a verification command in | head/tail — it masks the exit code and set -e; two broken states were committed (once pushed) because the pipeline's tail exited 0

In a scripted verify-then-commit sequence, `cargo test … | grep … |
head -N` reports the EXIT CODE OF `head`, which is 0 even when the
build failed and grep matched nothing. `set -e` sees success and the
script walks on to `git commit` / `git push`.

**A THIRD instance, 2026-09-03, in a REPORTING step rather than a
scripted commit.** `cargo test --workspace … | grep -E 'test result' |
tail -20` run as a background task: the harness reported "exit code 0",
which was `tail`'s. The run had in fact been cut short by its own
`timeout` mid-build, and the surviving output showed 4 tests across 20
binaries where the real figures are 1306 across 83. I reported the
workspace suite as passing on that basis, and had to retract it. So the
rule is not only about `set -e` scripts: **a piped verification is
unsafe even when nothing is committed after it, because the number you
then report is wrong.** Re-run wrote to a file and captured `$?`
directly on the next line.

**Why:** it happened twice in one session. A P2 fix was committed with
tests that did not compile (the "green" check printed nothing and
nobody noticed); later a whole commit+push ran after `cargo xtask ci`
FAILED, because the gate's output was piped and the commit was
unconditional in the same script.

**How to apply:**
- Run the gate as its own command and let its exit code gate the
  commit: `cargo xtask ci && git commit …`, never `ci | tail -2` then
  commit unconditionally.
- When trimming output for display, capture first, test `$?`, then
  trim: `out=$(cmd); echo "$out" | head`.
- Treat "the expected line did not print" as failure, not as quiet
  success — an empty grep is the signal, and `| head` eats it.
- After any scripted mutation check, verify the RESTORED state with an
  unmasked full run before committing.

See [[anchor-inserts-after-not-before]] for the sibling scripted-edit
trap, and [[verify-subagent-coverage-not-just-findings]] for the
general shape: a silent no-op reads as success.

**A SEARCH that reports zero hits needs a positive control too.** Zero is
the answer you were hoping for, which is exactly why it is not
interrogated. On PR #76 a reviewer's shell sweep for stale claims wrote
0 lines and exited 0 — a `>>` redirect inside a `while` loop fed by a
pipeline had eaten every match — and that is indistinguishable from
"swept clean". Its `perl -0777` rewrite of the same search returned 37
hits in 15 files, one of which was a real P2.

Before believing a clean sweep: run it against a pattern you KNOW is
present and confirm it prints. A grep that cannot find anything finds
nothing, and reports the same thing either way.

This also applies to multi-line claims: a line-oriented `grep` cannot
match a phrase that wraps across comment lines, so it reports zero
truthfully and misleadingly at once. Flatten newlines and comment markers
first — see [[assertions-that-cannot-fail]].


**A FOURTH shape, 2026-09-10: a trailing `echo` in a BACKGROUNDED command
becomes the exit code the harness reports.** Run as a background task:

```
bash tools/gh/pr-review-status.sh 86 --wait 30m --automated-only; echo "EXIT=$?"
```

The completion notification said **exit code 0** — that was the `echo`. The
script had exited **1**, meaning "this head is not covered by the automated
reviewer", and I told the user both review gates had passed. Same for
`cargo xtask ci 2>&1 | tail -60; echo "XTASK_CI_EXIT=$?"`, where the
notification's 0 was the echo and the real answer was 1 (clippy).

The `echo "EXIT=$?"` is still the right thing to write — it is what PUT the
real code in the log. The error is reading the notification's exit code as
the command's. **For any backgrounded command, the notification's exit code
is the exit code of the LAST thing in the command line. Open the output file
and read the captured value.**

*References: anchor-inserts-after-not-before, assertions-that-cannot-fail, verify-subagent-coverage-not-just-findings*
