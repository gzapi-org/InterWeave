---
role: "p2p-network-dev"
class: workflow
description: A scripted mutation that silently matches nothing reads as a passing mutation; patch after cargo fmt, against the formatted text, and confirm the patch applied
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 4a6d55c21fea748f
---

## A scripted mutation that silently matches nothing reads as a passing mutation; patch after cargo fmt, against the formatted text, and confirm the patch applied

When mutation-checking a guard with a scripted string replacement, the
patch must match the source **as `cargo fmt` left it**, and the script
must assert the match. A `str.replace` that finds nothing is a silent
no-op: the test then runs against unmutated code, passes, and reads as
"the test does not constrain this guard" — the opposite of the truth.

Hit on PR #64: `if self.running.get(&handle).is_none_or(...)` was
written on one line and reflowed by `cargo fmt` across five. The
one-line patch matched nothing, the class-check test "passed the
mutation", and I nearly recorded a load-bearing test as vacuous.

**Why:** the whole point of a mutation check is that it fails. A
mutation that cannot apply produces the same green as a mutation that
applied and was not caught, and nothing distinguishes them in the
output.

**How to apply:** put `assert old in s` in the patch script — every
mutation, not just the fiddly ones. If a mutation *passes*, suspect the
patch before suspecting the test: re-read the current text of the
target first. Related: [[verification-pipelines-must-fail-loudly]] is
the same failure one layer up — `| head` eating an exit code — and
[[verify-subagent-coverage-not-just-findings]] is it one layer above
that.

A second instance from the same PR, different mechanism, same shape:
`cargo test --workspace 2>&1 | grep -c "^test result: FAILED"` returned
0 for a tree where a crate did not COMPILE, because a crate that fails
to build emits no test-result lines at all. Check the exit code.

*References: verification-pipelines-must-fail-loudly, verify-subagent-coverage-not-just-findings*
