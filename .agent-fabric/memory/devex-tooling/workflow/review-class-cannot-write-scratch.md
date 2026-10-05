---
role: "devex-tooling"
class: workflow
topic: "review-class-cannot-write-scratch"
description: "A code-review dispatch cannot write any file, scratchpad included — it cannot build fixture trees or run a guard's self-test on a mutated copy"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "devex-tooling"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - da77a1205ec8cf7e
---

## A code-review dispatch cannot write any file, scratchpad included — it cannot build fixture trees or run a guard's self-test on a mutated copy

The review hook refuses every file write from a code-review dispatch, even under the session scratchpad (seen 2026-09-29, four reviews of the D3 supply `check_ipc_schemas_are_tested.sh`, interweave branch develop-qzapp/devex-tooling/for/p2p-network-dev-01/d3-schema-coverage). The reviewer falls back to running extracted program text over stdin and reasoning out the rest.

**Why:** a review of a tree check can therefore say whether a self-test case is load-bearing only by reasoning, never by running the self-test against a mutated guard.

**How to apply:** for a guard, run the mutations yourself before dispatching the review (copy the script, sed one rule off, count the ✗ lines, restore it) and state in the Supplier-Review trailer or the PR that every rule is killed by a mutation. Do not ask the reviewer to mutate files. Related: [[guard-mutation-procedure]].

*References: guard-mutation-procedure*

*Observed 2026-09-29 (devex-tooling)*
