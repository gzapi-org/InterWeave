#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_root_funnel_precondition.sh
#
# >>> help
# Does the root funnel's measurement still exist, and still run?
#
#   bash tools/checks/check_root_funnel_precondition.sh [--root DIR]
#
# The plan's Stage 12 (BOTTOM-UP-IMPLEMENTATION-PLAN.md §15, ADR-0052
# rule 5) may compose Kademlia, the relay client, or the authorised-
# identify opt-ins only while the root funnel — the wrapper pruning the
# union the Swarm dials from — has its measurement committed as a test:
# a behaviour-extended dial toward a loopback and a /dns4 name opens no
# socket through the wrapper, and does with it removed. The test is the
# record, and CI re-measures it on every run. Until this check the
# precondition was remembered; §15 asked for it to be mechanical
# (p2p-network-dev proposed, devex-tooling wires).
#
# UNCONDITIONAL, deliberately. The relay client is one of the four, and
# profile-config pins `transport.connectivity.relay.client.enabled` to
# literal true with a Default for the block, so EVERY valid profile
# enables it whether or not the key is written (p2p-network-dev,
# 2026-09-27). A trigger keyed on the literal would fire only where an
# author happened to spell it out.
#
# WHAT IS CHECKED, in crates/transport/libp2p:
#   1. tests/root_funnel.rs exists.
#   2. Each measurement test is present by name as a test function —
#      attributed #[test] or #[tokio::test…], never #[ignore]. They are
#      pairs, a control and its prune; a control alone asserts nothing,
#      so all four are pinned:
#        the_control_kademlia_dials_what_its_table_holds
#        the_root_funnel_prunes_what_a_behaviour_extends_a_dial_with
#        the_control_tcp_dials_the_last_host_of_a_stacked_address
#        the_root_funnel_prunes_a_stacked_address
#      Renaming one is a deliberate act: edit PINNED below with it.
#   3. The file still builds its Swarm through the wrapper
#      (`RootFunnel::new(`), so a test reduced to the control cannot pass
#      as the measurement.
#   4. The crate does not switch off test discovery (`autotests = false`)
#      unless a [[test]] names root_funnel.
#   5. .github/workflows/ci.yml still runs `cargo test --workspace
#      --all-targets` without excluding this crate — which is what makes
#      "present" mean "passes on every run".
#
# NOT CHECKED: that the tests measure what their names say. That is the
# test's own job and its review's; this keeps the record from vanishing.
#
# Exit codes:
#   0  the measurement is present and runs
#   1  a precondition is missing (each named)
#   2  invocation problem (bad --root)
# <<< help

set -uo pipefail

ROOT=""
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'; exit 0 ;;
        --root) ROOT="${2:-}"; shift 2 ;;
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
else
    for name in "${PINNED[@]}"; do
        # The attribute block is the run of #[…], /// and // lines directly
        # above `fn name(`; it must hold a test attribute and no #[ignore].
        verdict="$(awk -v name="$name" '
            { line[NR] = $0 }
            $0 ~ ("fn " name "\\(") { at = NR }
            END {
                if (!at) { print "absent"; exit }
                test = 0; ignored = 0
                for (i = at - 1; i > 0; i--) {
                    l = line[i]; sub(/^[ \t]+/, "", l)
                    if (l ~ /^#\[/ || l ~ /^\/\//) {
                        if (l ~ /^#\[(tokio::)?test([(\]]|$)/) test = 1
                        if (l ~ /^#\[ignore/) ignored = 1
                    } else break
                }
                if (ignored) print "ignored"; else if (!test) print "not-a-test"; else print "ok"
            }' "$TEST")"
        case "$verdict" in
            ok) ;;
            absent) fail "tests/root_funnel.rs has no test $name — a measurement test is gone (rename it in PINNED if that was deliberate)" ;;
            ignored) fail "tests/root_funnel.rs: $name is #[ignore]d — it no longer runs" ;;
            *) fail "tests/root_funnel.rs: $name carries no #[test] / #[tokio::test] — it does not run" ;;
        esac
    done
    grep -q 'RootFunnel::new(' "$TEST" \
        || fail "tests/root_funnel.rs never builds a Swarm through RootFunnel::new( — the prune side of the measurement is gone"
fi

if [ -f "$CRATE/Cargo.toml" ] && grep -Eq '^[[:space:]]*autotests[[:space:]]*=[[:space:]]*false' "$CRATE/Cargo.toml"; then
    awk '/^\[\[test\]\]/ {t=1; next} /^\[/ {t=0} t && /^[[:space:]]*name[[:space:]]*=[[:space:]]*"root_funnel"/ {found=1} END {exit !found}' "$CRATE/Cargo.toml" \
        || fail "crates/transport/libp2p/Cargo.toml sets autotests = false and no [[test]] names root_funnel — the test is not built"
fi

if [ ! -f "$CI" ]; then
    fail ".github/workflows/ci.yml is missing — nothing runs the measurement"
elif ! grep -Eq 'cargo test .*--workspace.*--all-targets|cargo test .*--all-targets.*--workspace' "$CI"; then
    fail ".github/workflows/ci.yml no longer runs cargo test --workspace --all-targets — present no longer means passes"
elif grep -Eq 'cargo test .*--exclude[ =]+interweave-transport-libp2p' "$CI"; then
    fail ".github/workflows/ci.yml excludes interweave-transport-libp2p from cargo test — the measurement never runs"
fi

if [ "$fails" -gt 0 ]; then
    echo "check_root_funnel_precondition: $fails problem(s) — Stage 12 may not compose kademlia, the relay client or authorised identify without the funnel's measurement (plan §15)." >&2
    exit 1
fi
echo "check_root_funnel_precondition: OK — the root funnel's ${#PINNED[@]} measurement tests are present, run under cargo test --all-targets, and build through RootFunnel."
