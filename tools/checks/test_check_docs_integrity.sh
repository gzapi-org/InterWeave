#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_docs_integrity.sh
#
# Self-test for check_docs_integrity.py. Every case builds a throwaway
# documentation tree under $TMPDIR, so no assertion depends on the state
# of the real one — except the last, which deliberately checks it.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/check_docs_integrity.py"

pass=0
fail=0
ok()  { printf '  ✓ %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf '  ✗ %s\n' "$1" >&2; fail=$((fail + 1)); }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Captured rather than piped into `grep -q`: pipefail plus an
# early-exiting grep turns correct output into a failed pipeline.
run()      { python3 "$CHECK" --root "$1" 2>&1; }
run_code() { python3 "$CHECK" --root "$1" >/dev/null 2>&1; printf '%s' "$?"; }

fresh() { local d="$TMP/$1"; rm -rf "$d"; mkdir -p "$d"; printf '%s' "$d"; }

printf 'test_check_docs_integrity\n'

# ── a tree whose links resolve passes ────────────────────────────────────
R="$(fresh clean)"
mkdir -p "$R/sub"
cat > "$R/index.md" <<'EOF'
# Index

See [the other page](sub/other.md) and [its section](sub/other.md#deep-section).
Also [an external site](https://example.invalid/page) and [a local jump](#index).

[ref]: sub/other.md
EOF
cat > "$R/sub/other.md" <<'EOF'
# Other

## Deep section

Back to [the index](../index.md).
EOF
[ "$(run_code "$R")" = "0" ] && ok "a tree whose links resolve passes" || bad "should pass: $(run "$R")"

# ── a missing target is reported ─────────────────────────────────────────
R="$(fresh missing)"
printf '# T\n\n[gone](nowhere/at-all.md)\n' > "$R/t.md"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a missing link target exits 1" || bad "missing target should exit 1"
[[ "$out" == *"nowhere/at-all.md"* ]] && ok "  and names the target" || bad "should name the target: $out"

# ── a renamed section leaves a link that still LOOKS fine ────────────────
# This is the case the anchor check exists for: the file resolves, the
# browser lands at the top of the page, and nothing looks broken.
R="$(fresh anchor)"
printf '# T\n\n[deep](other.md#the-old-name)\n' > "$R/t.md"
printf '# Other\n\n## The new name\n' > "$R/other.md"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "an anchor naming no heading exits 1" || bad "stale anchor should exit 1"
[[ "$out" == *"the-old-name"* ]] && ok "  and quotes the fragment" || bad "should quote the fragment: $out"

# ── a same-document fragment is checked too ──────────────────────────────
R="$(fresh selffrag)"
printf '# Title\n\n[up](#title) and [nope](#not-here)\n' > "$R/t.md"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a same-document fragment is checked" || bad "self fragment should be checked"
[[ "$out" == *"not-here"* && "$out" != *"'#title'"* ]] && ok "  and only the wrong one is reported" \
    || bad "should report only the bad fragment: $out"

# ── punctuation in a heading still resolves ──────────────────────────────
# GitHub drops everything outside word characters, hyphen and space, so a
# heading full of backticks and colons is still reachable.
R="$(fresh slug)"
printf '# T\n\n[x](other.md#the-xtask-runner-why)\n' > "$R/t.md"
printf '# Other\n\n## The `xtask` runner: why?\n' > "$R/other.md"
[ "$(run_code "$R")" = "0" ] && ok "punctuation in a heading slugs the GitHub way" || bad "slug mismatch: $(run "$R")"

# ── a heading inside a code fence is not an anchor ───────────────────────
# Regression: stripping fences before collecting headings is what stops a
# shell comment from satisfying a broken link.
R="$(fresh fencedheading)"
printf '# T\n\n[x](other.md#not-a-heading)\n' > "$R/t.md"
cat > "$R/other.md" <<'EOF'
# Other

```bash
# not a heading
echo hi
```
EOF
[ "$(run_code "$R")" = "1" ] && ok "a heading inside a code fence is not an anchor" || bad "fenced heading should not resolve"

# ── a link inside a code fence is an illustration, not navigation ────────
R="$(fresh fencedlink)"
cat > "$R/t.md" <<'EOF'
# T

```markdown
[an example](does/not/exist.md)
```
EOF
[ "$(run_code "$R")" = "0" ] && ok "a link inside a code fence is ignored" || bad "fenced link should be ignored: $(run "$R")"

# ── a percent-encoded path resolves ──────────────────────────────────────
R="$(fresh encoded)"
printf '# T\n\n[space](a%%20file.md)\n' > "$R/t.md"
printf '# A file\n' > "$R/a file.md"
[ "$(run_code "$R")" = "0" ] && ok "a percent-encoded path resolves" || bad "encoded path should resolve: $(run "$R")"

# ── a link to a directory resolves ───────────────────────────────────────
R="$(fresh dirlink)"
mkdir -p "$R/sub"
printf '# T\n\n[dir](sub/)\n' > "$R/t.md"
printf '# S\n' > "$R/sub/s.md"
[ "$(run_code "$R")" = "0" ] && ok "a link to a directory resolves" || bad "directory link should resolve: $(run "$R")"

# ── an external link is never fetched ────────────────────────────────────
# A guard that touches the network fails for reasons unrelated to the
# commit under test. The host here does not exist.
R="$(fresh external)"
printf '# T\n\n[x](https://nonexistent.invalid/a) [y](mailto:someone@example.invalid)\n' > "$R/t.md"
[ "$(run_code "$R")" = "0" ] && ok "external links are not fetched" || bad "external link should be skipped: $(run "$R")"

# ── a YAML file that stopped parsing is caught ───────────────────────────
R="$(fresh badyaml)"
printf '# T\n' > "$R/t.md"
printf 'a: 1\n  b: 2\n' > "$R/broken.yaml"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a YAML file that does not parse exits 1" || bad "bad YAML should exit 1"
[[ "$out" == *"broken.yaml"* ]] && ok "  and names the file" || bad "should name the file: $out"

# ── a yaml block inside Markdown is parsed ───────────────────────────────
# The configuration examples are quoted into prose as often as they are
# stored as files, and a reader copies whichever one they found.
R="$(fresh badfence)"
cat > "$R/t.md" <<'EOF'
# T

```yaml
transport:
   backend: libp2p
  limits: {}
```
EOF
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a broken yaml block in Markdown exits 1" || bad "bad yaml block should exit 1"
[[ "$out" == *"yaml block 1"* ]] && ok "  and says which block" || bad "should identify the block: $out"

# ── a valid yaml block passes ────────────────────────────────────────────
R="$(fresh goodfence)"
cat > "$R/t.md" <<'EOF'
# T

```yaml
transport:
  backend: libp2p
```

```text
this: is not yaml: at all: [
```
EOF
[ "$(run_code "$R")" = "0" ] && ok "a valid yaml block passes and other fences are left alone" \
    || bad "should pass: $(run "$R")"

# ── invocation ───────────────────────────────────────────────────────────
python3 "$CHECK" --help >/dev/null 2>&1 && ok "--help exits 0" || bad "--help should exit 0"
python3 "$CHECK" --nope >/dev/null 2>&1; [ "$?" = "2" ] && ok "an unknown option exits 2" || bad "unknown option should exit 2"
python3 "$CHECK" --root "$TMP/does-not-exist" >/dev/null 2>&1; [ "$?" = "2" ] \
    && ok "a missing root exits 2" || bad "missing root should exit 2"

# ── and the real tree ────────────────────────────────────────────────────
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
[ "$(run_code "$REPO_ROOT")" = "0" ] && ok "the repository's own documentation is intact" \
    || bad "the real tree fails: $(run "$REPO_ROOT")"

printf '\ntest_check_docs_integrity: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
