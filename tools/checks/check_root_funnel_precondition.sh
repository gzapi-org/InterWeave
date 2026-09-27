#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_root_funnel_precondition.sh
#
# >>> help
# Does the root funnel's measurement still exist, and still run?
#
#   bash tools/checks/check_root_funnel_precondition.sh [--root DIR]             # the tree
#   bash tools/checks/check_root_funnel_precondition.sh [--root DIR] --compiled  # + libtest
#
# The plan's Stage 12 (BOTTOM-UP-IMPLEMENTATION-PLAN.md §15, ADR-0052
# rule 5) may compose Kademlia, the relay client, or the authorised-
# identify opt-ins only while the root funnel — the wrapper pruning the
# union the Swarm dials from — has its measurement committed as a test:
# a behaviour-extended dial toward a loopback and a /dns4 name opens no
# socket through the wrapper, and does with it removed. The test is the
# record, and CI re-measures it on every run. §15 asked for this to be
# mechanical (p2p-network-dev proposed, devex-tooling wires).
#
# UNCONDITIONAL, deliberately. The relay client is one of the four, and
# profile-config pins `transport.connectivity.relay.client.enabled` to
# literal true with a Default for the block, so EVERY valid profile
# enables it whether or not the key is written (p2p-network-dev,
# 2026-09-27).
#
# WHETHER A TEST IS COMPILED AND NOT IGNORED IS ASKED OF LIBTEST, not read
# from the source. Three review rounds (#134) each found another Rust
# layout a text scan could not see — #[cfg(…)], #[cfg_attr(…, ignore)]
# wrapped over lines, #![cfg_attr(all(), cfg(any()))] on the file, a
# module around the test, a "]" inside a string. `cargo test --test
# root_funnel -- --list` names every test the compiler built, and
# `-- --list --ignored` every one it built as ignored; that answers all
# of those forms at once, and every future one. So --compiled (CI's
# `rust` job, after the Tests step has built the binary) requires each
# pinned test to be listed, and none of them listed as ignored. They are
# pairs, a control and its prune; a control alone asserts nothing:
#     the_control_kademlia_dials_what_its_table_holds
#     the_root_funnel_prunes_what_a_behaviour_extends_a_dial_with
#     the_control_tcp_dials_the_last_host_of_a_stacked_address
#     the_root_funnel_prunes_a_stacked_address
# Renaming one is a deliberate act: edit PINNED below with it.
#
# WHAT THE TREE CHECK ASKS (no compiler; CI's tree-checks job):
#   1. crates/transport/libp2p/tests/root_funnel.rs exists;
#   2. it builds its Swarm through `RootFunnel::new(`, so a test reduced
#      to the controls cannot pass as the measurement;
#   3. the crate does not switch off test discovery (`autotests = false`)
#      unless a [[test]] names root_funnel;
#   4. .github/workflows/ci.yml runs `cargo test --workspace --all-targets`
#      on a command line (not a comment), with no --no-run, no libtest
#      arguments after ` -- `, no `||`, and not excluding this crate.
#
# NOT CHECKED: that the tests measure what their names say (the test's
# own job and its review's), and CI edits made to defeat this — a step's
# continue-on-error or if:, a pipe without pipefail. Those are workflow
# changes, reviewed as such; this guards the record against quiet loss.
#
# Exit codes:
#   0  the measurement is present (and, with --compiled, built and run)
#   1  a precondition is missing (each named)
#   2  invocation problem (bad --root; --compiled without cargo)
# <<< help

set -uo pipefail

ROOT=""; COMPILED=0
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'; exit 0 ;;
        --root) [ $# -ge 2 ] || { echo "check_root_funnel_precondition: --root needs a value" >&2; exit 2; }; ROOT="$2"; shift 2 ;;
        --compiled) COMPILED=1; shift ;;
        *) echo "check_root_funnel_precondition: unknown argument '$1' (try --help)" >&2; exit 2 ;;
    esac
done
[ -n "$ROOT" ] || ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] && [ -d "$ROOT" ] || { echo "check_root_funnel_precondition: no repository root (--root DIR)" >&2; exit 2; }

CRATE="$ROOT/crates/transport/libp2p"
TEST="$CRATE/tests/root_funnel.rs"
CI="$ROOT/.github/workflows/ci.yml"
PINNED=(
    the_control_kademlia_dials_what_its_table_holds
    the_root_funnel_prunes_what_a_behaviour_extends_a_dial_with
    the_control_tcp_dials_the_last_host_of_a_stacked_address
    the_root_funnel_prunes_a_stacked_address
)
fails=0
fail() { echo "FAIL: $1"; fails=$((fails + 1)); }

if [ ! -f "$TEST" ]; then
    fail "crates/transport/libp2p/tests/root_funnel.rs is missing — the root funnel's measurement (plan §15, ADR-0052 rule 5) is gone"
elif ! grep -q 'RootFunnel::new(' "$TEST"; then
    fail "tests/root_funnel.rs never builds a Swarm through RootFunnel::new( — the prune side of the measurement is gone"
fi

if [ -f "$CRATE/Cargo.toml" ] && grep -Eq '^[[:space:]]*autotests[[:space:]]*=[[:space:]]*false' "$CRATE/Cargo.toml"; then
    awk '/^\[\[test\]\]/ {t=1; next} /^\[/ {t=0} t && /^[[:space:]]*name[[:space:]]*=[[:space:]]*"root_funnel"/ {found=1} END {exit !found}' "$CRATE/Cargo.toml" \
        || fail "crates/transport/libp2p/Cargo.toml sets autotests = false and no [[test]] names root_funnel — the test is not built"
fi

if [ ! -f "$CI" ]; then
    fail ".github/workflows/ci.yml is missing — nothing runs the measurement"
else
    # A command line, never a comment: `run: cargo test …` or a bare
    # `cargo test …` inside a `run: |` block. It must run everything and
    # fail CI on a failure.
    runs="$(grep -E '^[[:space:]]*(-[[:space:]]*)?(run:[[:space:]]*)?cargo test ' "$CI" \
        | grep -- '--workspace' | grep -- '--all-targets' \
        | grep -v -- '--no-run' | grep -vE -- ' -- |[|][|]' || true)"
    if [ -z "$runs" ]; then
        fail ".github/workflows/ci.yml has no command running cargo test --workspace --all-targets (a comment, --no-run, libtest arguments or || does not count) — present no longer means passes"
    elif grep -Eq -- '--exclude[ =]+interweave-transport-libp2p' <<<"$runs"; then
        fail ".github/workflows/ci.yml excludes interweave-transport-libp2p from cargo test — the measurement never runs"
    fi
fi

if [ "$COMPILED" = 1 ] && [ -f "$TEST" ]; then
    CARGO="${CARGO:-cargo}"
    command -v "$CARGO" >/dev/null 2>&1 || { echo "check_root_funnel_precondition: --compiled needs cargo on PATH" >&2; exit 2; }
    listed="$(cd "$ROOT" && "$CARGO" test -q -p interweave-transport-libp2p --test root_funnel --locked -- --list 2>&1)"; rc=$?
    if [ "$rc" -ne 0 ]; then
        fail "cargo could not build or list the root_funnel tests (exit $rc) — $(printf '%s\n' "$listed" | tail -1)"
    else
        # An unread ignored-list would read as "none ignored": fail closed.
        ignored="$(cd "$ROOT" && "$CARGO" test -q -p interweave-transport-libp2p --test root_funnel --locked -- --list --ignored 2>&1)"; rc=$?
        [ "$rc" -eq 0 ] || fail "cargo could not list the ignored root_funnel tests (exit $rc) — $(printf '%s\n' "$ignored" | tail -1)"
        [ "$rc" -eq 0 ] && for name in "${PINNED[@]}"; do
            if ! grep -qx "$name: test" <<<"$listed"; then
                fail "libtest does not list $name — it is gone, renamed, or compiled out (cfg, cfg_attr, a module); rename it in PINNED only if that was deliberate"
            elif grep -qx "$name: test" <<<"$ignored"; then
                fail "libtest lists $name as ignored — it no longer runs"
            fi
        done
    fi
fi

if [ "$fails" -gt 0 ]; then
    echo "check_root_funnel_precondition: $fails problem(s) — Stage 12 may not compose kademlia, the relay client or authorised identify without the funnel's measurement (plan §15)." >&2
    exit 1
fi
if [ "$COMPILED" = 1 ]; then
    echo "check_root_funnel_precondition: OK — libtest lists the root funnel's ${#PINNED[@]} measurement tests and none is ignored; the tree runs them under cargo test --all-targets."
else
    echo "check_root_funnel_precondition: OK (tree) — root_funnel.rs is present and built through RootFunnel, and CI runs cargo test --all-targets; the compiled listing is checked in the rust job."
fi
