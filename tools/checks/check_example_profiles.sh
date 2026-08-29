#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# A shipped example profile must satisfy the bounds the implementation
# enforces, not only the ones the schema states.
#
# WHY THIS EXISTS
#
# `config.schema.yaml` declares `max_entries: int` with no maximum, and
# validation enforces the peer cache's frozen 1024-peer ceiling on top of
# it — because a profile that cannot build the cache it configures is not
# a valid profile. The schema and the implementation therefore disagree
# by design, and an example written against the schema alone is refused
# by the code that reads it.
#
# That is exactly what shipped: `connectivity-infrastructure.yaml` set
# `max_entries: 4096`, so the canonical profile an operator is handed was
# rejected by the validator the same commit had tightened. Nothing
# compared the two, because the examples are prose to every existing
# check.
#
# Deliberately narrow. This checks the bounds where the IMPLEMENTATION is
# stricter than the schema — the only place an example can be
# schema-correct and still refused. Bounds the schema states are the
# schema's to police, and full profile validation would need a YAML
# parser these stdlib checks do not have.
#
# Exit codes:
#   0  every example is within the implementation's bounds
#   1  at least one example configures something the code refuses
#   2  invocation error, or a file could not be read

set -uo pipefail

usage() {
    cat <<'USAGE'
check_example_profiles.sh — shipped examples must satisfy the bounds the
implementation enforces beyond the schema

Usage:
  bash tools/checks/check_example_profiles.sh [--root DIR]

Options:
  --root DIR   repository root (default: the git toplevel)
  --help       this text

Covers `peer-cache.max_entries` against `CACHE_MAX_PEERS` in
crates/config/profile-config/src/lib.rs, which is the peer cache's frozen
ceiling and is stricter than the schema's unbounded `int`.
USAGE
}

ROOT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --root)
            # `shift 2` with one argument left FAILS, and with no
            # `set -e` the loop then spins on an unchanged $1 forever.
            [ $# -ge 2 ] || { echo "check_example_profiles: --root needs a directory" >&2; exit 2; }
            ROOT="$2"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) echo "check_example_profiles: unknown option '$1'" >&2; exit 2 ;;
    esac
done

if [ -z "$ROOT" ]; then
    ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
fi
[ -n "$ROOT" ] || { echo "check_example_profiles: no --root and not in a git repo" >&2; exit 2; }

RUST="$ROOT/crates/config/profile-config/src/lib.rs"
EXAMPLES="$ROOT/architecture/config/examples"
[ -r "$RUST" ] || { echo "check_example_profiles: cannot read $RUST" >&2; exit 2; }
[ -d "$EXAMPLES" ] || { echo "check_example_profiles: cannot read $EXAMPLES" >&2; exit 2; }

# The ceiling, read from the code that enforces it rather than restated.
CEILING="$(sed -n 's/^pub const CACHE_MAX_PEERS: usize = \([0-9_]*\);.*/\1/p' "$RUST" | tr -d '_')"
[ -n "$CEILING" ] || {
    echo "check_example_profiles: CACHE_MAX_PEERS not found — the constant moved" >&2
    exit 2
}

problems=0
checked=0
while IFS= read -r file; do
    while IFS= read -r value; do
        checked=$((checked + 1))
        if [ "$value" -gt "$CEILING" ] || [ "$value" -lt 1 ]; then
            printf 'check_example_profiles: %s: max_entries %s is outside 1..=%s\n' \
                "${file#"$ROOT/"}" "$value" "$CEILING" >&2
            problems=$((problems + 1))
        fi
    done < <(grep -oE 'max_entries: *[0-9]+' "$file" 2>/dev/null | grep -oE '[0-9]+$')
done < <(find "$EXAMPLES" -name '*.yaml' -type f | sort)

if [ "$problems" -ne 0 ]; then
    cat >&2 <<'WHY'

The schema leaves `max_entries` unbounded; validation enforces the peer
cache's frozen ceiling on top of it, because a profile that cannot build
its cache is not a valid profile. An example past that ceiling is handed
to operators and then refused by the code that reads it.
WHY
    exit 1
fi

printf 'check_example_profiles: OK — %d max_entries value(s) within 1..=%s.\n' \
    "$checked" "$CEILING"
