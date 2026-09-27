#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/test_check_root_funnel_precondition.sh
#
# Self-test for check_root_funnel_precondition.sh: a scaffolded tree with
# the four measurement tests, the wrapper call and the CI line, then one
# precondition broken per case.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more assertions failed

set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CHECK="$HERE/check_root_funnel_precondition.sh"
failures=0
SANDBOX="$(mktemp -d)" || { echo "test_check_root_funnel_precondition: mktemp failed" >&2; exit 1; }
trap 'rm -rf "$SANDBOX"' EXIT

PINNED="the_control_kademlia_dials_what_its_table_holds the_root_funnel_prunes_what_a_behaviour_extends_a_dial_with the_control_tcp_dials_the_last_host_of_a_stacked_address the_root_funnel_prunes_a_stacked_address"

scaffold() {
    # $1 root; builds a tree that passes
    rm -rf "$1"
    mkdir -p "$1/crates/transport/libp2p/tests" "$1/.github/workflows"
    printf '[package]\nname = "interweave-transport-libp2p"\n' > "$1/crates/transport/libp2p/Cargo.toml"
    {
        echo 'use interweave_transport_libp2p::root_funnel::RootFunnel;'
        for n in $PINNED; do
            printf '/// doc\n#[tokio::test]\nasync fn %s() {\n    let _ = RootFunnel::new(a, b);\n}\n\n' "$n"
        done
    } > "$1/crates/transport/libp2p/tests/root_funnel.rs"
    printf 'jobs:\n  test:\n    steps:\n      - run: cargo test --workspace --all-targets --locked\n' > "$1/.github/workflows/ci.yml"
}

expect() {  # expect <name> <want-exit> <root> [substring]
    local name="$1" want="$2" root="$3" sub="${4:-}" out got
    out="$(bash "$CHECK" --root "$root" 2>&1)"; got=$?
    if [ "$got" != "$want" ] || { [ -n "$sub" ] && [[ "$out" != *"$sub"* ]]; }; then
        echo "FAIL $name: wanted exit $want${sub:+ and \"$sub\"}, got $got" >&2
        printf '%s\n' "$out" | sed 's/^/      /' >&2
        failures=$((failures + 1))
    else
        echo "ok   $name"
    fi
}

R="$SANDBOX/r"
T="$R/crates/transport/libp2p/tests/root_funnel.rs"

scaffold "$R"; expect "the measurement present and run: passes" 0 "$R" "OK"

scaffold "$R"; rm "$T"; expect "the test file missing: refused" 1 "$R" "root_funnel.rs is missing"

scaffold "$R"; sed -i 's/the_root_funnel_prunes_a_stacked_address/renamed_away/' "$T"
expect "a pinned prune test renamed away: refused, naming it" 1 "$R" "no test the_root_funnel_prunes_a_stacked_address"

scaffold "$R"; sed -i '0,/#\[tokio::test\]/s//#[tokio::test]\n#[ignore]/' "$T"
expect "a pinned test #[ignore]d: refused" 1 "$R" "the_control_kademlia_dials_what_its_table_holds is #[ignore]d"

scaffold "$R"; sed -i '0,/#\[tokio::test\]/s//#[allow(dead_code)]/' "$T"
expect "a pinned function with no test attribute: refused" 1 "$R" "carries no #[test]"

scaffold "$R"; sed -i 's/#\[tokio::test\]/#[test]/' "$T"
expect "a plain #[test] attribute counts" 0 "$R"

# #134 review F1: a test the compiler never builds, or runs as ignored.
scaffold "$R"; sed -i '0,/#\[tokio::test\]/s//#[cfg(any())]\n#[tokio::test]/' "$T"
expect "a pinned test under #[cfg(any())]: refused" 1 "$R" "sits under #[cfg("

scaffold "$R"; sed -i '0,/#\[tokio::test\]/s//#[tokio::test]\n#[cfg_attr(all(), ignore)]/' "$T"
expect "a pinned test ignored through cfg_attr: refused" 1 "$R" "is #[ignore]d (directly or through cfg_attr)"

scaffold "$R"; sed -i '1i #![cfg(any())]' "$T"
expect "a file-level #![cfg]: refused" 1 "$R" "file-level #![cfg"

# #134 re-review F1: attributes as rustfmt writes them, and module nesting.
scaffold "$R"; sed -i '0,/#\[tokio::test\]/s//#[cfg_attr(\n    all(),\n    ignore = "a reason long enough that rustfmt wraps the attribute"\n)]\n#[tokio::test]/' "$T"
expect "a rustfmt-wrapped cfg_attr(…, ignore): refused" 1 "$R" "is #[ignore]d (directly or through cfg_attr)"

scaffold "$R"; sed -i '0,/#\[tokio::test\]/s//#[cfg(\n    any()\n)]\n#[tokio::test]/' "$T"
expect "a wrapped #[cfg(…)]: refused" 1 "$R" "sits under #[cfg("

scaffold "$R"; sed -i '0,/#\[tokio::test\]/s//#[tokio::test] #[ignore]/' "$T"
expect "two attributes on one line, one of them #[ignore]: refused" 1 "$R" "is #[ignore]d"

scaffold "$R"; sed -i 's/^async fn the_control_kademlia_dials_what_its_table_holds() {/#[cfg(any())]\nmod off {\n    #[tokio::test]\n    async fn the_control_kademlia_dials_what_its_table_holds() {/' "$T"; printf '}\n' >> "$T"
expect "a pinned test inside a module: refused" 1 "$R" "is not a top-level function"

scaffold "$R"; sed -i '1i #![cfg_attr(test, allow(dead_code))]' "$T"
expect "a harmless file-level #![cfg_attr(…)]: passes" 0 "$R"

scaffold "$R"; sed -i 's/^async fn the_root_funnel_prunes_a_stacked_address()/async fn gone()/' "$T"; printf '// fn the_root_funnel_prunes_a_stacked_address() was here\nfn xthe_root_funnel_prunes_a_stacked_address() {}\n' >> "$T"
expect "a comment or a longer name does not stand in for a pinned test" 1 "$R" "no test the_root_funnel_prunes_a_stacked_address"

scaffold "$R"; sed -i 's/RootFunnel::new(/Composite::new(/' "$T"
expect "no Swarm built through RootFunnel::new: refused" 1 "$R" "never builds a Swarm through RootFunnel::new("

scaffold "$R"; printf 'autotests = false\n' >> "$R/crates/transport/libp2p/Cargo.toml"
expect "autotests = false with no [[test]] for it: refused" 1 "$R" "autotests = false"

scaffold "$R"; printf 'autotests = false\n\n[[test]]\nname = "root_funnel"\n' >> "$R/crates/transport/libp2p/Cargo.toml"
expect "autotests = false with a [[test]] naming root_funnel: passes" 0 "$R"

scaffold "$R"; sed -i 's/cargo test --workspace --all-targets --locked/cargo test --workspace --lib/' "$R/.github/workflows/ci.yml"
expect "CI no longer runs --all-targets: refused" 1 "$R" "has no command running cargo test --workspace --all-targets"

scaffold "$R"; sed -i 's/--locked/--locked --exclude interweave-transport-libp2p/' "$R/.github/workflows/ci.yml"
expect "CI excludes the crate: refused" 1 "$R" "excludes interweave-transport-libp2p"

# #134 review F3: only a command that runs everything counts.
scaffold "$R"; printf 'jobs:\n  test:\n    steps:\n      - run: cargo build\n# was: cargo test --workspace --all-targets --locked\n' > "$R/.github/workflows/ci.yml"
expect "the command only in a YAML comment: refused" 1 "$R" "has no command running"

scaffold "$R"; sed -i 's/--locked/--locked --no-run/' "$R/.github/workflows/ci.yml"
expect "cargo test --no-run: refused" 1 "$R" "has no command running"

scaffold "$R"; sed -i 's/--locked/--locked -- --skip root_funnel/' "$R/.github/workflows/ci.yml"
expect "cargo test -- --skip: refused" 1 "$R" "has no command running"

# #134 re-review F3: libtest arguments and a swallowed status.
scaffold "$R"; sed -i 's/--locked/--locked -- --ignored/' "$R/.github/workflows/ci.yml"
expect "cargo test -- --ignored: refused" 1 "$R" "has no command running"

scaffold "$R"; sed -i 's/--locked/--locked -- some_filter/' "$R/.github/workflows/ci.yml"
expect "cargo test -- <name filter>: refused" 1 "$R" "has no command running"

scaffold "$R"; sed -i 's/--locked/--locked || true/' "$R/.github/workflows/ci.yml"
expect "cargo test … || true: refused" 1 "$R" "has no command running"

scaffold "$R"; printf 'jobs:\n  test:\n    steps:\n      - run: |\n          set -e\n          cargo test --workspace --all-targets --locked\n' > "$R/.github/workflows/ci.yml"
expect "the command inside a run: | block counts" 0 "$R"

scaffold "$R"; rm "$R/.github/workflows/ci.yml"
expect "no ci.yml at all: refused" 1 "$R" "ci.yml is missing"

scaffold "$R"; rm "$T"; sed -i 's/--all-targets/--lib/' "$R/.github/workflows/ci.yml"
out="$(bash "$CHECK" --root "$R" 2>&1)"
if [[ "$out" == *"2 problem(s)"* ]]; then echo "ok   several problems: each named, all counted"; else echo "FAIL several problems not all counted" >&2; printf '%s\n' "$out" | sed 's/^/      /' >&2; failures=$((failures + 1)); fi

bash "$CHECK" --root "$SANDBOX/nowhere" >/dev/null 2>&1; got=$?
if [ "$got" = 2 ]; then echo "ok   a --root that is not a directory: exit 2"; else echo "FAIL bad --root gave $got" >&2; failures=$((failures + 1)); fi

# #134 review F2: --root with no value exits 2, never loops.
timeout 10 bash "$CHECK" --root >/dev/null 2>&1; got=$?
if [ "$got" = 2 ]; then echo "ok   --root with no value: exit 2, no loop"; else echo "FAIL --root with no value gave $got (124 = hung)" >&2; failures=$((failures + 1)); fi

bash "$CHECK" --help 2>&1 | grep -q "Does the root funnel's measurement still exist" \
    && echo "ok   --help prints the header" || { echo "FAIL --help" >&2; failures=$((failures + 1)); }

if [ "$failures" -gt 0 ]; then echo "test_check_root_funnel_precondition: FAILED — $failures assertion(s)" >&2; exit 1; fi
echo "test_check_root_funnel_precondition: OK"
