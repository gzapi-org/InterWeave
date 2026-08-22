#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_stage_status.sh
#
# >>> help
# Prove every human-facing statement of the open stage agrees with the
# one machine-readable value.
#
#   tools/checks/check_stage_status.sh
#   tools/checks/check_stage_status.sh --root <dir>
#
# `workspace.metadata.interweave.status` in the root Cargo.toml is the
# single source of truth, in the form `stage-<N>-<slug>`. Three prose
# copies of that fact live in README.md, IMPLEMENTATION.md, and
# CLAUDE.md, and all three drifted: after Stage 2 merged, every one of
# them still said "Stage 0 complete, Stage 1 open", and the Cargo.toml
# comment sitting directly above the status field contradicted the field
# it annotated.
#
# That matters more here than in most repositories, because CLAUDE.md
# section 3 makes the open stage an authorization boundary: an agent or a
# contributor reads the status, concludes which packages may be created,
# and builds the wrong thing. A stale status is not a documentation nit,
# it is a wrong instruction.
#
# Two checks:
#
#   OPEN     — each file states the CURRENT stage as open. A file that
#              names a different stage as open fails, whichever direction
#              it drifted.
#   ROSTER   — no file restates the workspace member list in prose. The
#              manifest is the roster; a sentence enumerating it goes
#              stale the moment a stage opens, which is exactly how the
#              last drift happened.
#
# Options:
#   --root <dir>   check this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  every statement agrees with the manifest
#   1  a statement disagrees, or a roster was restated in prose
#   2  invocation problem, or the status value is missing/malformed
# <<< help

set -uo pipefail

die() { printf '%s\n' "$*" >&2; exit 2; }

show_help() {
  sed -n '/^# >>> help$/,/^# <<< help$/p' "${BASH_SOURCE[0]}" \
    | sed '1d;$d' | sed 's/^# \{0,1\}//'
}

ROOT=""
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) show_help; exit 0 ;;
    --root) shift; [ $# -gt 0 ] || die "--root needs a value"; ROOT="$1" ;;
    *) die "check_stage_status: unexpected argument: $1" ;;
  esac
  shift
done

if [ -z "$ROOT" ]; then
  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi
[ -d "$ROOT" ] || die "check_stage_status: not a directory: $ROOT"

MANIFEST="$ROOT/Cargo.toml"
[ -f "$MANIFEST" ] || die "check_stage_status: no Cargo.toml at $ROOT"

STATUS="$(sed -n 's/^status = "\(stage-[0-9][0-9]*-[a-z0-9-]*\)"$/\1/p' "$MANIFEST" | head -1)"
[ -n "$STATUS" ] || die "check_stage_status: no 'status = \"stage-N-slug\"' in Cargo.toml"

STAGE="$(printf '%s' "$STATUS" | sed 's/^stage-\([0-9][0-9]*\)-.*$/\1/')"
[ -n "$STAGE" ] || die "check_stage_status: cannot read a stage number from '$STATUS'"

problems=0
report() { printf '%s\n' "$*"; problems=$((problems + 1)); }

# Files that state the open stage in prose.
FILES="README.md IMPLEMENTATION.md CLAUDE.md"

for rel in $FILES; do
  f="$ROOT/$rel"
  [ -f "$f" ] || { report "$rel: missing, but it states the repository status"; continue; }

  # OPEN — the file must name THIS stage as open.
  if ! grep -Eqi "stage $STAGE is open|stage $STAGE open|\`stage-$STAGE-" "$f"; then
    report "$rel: does not state that Stage $STAGE is open (manifest says $STATUS)"
  fi

  # And must not name any OTHER stage as open.
  #
  # BOTH PHRASINGS, matching what the positive check above accepts. The
  # exclusion scan used to recognise only "stage N is open", so a file
  # saying "Stage 4 open. Stage 1 open." passed while stating two
  # different open stages — and the no-`is` form is the one the README
  # actually uses, so the gap was over the common case.
  #
  # `\b` stops "stage 3 opened" being read as a claim that Stage 3 is
  # open.
  others="$(grep -Eoi "stage [0-9]+ (is )?open\b" "$f" \
            | grep -Evi "stage $STAGE (is )?open\b" | sort -u || true)"
  if [ -n "$others" ]; then
    while IFS= read -r line; do
      [ -n "$line" ] && report "$rel: says \"$line\" but the manifest says Stage $STAGE"
    done <<< "$others"
  fi

  # ROSTER — prose must not enumerate the workspace members.
  if grep -Eq 'members are .*(xtask|tests/support)|active members.*are .*(xtask|`crates/)' "$f"; then
    report "$rel: restates the workspace member list in prose; [workspace].members is the roster"
  fi
done

# EMPTY — no file may say the repository holds no production code once
# it does.
#
# This is the same drift `check_component_status.sh` catches one level
# down, and it went unnoticed for longer because nothing here looks at
# it: IMPLEMENTATION.md still opened with "there are no production Rust
# crates" across five completed stages, README.md called the repository
# a skeleton, and the Cargo.toml's first line said the same. A reader
# arriving at any of the three was told not to look.
#
# The trigger is the manifest itself. `xtask` and `tests/support` are
# scaffolding and do not make the claim false; anything else does.
#
# Read with awk rather than a sed range: `members = [...]` may be one
# line or many, and a range whose end pattern never matches runs to end
# of file -- which swept up `status = "stage-N-..."` and made the
# scaffolding-only tree look populated. `planned_members` further down
# is an inventory, not a roster, so the scan stops at the first `]`.
members="$(awk '
    /^members[[:space:]]*=[[:space:]]*\[/ { inside = 1 }
    inside { print }
    inside && /\]/ { exit }
  ' "$MANIFEST" \
           | grep -Eo '"[^"]+"' | tr -d '"' \
           | grep -Ev '^(xtask|tests/support)$' || true)"
if [ -n "$members" ]; then
  count="$(printf '%s\n' "$members" | wc -l | tr -d ' ')"
  EMPTY_CLAIM='no production Rust (crates|implementation)|workspace skeleton only|remains an architecture/skeleton repository'
  for rel in README.md IMPLEMENTATION.md CLAUDE.md Cargo.toml; do
    f="$ROOT/$rel"
    [ -f "$f" ] || continue
    hit="$(grep -Eoi "$EMPTY_CLAIM" "$f" | head -1 || true)"
    if [ -n "$hit" ]; then
      report "$rel: says \"$hit\" while [workspace].members holds $count production package(s)"
    fi
  done
fi

# The comment above the status field must not contradict it either. This
# is the one that went wrong last time: the value was updated and the
# comment three lines above it was not.
before="$(grep -B8 '^status = "' "$MANIFEST" || true)"
if printf '%s' "$before" | grep -Eqi "stage [0-9]+ of the canonical plan"; then
  named="$(printf '%s' "$before" | grep -Eoi "stage [0-9]+ of the canonical plan" | head -1)"
  report "Cargo.toml: the comment above 'status' says \"$named\" while the field says $STATUS"
fi

if [ "$problems" -gt 0 ]; then
  printf '\ncheck_stage_status: %d problem(s).\n' "$problems" >&2
  exit 1
fi

printf 'check_stage_status: OK — %s, and all %d prose statement(s) agree.\n' \
  "$STATUS" "$(printf '%s' "$FILES" | wc -w)"
