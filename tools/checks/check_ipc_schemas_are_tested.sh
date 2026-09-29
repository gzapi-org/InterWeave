#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# >>> help
# check_ipc_schemas_are_tested.sh — every IPC schema is named by a test
# somewhere other than an inventory of the schema directory
#
#   tools/checks/check_ipc_schemas_are_tested.sh
#   tools/checks/check_ipc_schemas_are_tested.sh --root <dir>
#
# Plan §16 (Stage 13), devex-tooling's D3: the schema-agreement coverage
# check. Every `architecture/contracts/schemas/ipc/**/*.schema.json`
# must be NAMED by a test: its path, relative to `schemas/`, ends a
# double-quoted Rust string literal — `"ipc/hello.schema.json"`, or the
# same after any prefix, `"architecture/contracts/schemas/ipc/…"`. A
# schema no test names is one whose agreement with the Rust mirror
# nothing checks: the types can drift from it with every test green.
#
# WHAT COUNTS AS A TEST: any `.rs` file under a `tests/` directory in
# `crates/` or `tests/`, and a `src/` file from a `#[cfg(test)]` that
# opens a `mod` (on its line or the next item, other attributes between)
# to the end of the file. A `#[cfg(test)]` on anything else — a hook
# function, a `thread_local!` — starts nothing, since production code
# follows it. Code after the tests module would count; none is written
# that way here.
#
# ONLY A STRING LITERAL IN CODE COUNTS. Each file is lexed from its
# first line, not grepped: a `//` comment (whole-line or trailing) and a
# `/* */` block (nested, across lines) are skipped wherever they sit, the
# `#[cfg(test)]` gate and the inventory are recognised only in code, and
# a raw string (`r"…"`, `r#"…"#`) ends where Rust ends it. So a comment
# quoting the path — or a commented-out test module — is not a name.
# What is left is something the code opens, validates or compares
# against.
#
# AN INVENTORY DOES NOT COUNT. `schema_agreement.rs` lists the schema
# directory in `const IPC_SCHEMAS` and asserts the list equals the
# directory, so a schema added to the tree is added to the list, and
# counted there it would be "named" the moment it existed — the check
# would pass by construction. Lines inside a `const` or `static` whose
# name ends in `SCHEMAS`, from its declaration to the closing `];`, are
# skipped. What is left is a site that reads the schema for a reason:
# a validator, an enum compared with a Rust type, a bound read back.
#
# The check does not judge what the test does with the schema; the
# review does. It asks the one question that has a mechanical answer.
#
# Exit codes:
#   0  every IPC schema is named by a test outside an inventory
#   1  at least one is not, or there is nothing to check (no schema, or
#      no test file: a guard with no input passes by never running)
#   2  invocation error
# <<< help

set -uo pipefail

usage() { sed -n '/^# >>> help$/,/^# <<< help$/{/^# [<>]\{3\} help$/d;s/^# \{0,1\}//;p}' "$0"; }

ROOT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --root)
            # `shift 2` with one argument left fails, and with no `set -e`
            # the loop then spins on an unchanged $1 forever.
            [ $# -ge 2 ] || { echo "check_ipc_schemas_are_tested: --root needs a directory" >&2; exit 2; }
            ROOT="$2"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) echo "check_ipc_schemas_are_tested: unknown option '$1'" >&2; exit 2 ;;
    esac
done

if [ -z "$ROOT" ]; then
    ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
fi
[ -n "$ROOT" ] || { echo "check_ipc_schemas_are_tested: no --root and not in a git repo" >&2; exit 2; }
[ -d "$ROOT" ] || { echo "check_ipc_schemas_are_tested: not a directory: $ROOT" >&2; exit 2; }

SCHEMAS="$ROOT/architecture/contracts/schemas"

schemas="$(cd "$SCHEMAS" 2>/dev/null && find ipc -name '*.schema.json' -type f 2>/dev/null | LC_ALL=C sort)"
if [ -z "$schemas" ]; then
    echo "check_ipc_schemas_are_tested: no ipc/*.schema.json under $SCHEMAS — nothing to check, which is a failure, not a pass" >&2
    exit 1
fi

# Test sources. `target/` is a build tree; `spikes/` and `third_party/`
# are not this repository's tests and must not vouch for its schemas.
test_files="$(
    cd "$ROOT" && {
        find crates tests -type f -name '*.rs' -path '*/tests/*' \
            -not -path '*/target/*' 2>/dev/null
        find crates -type f -name '*.rs' -path '*/src/*' \
            -not -path '*/target/*' 2>/dev/null
    } | LC_ALL=C sort -u
)"
if [ -z "$test_files" ]; then
    echo "check_ipc_schemas_are_tested: no Rust test source under crates/ or tests/ — nothing to check" >&2
    exit 1
fi

# The lexer. It runs over every line of a file so its comment and string
# state is right wherever counting starts; `emit` decides what it prints.
# Every string literal at a counting site is printed on one line with its
# delimiters, its content verbatim (escapes kept, a line break a space).
LEXER="$(cat <<'AWK'
function code_start() { return !depth && !in_str }
{
    line = $0
    counting = !in_src || tested
    if (in_src && !tested && code_start()) {
        # The src/ gate: a #[cfg(test)] arms it, and the next item
        # decides. Blank, attribute and comment lines keep it armed; a
        # mod opens counting; anything else disarms.
        if (line ~ /^[[:space:]]*#\[cfg\(test\)\]/) {
            armed = 1
            sub(/^[[:space:]]*#\[cfg\(test\)\][[:space:]]*/, "", line)
        }
        if (armed && line ~ /^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z0-9_]+[[:space:]]*\{/) {
            tested = 1; counting = 1; armed = 0
        } else if (armed && line !~ /^[[:space:]]*($|#\[|\/\/)/) {
            armed = 0
        }
        line = $0
    }
    if (counting && inventory) {
        if (line ~ /^[[:space:]]*\];/) inventory = 0
        next
    }
    if (counting && code_start() && line ~ /^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?(const|static)[[:space:]]+[A-Z0-9_]*SCHEMAS[[:space:]]*:/) {
        if (line !~ /\];/) inventory = 1
        next
    }
    n = length(line); i = 1
    while (i <= n) {
        c = substr(line, i, 1); c2 = substr(line, i, 2)
        if (depth) {
            if (c2 == "*/") { depth--; i += 2 }
            else if (c2 == "/*") { depth++; i += 2 }
            else i++
        } else if (in_str) {
            if (raw) {
                close_at = "\"" substr("################################", 1, hashes)
                if (substr(line, i, length(close_at)) == close_at) {
                    if (counting) print "\"" buf "\""
                    in_str = 0; raw = 0; i += length(close_at)
                } else { buf = buf c; i++ }
            } else if (c == "\\") { buf = buf c2; i += 2 }
            else if (c == "\"") { if (counting) print "\"" buf "\""; in_str = 0; i++ }
            else { buf = buf c; i++ }
        } else if (c2 == "//") {
            break
        } else if (c2 == "/*") {
            depth = 1; i += 2
        } else if (c == "\"") {
            in_str = 1; raw = 0; buf = ""; i++
        } else if (c == "r" && (i == 1 || substr(line, i - 1, 1) !~ /[A-Za-z0-9_]/) && match(substr(line, i + 1), /^#*"/)) {
            in_str = 1; raw = 1; hashes = RLENGTH - 1; buf = ""; i += RLENGTH + 1
        } else if (substr(line, i, 3) == "\047\"\047") {
            i += 3
        } else if (substr(line, i, 4) == "\047\\\"\047") {
            i += 4
        } else i++
    }
    if (in_str) buf = buf " "
}
AWK
)"

literals="$(
    cd "$ROOT" && while IFS= read -r f; do
        case "$f" in */tests/*) in_src=0 ;; *) in_src=1 ;; esac
        awk -v in_src="$in_src" "$LEXER" "$f"
    done <<<"$test_files"
)"

missing=0
count=0
while IFS= read -r schema; do
    count=$((count + 1))
    # The literal is the schema's path, whole, or ends in "/" + that path.
    # Fixed strings: the only unescaped quotes on a literal line are its
    # two delimiters, and a path holds no backslash, so a match on
    # `/<path>"` can only be the literal's end.
    if ! grep -qxF "\"$schema\"" <<<"$literals" \
        && ! grep -qF "/$schema\"" <<<"$literals"; then
        echo "architecture/contracts/schemas/$schema: no test names it outside a *SCHEMAS inventory"
        missing=$((missing + 1))
    fi
done <<<"$schemas"

if [ "$missing" -gt 0 ]; then
    printf '\ncheck_ipc_schemas_are_tested: %d of %d IPC schema(s) named by no test.\n' "$missing" "$count" >&2
    echo "Name each one in a test that reads it (tests/*.rs, or a src/ #[cfg(test)] module), as a string literal." >&2
    exit 1
fi

printf 'check_ipc_schemas_are_tested: OK — all %d IPC schema(s) are named by a test.\n' "$count"
