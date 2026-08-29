#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# A component README must not call itself unimplemented while its own
# directory holds a manifest and source.
#
# WHY THIS EXISTS
#
# `check_stage_status.sh` compares three top-level files against the
# manifest's open stage. It says nothing about the forty-odd component
# READMEs, and those drift in one direction only: a crate is activated,
# its code lands, and the line saying "planned crate boundary only; no
# Cargo.toml or Rust source yet" stays exactly where it was.
#
# Three of them were false at once — the libp2p substrate, the discovery
# cache, and the human store — each with a manifest, a src/ tree, and
# hundreds of lines of tested code, all describing themselves as not
# existing. That is worse than an out-of-date document: it is the first
# thing a reader opens, and it tells them not to look.
#
# The check is deliberately narrow. It does not judge what a README says
# about a crate's stage or its exclusions, because those are prose a
# person has to write. It asks one mechanical question that has one
# right answer: does this directory contain the things the README claims
# it does not.
#
# Exit codes:
#   0  every README agrees with its own directory
#   1  at least one claims to be a placeholder while holding code
#   2  invocation error

set -uo pipefail

usage() {
    cat <<'USAGE'
check_component_status.sh — component READMEs must not deny their own code

Usage:
  bash tools/checks/check_component_status.sh [--root DIR]

Options:
  --root DIR   repository root (default: the git toplevel)
  --help       this text
USAGE
}

ROOT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --root)
            # `shift 2` with one argument left FAILS, and with no
            # `set -e` the loop then spins on an unchanged $1 forever.
            [ $# -ge 2 ] || { echo "check_component_status: --root needs a directory" >&2; exit 2; }
            ROOT="$2"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) echo "check_component_status: unknown option '$1'" >&2; exit 2 ;;
    esac
done

if [ -z "$ROOT" ]; then
    ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
fi
[ -n "$ROOT" ] || { echo "check_component_status: no --root and not in a git repo" >&2; exit 2; }
[ -d "$ROOT" ] || { echo "check_component_status: not a directory: $ROOT" >&2; exit 2; }

# The phrase a placeholder README uses. Matched on its distinctive half
# so a reworded sentence around it is still caught.
PLACEHOLDER='no .Cargo\.toml. or Rust source yet'

problems=0
checked=0

while IFS= read -r readme; do
    [ -n "$readme" ] || continue
    dir="$(dirname "$readme")"
    # Only a directory that is itself a crate — a parent holding crates
    # is not making a claim about code it does not own.
    [ -f "$dir/Cargo.toml" ] || continue
    [ -d "$dir/src" ] || continue
    checked=$((checked + 1))
    if grep -Eq "$PLACEHOLDER" "$readme"; then
        rel="${readme#"$ROOT"/}"
        echo "$rel: says it has no Cargo.toml or Rust source, but $dir has both"
        problems=$((problems + 1))
    fi
done <<EOF
$(find "$ROOT" -name README.md -not -path '*/target/*' -not -path '*/.git/*' 2>/dev/null | sort)
EOF

if [ "$problems" -gt 0 ]; then
    printf '\ncheck_component_status: %d README(s) deny code that is present.\n' "$problems" >&2
    exit 1
fi

printf 'check_component_status: OK — %d component README(s) agree with their own directory.\n' \
    "$checked"
