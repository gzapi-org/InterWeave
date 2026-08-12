#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_validate_adr_index.sh
#
# Self-test for validate_adr_index.sh. Each case builds a miniature
# architecture/adr/ tree under $TMPDIR and points the validator at it
# with --root, so no assertion depends on the real corpus.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/validate_adr_index.sh"

pass=0
fail=0
ok()  { printf '  ✓ %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf '  ✗ %s\n' "$1" >&2; fail=$((fail + 1)); }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Output is captured, never piped into `grep -q`: with pipefail set, a
# `grep -q` that exits early kills the producer with EPIPE and the
# pipeline reports failure on output that was correct.
run()      { bash "$CHECK" --root "$1" 2>&1; }
run_code() { bash "$CHECK" --root "$1" >/dev/null 2>&1; printf '%s' "$?"; }

# write_adr <root> <number> <slug> [extra-body]
write_adr() {
    local root="$1" num="$2" slug="$3" extra="${4:-}"
    {
        printf '# A decision about %s\n\n**Status:** Accepted\n\n' "$slug"
        for h in Context Decision "Alternatives considered" Consequences \
                 "Security implications" "Operational implications" \
                 "Implementation implications" "Revisit conditions"; do
            printf '## %s\n\nText.\n\n' "$h"
        done
        [ -n "$extra" ] && printf '%s\n' "$extra"
    } > "$root/architecture/adr/$num-$slug.md"
}

# make_tree <root> — two conformant ADRs, indexed and digested.
make_tree() {
    local root="$1"
    mkdir -p "$root/architecture/adr"
    write_adr "$root" 0001 first
    write_adr "$root" 0002 second
    cat > "$root/architecture/adr/README.md" <<'EOF'
# ADR index

| ADR | Decision |
|---|---|
| [0001](./0001-first.md) | First. |
| [0002](./0002-second.md) | Second. |
EOF
    cat > "$root/architecture/adr/ADR-DIGEST.md" <<'EOF'
# ADR digest

## Keyword → ADR lookup

| You are working on… | Read |
|---|---|
| the first thing | **0001** |

---

### 0001 — A decision about first (Accepted)
- Keywords: first

### 0002 — A decision about second (Accepted)
- Keywords: second
EOF
}

printf 'test_validate_adr_index\n'

# ── a conformant tree passes ─────────────────────────────────────────────
R="$TMP/clean"; make_tree "$R"
[ "$(run_code "$R")" = "0" ] && ok "conformant tree exits 0" || bad "conformant tree should pass: $(run "$R")"

# ── an ADR missing from the README ───────────────────────────────────────
R="$TMP/noindex"; make_tree "$R"; write_adr "$R" 0003 third
printf '\n### 0003 — A decision about third (Accepted)\n' >> "$R/architecture/adr/ADR-DIGEST.md"
out="$(run "$R")"
[[ "$out" == *"no row in README.md"* ]] && ok "an ADR missing from the index is reported" || bad "should report the missing index row"

# ── an ADR missing from the digest ───────────────────────────────────────
R="$TMP/nodigest"; make_tree "$R"; write_adr "$R" 0003 third
printf '| [0003](./0003-third.md) | Third. |\n' >> "$R/architecture/adr/README.md"
out="$(run "$R")"
[[ "$out" == *"no '### 0003 — ' entry"* ]] && ok "an ADR missing from the digest is reported" || bad "should report the missing digest entry"

# ── the reverse direction: index and digest naming absent ADRs ───────────
R="$TMP/ghost"; make_tree "$R"
printf '| [0009](./0009-ghost.md) | Ghost. |\n' >> "$R/architecture/adr/README.md"
printf '\n### 0008 — Ghost entry (Accepted)\n' >> "$R/architecture/adr/ADR-DIGEST.md"
out="$(run "$R")"
[[ "$out" == *"README.md links ./0009-ghost.md"* ]] && ok "an index row for a missing file is reported" || bad "should report the dangling index link"
[[ "$out" == *"entry for 0008"* ]] && ok "a digest entry for a missing file is reported" || bad "should report the dangling digest entry"

# ── a keyword-table target that does not exist ───────────────────────────
R="$TMP/badkw"; make_tree "$R"
sed -i 's/| the first thing | \*\*0001\*\* |/| the first thing | **0001** |\n| a missing thing | **0077** |/' "$R/architecture/adr/ADR-DIGEST.md"
out="$(run "$R")"
[[ "$out" == *"keyword table points at **0077**"* ]] && ok "a dangling keyword-table target is reported" || bad "should report the dangling keyword target"

# ── template: missing mandatory section ──────────────────────────────────
R="$TMP/nosec"; make_tree "$R"
sed -i '/^## Security implications$/,+2d' "$R/architecture/adr/0001-first.md"
out="$(run "$R")"
[[ "$out" == *"missing mandatory section '## Security implications'"* ]] && ok "a missing mandatory section is reported" || bad "should report the missing section"

# ── template: sections out of order ──────────────────────────────────────
R="$TMP/order"; make_tree "$R"
python3 - "$R/architecture/adr/0001-first.md" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read()
s=s.replace("## Context\n\nText.\n\n","",1)
s=s.replace("## Consequences\n\nText.\n\n","## Consequences\n\nText.\n\n## Context\n\nText.\n\n",1)
open(p,'w').write(s)
PY
out="$(run "$R")"
[[ "$out" == *"out of template order"* ]] && ok "sections out of template order are reported" || bad "should report the ordering violation"

# ── template: title re-stating its own number ────────────────────────────
R="$TMP/title"; make_tree "$R"
sed -i '1s/.*/# ADR-0001 — A decision about first/' "$R/architecture/adr/0001-first.md"
out="$(run "$R")"
[[ "$out" == *"re-states its own number"* ]] && ok "a title embedding the ADR number is reported" || bad "should report the title style"

# ── template: no Status line ─────────────────────────────────────────────
R="$TMP/nostatus"; make_tree "$R"
sed -i '/^\*\*Status:\*\*/d' "$R/architecture/adr/0002-second.md"
out="$(run "$R")"
[[ "$out" == *"no '**Status:**' line"* ]] && ok "a missing Status line is reported" || bad "should report the missing status"

# ── amendment: table without a history file ──────────────────────────────
R="$TMP/notable"; make_tree "$R"
cat >> "$R/architecture/adr/0001-first.md" <<'EOF'
## Amendments

| Date | Amendment | Effect |
|---|---|---|
| 2026-09-01 | Something changed | Effect |
EOF
out="$(run "$R")"
[[ "$out" == *"no history/0001-amendments.md"* ]] && ok "an amendment table without its history file is reported" || bad "should report the missing history file"

# ── amendment: history file without a table ──────────────────────────────
R="$TMP/nohist"; make_tree "$R"
mkdir -p "$R/architecture/adr/history"
printf '### Amendment 2026-09-01 — Something changed\n' > "$R/architecture/adr/history/0002-amendments.md"
out="$(run "$R")"
[[ "$out" == *"has no '## Amendments' table"* ]] && ok "a history file without its table is reported" || bad "should report the orphan history file"

# ── amendment: matching pair passes ──────────────────────────────────────
R="$TMP/pair"; make_tree "$R"
mkdir -p "$R/architecture/adr/history"
cat >> "$R/architecture/adr/0001-first.md" <<'EOF'
## Amendments

| Date | Amendment | Effect |
|---|---|---|
| 2026-09-01 | Something changed | Effect |
EOF
printf '# history\n\n### Amendment 2026-09-01 — Something changed\n\nWhy.\n' \
    > "$R/architecture/adr/history/0001-amendments.md"
[ "$(run_code "$R")" = "0" ] && ok "a matching table/history pair passes" || bad "matching pair should pass: $(run "$R")"

# ── amendment: title drift between table and note ────────────────────────
R="$TMP/drift"; make_tree "$R"
mkdir -p "$R/architecture/adr/history"
cat >> "$R/architecture/adr/0001-first.md" <<'EOF'
## Amendments

| Date | Amendment | Effect |
|---|---|---|
| 2026-09-01 | Something changed | Effect |
EOF
printf '# history\n\n### Amendment 2026-09-01 — Something ELSE changed\n\nWhy.\n' \
    > "$R/architecture/adr/history/0001-amendments.md"
out="$(run "$R")"
[[ "$out" == *"keys differ between the table and history"* ]] && ok "a (date, title) mismatch is reported" || bad "should report the key mismatch"

# ── amendment: a second note with no row ─────────────────────────────────
R="$TMP/extra"; make_tree "$R"
mkdir -p "$R/architecture/adr/history"
cat >> "$R/architecture/adr/0001-first.md" <<'EOF'
## Amendments

| Date | Amendment | Effect |
|---|---|---|
| 2026-09-01 | Something changed | Effect |
EOF
printf '# history\n\n### Amendment 2026-09-01 — Something changed\n\nWhy.\n\n### Amendment 2026-09-02 — Unlogged\n\nWhy.\n' \
    > "$R/architecture/adr/history/0001-amendments.md"
out="$(run "$R")"
[[ "$out" == *"keys differ"* ]] && ok "a history note with no table row is reported" || bad "should report the unlogged note"

# ── the pre-0048 trailing amendment section is rejected ──────────────────
R="$TMP/stray"; make_tree "$R"
printf '\n## Android amendment\n\nOld convention.\n' >> "$R/architecture/adr/0001-first.md"
out="$(run "$R")"
[[ "$out" == *"## Android amendment"* ]] && ok "a trailing pre-0048 amendment section is rejected" || bad "should reject the old convention"

# ── `## Amendments` must be last ─────────────────────────────────────────
R="$TMP/notlast"; make_tree "$R"
mkdir -p "$R/architecture/adr/history"
cat >> "$R/architecture/adr/0001-first.md" <<'EOF'
## Amendments

| Date | Amendment | Effect |
|---|---|---|
| 2026-09-01 | Something changed | Effect |

## Trailing extra

Text.
EOF
printf '# history\n\n### Amendment 2026-09-01 — Something changed\n\nWhy.\n' \
    > "$R/architecture/adr/history/0001-amendments.md"
out="$(run "$R")"
[[ "$out" == *"is not the last section"* ]] && ok "a section after '## Amendments' is reported" || bad "should require Amendments to be last"

# ── the real corpus is clean ─────────────────────────────────────────────
REAL="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null)"
if [ -n "$REAL" ]; then
    [ "$(run_code "$REAL")" = "0" ] && ok "the real ADR corpus validates" || bad "real corpus fails: $(run "$REAL")"
fi

# ── usage ────────────────────────────────────────────────────────────────
[ "$(run_code "$TMP/nothing-here")" = "2" ] && ok "a missing tree exits 2" || bad "missing tree should exit 2"
help_out="$(bash "$CHECK" --help 2>/dev/null)"
[[ "$help_out" == *"TEMPLATE"* ]] && ok "--help prints the help block" || bad "--help should print help"

printf '\n'
if [ "$fail" -gt 0 ]; then
    printf 'test_validate_adr_index: %d passed, %d FAILED.\n' "$pass" "$fail" >&2
    exit 1
fi
printf 'test_validate_adr_index: OK — all %d assertions passed.\n' "$pass"
