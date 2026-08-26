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
# WHAT COUNTS AS A CALLER. A mention of the name in a different tracked
# `.rs` file — and, for a method, a mention of its enclosing type in
# that same file, OR a call in method position. Deliberately loose about
# the call itself: this asks
# "does anyone anywhere know this exists", not "is there a call edge".
# A trait impl, a re-export or a doc link all count.
#
# The enclosing type is not a refinement, it is the difference between
# working and not. Matching a bare name means every `new`, `len` and
# `is_empty` in the tree vouches for every other one: `ObservedCandidates
# ::new` is referenced nowhere outside its own file, and sixteen
# unrelated `new` methods were enough to let it pass. Requiring the type
# name to appear too costs nothing for a genuinely-used method — a file
# that calls `EndpointQueues::len` names `EndpointQueues` — and removes
# the whole class.
#
# There is deliberately NO implicit escape for a method call. Two were
# tried and both handed back the false-green the type rule removed. A
# bare `.<name>(` let unrelated `.len(` calls vouch for
# `OfferedAddresses::len`. Matching the receiver identifier against the
# owner's snake_case then reported seven genuinely-called functions as
# uncalled — `queues.is_open()`, `handle.admit()`, `r.release_session()`
# — because Rust receivers are named for their role, not their type.
#
# Textual analysis cannot attribute a method call to a type, so the check
# does not pretend to. A call it cannot see is recorded explicitly and
# VERIFIED, which is what `call` exemptions below are for.
#
# EXEMPTIONS come in two kinds, both in `tools/checks/domain_fn_exempt.txt`
# and both qualified as `Type::name`, because a bare `new` would exempt
# sixteen unrelated constructors at once.
#
#   <Type::name> stage-N <reason>   nothing calls it YET; the stage that
#                                   will is a deadline (see below).
#   <Type::name> call <expr>        it IS called, in method position the
#                                   check cannot attribute. `<expr>` is
#                                   the literal call, e.g.
#                                   `refusal.to_wire(`, and the check
#                                   fails unless that text still appears
#                                   in some other tracked source.
#
# The second kind is the difference between "trust me, it is called" and
# "here is the call". A snooze button would be the former; this one goes
# stale on its own the moment the call site changes, and an exemption expires once the open
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
# Everything from here down in a source file is tests, not callers.
TEST_MODULE_MARKER="#\[cfg(test)\]"
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
declare -A exempt_stage exempt_reason exempt_call
exempt_count=0
if [[ -f "$EXEMPT_FILE" ]]; then
    while read -r name stage reason; do
        [[ -z "${name:-}" || "$name" == \#* ]] && continue
        if [[ "$stage" == "call" ]]; then
            if [[ -z "${reason// /}" ]]; then
                echo "check_domain_fns_are_called: $EXEMPT_FILE: \`$name\` is exempt as called but names no call expression." >&2
                exit 2
            fi
            exempt_call["$name"]="$reason"
            exempt_count=$((exempt_count + 1))
            continue
        fi
        if ! [[ "$stage" =~ ^stage-([0-9]+)$ ]]; then
            echo "check_domain_fns_are_called: $EXEMPT_FILE: \`$name\` has neither a \`stage-N\` deadline nor \`call\` (got '${stage:-}')." >&2
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
    while IFS=$'\t' read -r name owner; do
        [[ -z "$name" ]] && continue

        # One grep per name across every tracked source, rather than one
        # per (name, file) pair: the same answer, a fraction of the forks.
        # A method additionally requires its type to be named in the same
        # file, or every same-named method in the tree vouches for it.
        # Production use inside the DEFINING file counts, and it is a
        # USE only if it outnumbers the declarations. Same-file use
        # is discounted so a unit test cannot vouch for its own subject,
        # but that must not discard a real caller: `FrameError::to_wire`
        # is invoked by `parse_inbound` a few lines below its definition,
        # which is exactly the wiring a P1 finding asked for. So the
        # defining file is re-read with its test module cut off, and a
        # mention beyond the declaration itself is a caller.
        own="$(sed "/$TEST_MODULE_MARKER/q" "$file" 2>/dev/null | sed 's,//.*,,')"
        uses_in_own=$(grep -ow -- "$name" <<<"$own" | wc -l)
        # Subtract EVERY declaration of the name, not one. Counting a
        # single declaration assumed the name is declared once per file,
        # and `as_slice` is declared twice in the Kademlia port — on
        # `OfferedAddresses` and on `ObservedCandidates`. Each then saw
        # two occurrences, counted the other's declaration as its own
        # caller, and passed with no caller anywhere.
        decls_in_own=$(grep -oE "fn $name\b" <<<"$own" | wc -l)
        if (( uses_in_own > decls_in_own )); then
            elsewhere=1
        else
            elsewhere=0
        fi

        while IFS= read -r hit; do
            (( elsewhere == 1 )) && break
            [[ "$hit" == "$file" ]] && continue
            # Comments are stripped first. Prose vouched for a function
            # once already: `Refusal::to_wire` passed only because the
            # conformance matrix's own doc comment happened to name both
            # `to_wire` and `Refusal`, so a paragraph ABOUT the check was
            # what made the check green.
            stripped="$(sed 's,//.*,,' "$hit" 2>/dev/null)"
            if [[ -n "$owner" ]] && ! grep -qw -- "$owner" <<<"$stripped"; then
                continue
            fi
            elsewhere=1
            break
        done < <(grep -lw -- "$name" "${all_rs[@]}" 2>/dev/null)

        qualified="$name"
        [[ -n "$owner" ]] && qualified="$owner::$name"

        if [[ -n "${exempt_call[$qualified]:-}" ]]; then
            seen_exempt["$qualified"]=1
            expr="${exempt_call[$qualified]}"
            found_call=0
            for other in "${all_rs[@]}"; do
                [[ "$other" == "$file" ]] && continue
                if grep -qF -- "$expr" "$other" 2>/dev/null; then found_call=1; break; fi
            done
            if (( found_call == 0 )); then
                echo "check_domain_fns_are_called: $EXEMPT_FILE: \`$qualified\` is exempt as called via \`$expr\`, but no other source contains that call." >&2
                problems=$((problems + 1))
            fi
            continue
        fi

        if [[ -n "${exempt_stage[$qualified]:-}" ]]; then
            seen_exempt["$qualified"]=1
            if (( elsewhere == 1 )); then
                echo "check_domain_fns_are_called: $EXEMPT_FILE: \`$qualified\` is exempt but IS referred to elsewhere — drop the entry." >&2
                problems=$((problems + 1))
            elif (( exempt_stage[$qualified] < open_stage )); then
                echo "check_domain_fns_are_called: $file: \`$qualified\` is exempt until stage ${exempt_stage[$qualified]}, but stage $open_stage is open — the deadline passed." >&2
                problems=$((problems + 1))
            fi
            continue
        fi

        if (( elsewhere == 0 )); then
            echo "check_domain_fns_are_called: $file: \`$qualified\` is referred to nowhere else." >&2
            problems=$((problems + 1))
        fi
    done < <(awk '
        # Top-level `impl` opens an owner; the matching top-level `}`
        # closes it. `impl Trait for Type` owns Type, not Trait.
        /^impl/ {
            line = $0
            sub(/^impl(<[^>]*>)?[[:space:]]*/, "", line)
            if (line ~ / for /) { sub(/.* for /, "", line) }
            sub(/[[:space:]]*[{<].*/, "", line)
            gsub(/[^A-Za-z0-9_]/, "", line)
            owner = line
            next
        }
        /^}/ { owner = "" }
        /^[[:space:]]*pub (const |async )?fn [a-z_]/ {
            n = $0
            sub(/.*fn /, "", n)
            sub(/[^a-z0-9_].*/, "", n)
            print n "\t" owner
        }
    ' "$file" 2>/dev/null | sort -u)
done

# An exemption naming a function that no longer exists stops protecting
# anything and starts hiding the next one that takes its name.
# `${!arr[@]}` on an empty associative array trips `set -u`, so the
# iteration is guarded rather than the array pre-seeded: a sentinel entry
# would be an exemption nobody wrote.
for name in ${exempt_stage[@]+"${!exempt_stage[@]}"} ${exempt_call[@]+"${!exempt_call[@]}"}; do
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
