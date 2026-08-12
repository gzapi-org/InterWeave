#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_license_headers.sh
#
# >>> help
# Prove every first-party source file declares Apache-2.0, and that no
# foreign licence terms rode in with copied material.
#
#   tools/checks/check_license_headers.sh            # scan the tracked tree
#   tools/checks/check_license_headers.sh --list     # print what would be scanned
#
# Two checks, because a licence goes wrong in two directions:
#
#   MISSING  — a first-party source file with no `SPDX-License-Identifier`
#              in its opening lines. The repository LICENSE says Apache-2.0;
#              a file that does not say so itself is ambiguous the moment it
#              is copied back out.
#   FOREIGN  — a tracked file carrying an SPDX tag that is not Apache-2.0,
#              or the boilerplate of a rights-reserved / confidential
#              notice (see PROPRIETARY_RE below). This is the one that
#              matters: code moved in from a differently-licensed source
#              keeps its own terms until the copyright holder relicenses
#              it, and a public Apache-2.0 tree is exactly where that goes
#              unnoticed.
#
# Genuinely third-party material is not a violation — it is an EXEMPTION,
# recorded with its provenance in tools/checks/license_exempt.txt (one
# path per line, `#` comments). Exempting a file silences both checks for
# it, so the entry is where the third-party licence gets named. Vendoring
# without an entry is the failure this script exists to catch.
#
# Header extensions are the ones a header can live in without breaking the
# format. Markdown, YAML, TOML and JSON are scanned for FOREIGN terms but
# never required to carry a header: the LICENSE file is canonical for
# prose, and a licence banner on every ADR would be noise.
#
# Options:
#   --list            print the files that would be header-checked, then exit
#   --root <dir>      scan this directory instead of the repository root
#   -h, --help        this text
#
# Exit codes:
#   0  clean
#   1  violations found (each printed with file and reason)
#   2  invocation problem, or the tree could not be read
# <<< help

set -uo pipefail

readonly EXPECTED_SPDX="Apache-2.0"
readonly HEADER_LINES=10
readonly HEADER_EXTENSIONS="sh|bash|rs|kt|kts|java|py|ts|tsx|js|mjs|c|h|cpp|hpp|swift"

# Written with a bracket expression between the words on purpose: it
# matches the proprietary phrases in a scanned file, but this file does
# not then contain the literal phrase and flag itself. A checker that
# cannot survive its own scan teaches everyone to exempt things.
readonly PROPRIETARY_RE='all[[:space:]]+rights[[:space:]]+reserved|proprietary[[:space:]]+and[[:space:]]+confidential'

ROOT=""
LIST_ONLY=0

die() { printf '%s\n' "$*" >&2; exit 2; }

show_help() {
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed -e '1d' -e '$d' -e 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) show_help; exit 0 ;;
        --list)    LIST_ONLY=1; shift ;;
        --root)    [ $# -ge 2 ] || die "--root needs a value"; ROOT="$2"; shift 2 ;;
        -*)        die "unknown option: $1" ;;
        *)         die "unexpected argument: $1" ;;
    esac
done

if [ -z "$ROOT" ]; then
    ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" \
        || die "not inside a git repository; pass --root"
fi
[ -d "$ROOT" ] || die "not a directory: $ROOT"
cd "$ROOT" || die "cannot enter: $ROOT"

# Tracked files PLUS untracked-but-not-ignored ones. A file that has not
# been `git add`ed yet is precisely the file about to be committed, so a
# tracked-only scan would pass every import on the run that mattered and
# only object afterwards. Ignored paths stay out: build output and local
# state are not the repository's licence problem.
mapfile -t TRACKED < <(git ls-files --cached --others --exclude-standard 2>/dev/null) \
    || die "git ls-files failed in $ROOT"
[ "${#TRACKED[@]}" -gt 0 ] || die "no tracked files in $ROOT"

EXEMPT_FILE="tools/checks/license_exempt.txt"
declare -A EXEMPT=()
if [ -f "$EXEMPT_FILE" ]; then
    while IFS= read -r line; do
        line="${line%%#*}"
        line="$(printf '%s' "$line" | tr -d '[:space:]')"
        [ -n "$line" ] && EXEMPT["$line"]=1
    done < "$EXEMPT_FILE"
fi

needs_header() {
    case "$1" in
        *.*) [[ "${1##*.}" =~ ^($HEADER_EXTENSIONS)$ ]] ;;
        *)   return 1 ;;
    esac
}

if [ "$LIST_ONLY" -eq 1 ]; then
    for f in "${TRACKED[@]}"; do
        [ -n "${EXEMPT[$f]:-}" ] && continue
        needs_header "$f" && printf '%s\n' "$f"
    done
    exit 0
fi

violations=0
report() { printf '%s: %s\n' "$1" "$2"; violations=$((violations + 1)); }

for f in "${TRACKED[@]}"; do
    [ -n "${EXEMPT[$f]:-}" ] && continue
    [ -f "$f" ] || continue

    # The LICENSE file is the one place the full Apache text belongs, and
    # its appendix contains the boilerplate this scan would otherwise read
    # as a foreign notice.
    [ "$f" = "LICENSE" ] && continue

    # FOREIGN — an explicit SPDX tag naming something else, or proprietary
    # wording anywhere in the file.
    # Match the licence TOKEN only, not the rest of the line: prose that
    # quotes the tag mid-sentence (this repository's own documentation
    # does) would otherwise read as an expression naming everything that
    # followed it.
    foreign_spdx="$(grep -m1 -oE 'SPDX-License-Identifier:[[:space:]]*[A-Za-z0-9.+_-]+' "$f" 2>/dev/null \
        | sed 's/SPDX-License-Identifier:[[:space:]]*//' | tr -d '[:space:]')"
    if [ -n "$foreign_spdx" ] && [ "$foreign_spdx" != "$EXPECTED_SPDX" ]; then
        report "$f" "declares SPDX '$foreign_spdx', expected '$EXPECTED_SPDX' (exempt it with provenance if genuinely third-party)"
    fi
    if grep -q -i -E "$PROPRIETARY_RE" "$f" 2>/dev/null; then
        report "$f" "carries proprietary licence wording — it is not Apache-2.0 until the copyright holder relicenses it"
    fi

    # MISSING — only for file types that can carry a header.
    if needs_header "$f"; then
        if ! head -n "$HEADER_LINES" "$f" 2>/dev/null | grep -q "SPDX-License-Identifier:[[:space:]]*$EXPECTED_SPDX"; then
            report "$f" "no 'SPDX-License-Identifier: $EXPECTED_SPDX' in the first $HEADER_LINES lines"
        fi
    fi
done

if [ "$violations" -gt 0 ]; then
    printf 'check_license_headers: %d violation(s).\n' "$violations" >&2
    exit 1
fi

printf 'check_license_headers: OK — every first-party source file declares %s.\n' "$EXPECTED_SPDX"
exit 0
