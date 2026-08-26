#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_domain_fns_are_called.sh
#
# >>> help
# Does every public domain function have a caller outside its own file?
#
# A `pub fn` in a neutral API or a pure runtime module exists to be used
# by a backend. One that is fully written, fully unit-tested, and called
# by nothing is a rule that binds nothing — and it reads as covered,
# because its own tests are green and its documentation says what it
# enforces.
#
# THIS IS NOT HYPOTHETICAL. Stage 6 shipped two, both later P1 findings:
#
#   EndpointRegistry::authorize_outbound — endpoint outbound narrowing.
#     Implemented, tested, documented. The send path never called it, so
#     an endpoint configured to reach only some peers reached any
#     profile-trusted one.
#   FrameError::to_wire — the `too_large` / `malformed` split for a frame
#     that fails to decode. Reachable only from its own unit tests, so a
#     malformed frame produced a broken exchange instead of the
#     rejection the contract names.
#
# Neither was findable by reading the defining file: both looked
# complete there. What was missing was somewhere else entirely, which is
# what makes this mechanical rather than a review question.
#
# SCOPE. `crates/api/*` and `crates/transport/runtime/*` — the layers
# whose whole purpose is to be called from a backend. Only `pub`;
# `pub(crate)` announces a narrower audience and is out of scope.
#
# WHAT COUNTS AS A CALLER. Any mention of the name in a different
# tracked `.rs` file. Deliberately loose: this asks "does anyone
# anywhere know this exists", not "is there a call edge". A trait impl,
# a re-export or a doc link all count.
#
# EXEMPTIONS carry a DEADLINE. `tools/checks/domain_fn_exempt.txt` holds
# `<name> <stage-N> <reason>`, and an exemption expires once the open
# stage — `workspace.metadata.interweave.status` — is past stage N. That
# is the whole point: a flat allow-list is a snooze button, and
# `authorize_outbound` was written *in* the stage that was supposed to
# call it. An exemption that no longer applies, or names a function that
# no longer exists, also fails — a stale entry silently widens the list.
#
# Exit codes:
#   0  every public domain function is referred to somewhere else,
#      or is exempt with a stage still ahead
#   1  one or more are referred to nowhere, or an exemption is expired
#      or stale
#   2  the exemption file or the stage status could not be read
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
cd "$ROOT" || exit 2

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi

EXEMPT_FILE="${INTERWEAVE_DOMAIN_FN_EXEMPT:-tools/checks/domain_fn_exempt.txt}"
MANIFEST="${INTERWEAVE_MANIFEST:-Cargo.toml}"

# The open stage, from the one machine-readable place it is recorded.
status_line="$(grep -E '^status *= *"' "$MANIFEST" 2>/dev/null | head -1)"
open_stage="$(sed -E 's/.*"stage-([0-9]+).*/\1/' <<<"$status_line")"
if ! [[ "$open_stage" =~ ^[0-9]+$ ]]; then
    echo "check_domain_fns_are_called: cannot read the open stage from $MANIFEST." >&2
    echo "  Expected a workspace.metadata.interweave \`status = \"stage-N-...\"\` line." >&2
    exit 2
fi

# name -> "stage reason", parsed once.
declare -A exempt_stage exempt_reason
exempt_count=0
if [[ -f "$EXEMPT_FILE" ]]; then
    while read -r name stage reason; do
        [[ -z "${name:-}" || "$name" == \#* ]] && continue
        if ! [[ "$stage" =~ ^stage-([0-9]+)$ ]]; then
            echo "check_domain_fns_are_called: $EXEMPT_FILE: \`$name\` has no \`stage-N\` deadline (got '${stage:-}')." >&2
            exit 2
        fi
        if [[ -z "${reason// /}" ]]; then
            echo "check_domain_fns_are_called: $EXEMPT_FILE: \`$name\` has a deadline but no reason." >&2
            exit 2
        fi
        exempt_stage["$name"]="${BASH_REMATCH[1]}"
        exempt_reason["$name"]="$reason"
        exempt_count=$((exempt_count + 1))
    done < "$EXEMPT_FILE"
fi

mapfile -t domain < <(git ls-files 'crates/api/*.rs' 'crates/transport/runtime/*.rs' 2>/dev/null)
if [[ ${#domain[@]} -eq 0 ]]; then
    echo "check_domain_fns_are_called: no domain sources yet; nothing to check."
    exit 0
fi
mapfile -t all_rs < <(git ls-files '*.rs' 2>/dev/null)

problems=0
declare -A seen_exempt

for file in "${domain[@]}"; do
    while IFS= read -r name; do
        [[ -z "$name" ]] && continue

        # One grep per name across every tracked source, rather than one
        # per (name, file) pair: the same answer, a fraction of the forks.
        elsewhere=0
        while IFS= read -r hit; do
            [[ "$hit" != "$file" ]] && { elsewhere=1; break; }
        done < <(grep -lw -- "$name" "${all_rs[@]}" 2>/dev/null)

        if [[ -n "${exempt_stage[$name]:-}" ]]; then
            seen_exempt["$name"]=1
            if (( elsewhere == 1 )); then
                echo "check_domain_fns_are_called: $EXEMPT_FILE: \`$name\` is exempt but IS referred to elsewhere — drop the entry." >&2
                problems=$((problems + 1))
            elif (( exempt_stage[$name] < open_stage )); then
                echo "check_domain_fns_are_called: $file: \`$name\` is exempt until stage ${exempt_stage[$name]}, but stage $open_stage is open — the deadline passed." >&2
                problems=$((problems + 1))
            fi
            continue
        fi

        if (( elsewhere == 0 )); then
            echo "check_domain_fns_are_called: $file: \`$name\` is referred to nowhere else." >&2
            problems=$((problems + 1))
        fi
    done < <(grep -oE '^[[:space:]]*pub (const |async )?fn [a-z_][a-z0-9_]*' "$file" 2>/dev/null \
                | sed 's/.*fn //' | sort -u)
done

# An exemption naming a function that no longer exists stops protecting
# anything and starts hiding the next one that takes its name.
# `${!arr[@]}` on an empty associative array trips `set -u`, so the
# iteration is guarded rather than the array pre-seeded: a sentinel entry
# would be an exemption nobody wrote.
for name in ${exempt_stage[@]+"${!exempt_stage[@]}"}; do
    [[ -n "${seen_exempt[$name]:-}" ]] && continue
    echo "check_domain_fns_are_called: $EXEMPT_FILE: \`$name\` names no public domain function — stale entry." >&2
    problems=$((problems + 1))
done

if (( problems > 0 )); then
    cat >&2 <<EOF

A public domain function nothing refers to is a rule that binds nothing,
and it reads as covered because its own tests are green. Stage 6 shipped
two that way, and both came back as P1 findings.

Either call it, or give it a dated deadline in $EXEMPT_FILE:

    <fn name>  stage-N  <why, and what will call it>

EOF
    exit 1
fi

echo "check_domain_fns_are_called: OK — ${#domain[@]} domain sources, $exempt_count exemptions, all deadlines ahead of stage $open_stage."
