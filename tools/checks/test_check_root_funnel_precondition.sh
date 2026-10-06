#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/test_check_root_funnel_precondition.sh
#
# Self-test for check_root_funnel_precondition.sh.
#
# The TREE half on a scaffolded tree, one precondition broken per case.
# The COMPILED half with a fake cargo (the CARGO override) that prints the
# libtest listing a case scripts: which tests the compiler built, which it
# built as ignored, or a build failure. What that listing IS for a given
# source — cfg, cfg_attr, a module, a wrapped attribute — is the compiler's
# answer, measured on the real file when this approach was chosen (#134: a
# #[cfg_attr(all(), cfg(any()))] test vanished from --list; an #[ignore]d
# one appeared under --list --ignored), so it is not re-derived here.
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
    # $1 root; builds a tree whose tree check passes
    rm -rf "$1"
    mkdir -p "$1/crates/transport/libp2p/tests" "$1/.github/workflows"
    printf '[package]\nname = "interweave-transport-libp2p"\n' > "$1/crates/transport/libp2p/Cargo.toml"
    {
        echo 'use interweave_transport_libp2p::root_funnel::RootFunnel;'
        for n in $PINNED; do
            printf '#[tokio::test]\nasync fn %s() {\n    let _ = RootFunnel::new(a, b);\n}\n\n' "$n"
        done
    } > "$1/crates/transport/libp2p/tests/root_funnel.rs"
    printf 'jobs:\n  test:\n    steps:\n      - run: cargo test --workspace --all-targets --locked\n' > "$1/.github/workflows/ci.yml"
}

# fake_cargo <listed names> <ignored names> [build-fails]
fake_cargo() {
    cat > "$SANDBOX/cargo" <<FAKE
#!/usr/bin/env bash
case " \$* " in *" --ignored "*) [ "${3:-}" = ignored-list-fails ] && { echo "error: target dir locked"; exit 101; }; for n in $2; do echo "\$n: test"; done; exit 0 ;; esac
[ "${3:-}" = build-fails ] && { echo "error[E0308]: mismatched types"; exit 101; }
for n in $1; do echo "\$n: test"; done
echo "an_extra_test: test"
FAKE
    chmod +x "$SANDBOX/cargo"
}

expect() {  # expect <name> <want-exit> <root> <substring> [--compiled]
    local name="$1" want="$2" root="$3" sub="${4:-}" mode="${5:-}" out got
    if [ -n "$mode" ]; then
        out="$(CARGO="$SANDBOX/cargo" bash "$CHECK" --root "$root" "$mode" 2>&1)"; got=$?
    else
        out="$(CARGO="$SANDBOX/cargo" bash "$CHECK" --root "$root" 2>&1)"; got=$?
    fi
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

# ── the tree ─────────────────────────────────────────────────────────
scaffold "$R"; expect "the tree: passes, and says the listing is the rust job's" 0 "$R" "OK (tree)"
scaffold "$R"; rm "$T"; expect "the test file missing: refused" 1 "$R" "root_funnel.rs is missing"
scaffold "$R"; sed -i 's/RootFunnel::new(/Composite::new(/' "$T"
expect "no Swarm built through RootFunnel::new: refused" 1 "$R" "never builds its Swarm through RootFunnel::new("
# The funnel used only by OTHER tests: each prune test is judged by its own
# body (#134's review: a file-wide grep let both prunes drop it).
scaffold "$R"; printf '#[tokio::test]\nasync fn an_operators_address_passes() {\n    let _ = RootFunnel::new(a, b);\n}\n' >> "$T"
python3 - "$T" <<'PY'
import re, sys
p = sys.argv[1]; s = open(p).read()
for n in ("the_root_funnel_prunes_what_a_behaviour_extends_a_dial_with", "the_root_funnel_prunes_a_stacked_address"):
    s = s.replace(f"async fn {n}() {{\n    let _ = RootFunnel::new(a, b);", f"async fn {n}() {{\n    let _ = composite(a);")
open(p, "w").write(s)
PY
expect "the funnel only in an unpinned test: refused, naming the prune" 1 "$R" "the_root_funnel_prunes_a_stacked_address never builds its Swarm"
# The body ends at the fn's own closing brace: a use in the NEXT function
# does not count for this one.
scaffold "$R"; python3 - "$T" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
s = s.replace("async fn the_root_funnel_prunes_a_stacked_address() {\n    let _ = RootFunnel::new(a, b);\n}",
              "async fn the_root_funnel_prunes_a_stacked_address() {\n    let _ = composite(a);\n}\n\nfn helper() {\n    let _ = RootFunnel::new(a, b);\n}")
open(p, "w").write(s)
PY
expect "a use after the prune's closing brace: refused" 1 "$R" "the_root_funnel_prunes_a_stacked_address never builds its Swarm"
scaffold "$R"; sed -i 's/fn the_root_funnel_prunes_a_stacked_address/fn renamed_prune/' "$T"
expect "a prune test gone by its name: refused" 1 "$R" "defines no fn the_root_funnel_prunes_a_stacked_address"
# Inside a module, indented: the closing brace is found at the fn's indent.
scaffold "$R"; python3 - "$T" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
s = s.replace("#[tokio::test]\nasync fn the_root_funnel_prunes_a_stacked_address() {\n    let _ = RootFunnel::new(a, b);\n}",
              "mod stacked {\n    #[tokio::test]\n    async fn the_root_funnel_prunes_a_stacked_address() {\n        if x {\n        }\n        let _ = RootFunnel::new(a, b);\n    }\n}")
open(p, "w").write(s)
PY
expect "an indented prune in a module, the funnel after an inner block: passes" 0 "$R" "OK (tree)"
scaffold "$R"; printf 'autotests = false\n' >> "$R/crates/transport/libp2p/Cargo.toml"
expect "autotests = false with no [[test]] for it: refused" 1 "$R" "autotests = false"
scaffold "$R"; printf 'autotests = false\n\n[[test]]\nname = "root_funnel"\n' >> "$R/crates/transport/libp2p/Cargo.toml"
expect "autotests = false with a [[test]] naming root_funnel: passes" 0 "$R" ""

# The CI command: only one that runs everything and fails CI on a failure.
scaffold "$R"; sed -i 's/--all-targets/--lib/' "$R/.github/workflows/ci.yml"
expect "CI no longer runs --all-targets: refused" 1 "$R" "has no command running"
scaffold "$R"; printf 'jobs:\n  test:\n    steps:\n      - run: cargo build\n# was: cargo test --workspace --all-targets --locked\n' > "$R/.github/workflows/ci.yml"
expect "the command only in a YAML comment: refused" 1 "$R" "has no command running"
for variant in "--no-run" "-- --skip root_funnel" "-- --ignored" "-- some_filter" "|| true"; do
    scaffold "$R"; sed -i "s/--locked/--locked $variant/" "$R/.github/workflows/ci.yml"
    expect "cargo test … $variant: refused" 1 "$R" "has no command running"
done
scaffold "$R"; sed -i 's/--locked/--locked --exclude interweave-transport-libp2p/' "$R/.github/workflows/ci.yml"
expect "CI excludes the crate: refused" 1 "$R" "excludes interweave-transport-libp2p"
scaffold "$R"; printf 'jobs:\n  test:\n    steps:\n      - run: |\n          set -e\n          cargo test --workspace --all-targets --locked\n' > "$R/.github/workflows/ci.yml"
expect "the command inside a run: | block counts" 0 "$R" ""
# Under the session wrapper the Tests step runs in: it counts, and the
# refusals above still apply through it; any other prefix does not count.
scaffold "$R"; sed -i 's|run: cargo test|run: bash tools/ci/with_display.sh cargo test|' "$R/.github/workflows/ci.yml"
expect "the command under tools/ci/with_display.sh counts" 0 "$R" ""
for variant in "--no-run" "|| true"; do
    scaffold "$R"; sed -i "s|run: cargo test|run: bash tools/ci/with_display.sh cargo test|; s/--locked/--locked $variant/" "$R/.github/workflows/ci.yml"
    expect "the wrapped command with $variant: refused" 1 "$R" "has no command running"
done
for prefix in "echo" "bash tools/ci/other.sh" "true ||"; do
    scaffold "$R"; sed -i "s#run: cargo test#run: $prefix cargo test#" "$R/.github/workflows/ci.yml"
    grep -qF -- "$prefix cargo test" "$R/.github/workflows/ci.yml" || { echo "FAIL the '$prefix' case did not apply" >&2; failures=$((failures + 1)); }
    expect "cargo test behind '$prefix': refused" 1 "$R" "has no command running"
done
scaffold "$R"; rm "$R/.github/workflows/ci.yml"
expect "no ci.yml at all: refused" 1 "$R" "ci.yml is missing"
scaffold "$R"; rm "$T"; sed -i 's/--all-targets/--lib/' "$R/.github/workflows/ci.yml"
expect "several problems: each named, all counted" 1 "$R" "2 problem(s)"

# ── the compiled listing (--compiled) ────────────────────────────────
scaffold "$R"; fake_cargo "$PINNED" ""
expect "all four listed, none ignored: passes" 0 "$R" "libtest lists the root funnel's 4 measurement tests" --compiled
scaffold "$R"; fake_cargo "${PINNED/the_control_kademlia_dials_what_its_table_holds/}" ""
expect "a pinned test not listed (gone, renamed, compiled out): refused, naming it" 1 "$R" "libtest does not list the_control_kademlia_dials_what_its_table_holds" --compiled
scaffold "$R"; fake_cargo "$PINNED" "the_root_funnel_prunes_a_stacked_address"
expect "a pinned test listed as ignored: refused, naming it" 1 "$R" "lists the_root_funnel_prunes_a_stacked_address as ignored" --compiled
scaffold "$R"; fake_cargo "$PINNED" "an_unpinned_test"
expect "an unpinned ignored test: not this check's business" 0 "$R" "" --compiled
scaffold "$R"; fake_cargo "$PINNED" "" build-fails
expect "the test binary does not build: refused, with cargo's last line" 1 "$R" "mismatched types" --compiled
scaffold "$R"; fake_cargo "$PINNED" "" ignored-list-fails
expect "the ignored listing fails: refused, never read as none ignored" 1 "$R" "could not list the ignored" --compiled
scaffold "$R"; fake_cargo "the_control_kademlia_dials_what_its_table_holds_v2" ""
expect "a longer name does not stand in for a pinned one" 1 "$R" "does not list the_control_kademlia_dials_what_its_table_holds " --compiled
scaffold "$R"; fake_cargo "${PINNED/the_control_kademlia/off::the_control_kademlia}" ""
expect "a pinned name under a module path is another test: refused" 1 "$R" "does not list the_control_kademlia_dials_what_its_table_holds " --compiled
scaffold "$R"; fake_cargo "$PINNED" ""; sed -i 's/--all-targets/--lib/' "$R/.github/workflows/ci.yml"
expect "the tree is still checked under --compiled" 1 "$R" "has no command running" --compiled
scaffold "$R"; out="$(CARGO="$SANDBOX/no-such-cargo" bash "$CHECK" --root "$R" --compiled 2>&1)"; got=$?
if [ "$got" = 2 ]; then echo "ok   --compiled with no cargo: exit 2"; else echo "FAIL --compiled with no cargo gave $got" >&2; printf '%s\n' "$out" | sed 's/^/      /' >&2; failures=$((failures + 1)); fi

# ── invocation ───────────────────────────────────────────────────────
bash "$CHECK" --root "$SANDBOX/nowhere" >/dev/null 2>&1; got=$?
if [ "$got" = 2 ]; then echo "ok   a --root that is not a directory: exit 2"; else echo "FAIL bad --root gave $got" >&2; failures=$((failures + 1)); fi
timeout 10 bash "$CHECK" --root >/dev/null 2>&1; got=$?
if [ "$got" = 2 ]; then echo "ok   --root with no value: exit 2, no loop"; else echo "FAIL --root with no value gave $got (124 = hung)" >&2; failures=$((failures + 1)); fi
bash "$CHECK" --bogus >/dev/null 2>&1; got=$?
if [ "$got" = 2 ]; then echo "ok   an unknown argument: exit 2"; else echo "FAIL unknown argument gave $got" >&2; failures=$((failures + 1)); fi
bash "$CHECK" --help 2>&1 | grep -q "Does the root funnel's measurement still exist" \
    && echo "ok   --help prints the header" || { echo "FAIL --help" >&2; failures=$((failures + 1)); }

if [ "$failures" -gt 0 ]; then echo "test_check_root_funnel_precondition: FAILED — $failures assertion(s)" >&2; exit 1; fi
echo "test_check_root_funnel_precondition: OK"
