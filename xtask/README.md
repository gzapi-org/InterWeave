# xtask

> Activation and dependency order is governed by [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](../architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) and ADR-0046.

Developer automation for the workspace: fixture verification, conformance orchestration, multi-process test setup, packaging checks and repository audits. It must not contain runtime application behavior.

Activated by Stage 0 as the workspace's first and, for now, only member.

## Commands

```
cargo xtask checks       # the tree checks under tools/checks/
cargo xtask selftests    # every test_*.sh beside its script
cargo xtask fmt [--check]
cargo xtask clippy
cargo xtask test
cargo xtask ci           # all of the above, with fmt in --check mode
```

Nothing short-circuits: a failing task is reported and the run continues, so one invocation tells you everything that is wrong.

## Why CI does not go through here

`xtask` **calls** the `tools/checks` scripts; it does not reimplement them. Two implementations of the same question disagree exactly when it matters.

CI still invokes those scripts by name, and that is not duplication. `tools/checks/check_guards_are_wired.sh` proves a guard is reachable by finding its basename in a workflow file, so a CI that ran `cargo run -p xtask` instead would hide every guard from the check written to find unreachable guards.

`cargo xtask checks` is kept in step with the directory by a test, not by discipline: `every_tree_check_is_run` reads `tools/checks/` from disk and fails when a guard exists that the local run would skip.
