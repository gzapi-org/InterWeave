# xtask

> Activation and dependency order is governed by [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](../architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) and ADR-0046.

Developer automation for the workspace: fixture verification, conformance orchestration, multi-process test setup, packaging checks and repository audits. It must not contain runtime application behavior.

Activated by Stage 0 as the workspace's first member, alongside the test-only `tests/support`.

## Commands

```
cargo xtask checks       # the tree checks under tools/checks/
cargo xtask selftests    # every test_*.sh beside its script, through tools/checks/run_suite.sh
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

Self-tests run through `tools/checks/run_suite.sh`, here and in CI's self-test loop alike: run bare, a suite whose assertion calls an undefined helper prints "command not found" and passes. To run one suite by hand, `bash tools/checks/run_suite.sh tools/gh/test_arm.sh`. The runner is not a guard, so `every_tree_check_is_run` does not ask `checks` to run it; `self_tests_are_discovered` pins that every suite goes through it.
