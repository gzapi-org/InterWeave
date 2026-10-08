#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/ci/test_build_previous_builds.sh
#
# Self-test for build_previous_builds.sh.
#
# git and cargo are stubs on PATH (rustup is absent, so the toolchain
# step is skipped), each logging its arguments and told by a file in the
# sandbox how to misbehave: git's worktree add makes a tree with the two
# apps, cargo leaves target/release/<bin> where CARGO_TARGET_DIR says.
# Every case runs the script against a list of its own. The real build
# of the first entry was measured on the author's host; this file pins
# the logic: what is built, what is skipped, what is refused before
# anything is built, and that a failure leaves no entry and no scratch.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/build_previous_builds.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
SANDBOX="$(realpath -- "$(mktemp -d)")"
trap 'rm -rf "$SANDBOX"' EXIT
BIN="$SANDBOX/bin"
mkdir -p "$BIN"

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

SHA_A=45ba39283a89acfabb728ae4533be0589264353b
SHA_B=0123456789abcdef0123456789abcdef01234567

# git: `cat-file -e` succeeds only for a sha in known-commits (fetch
# adds it there, unless fetch-fails); `worktree add` makes the tree.
cat > "$BIN/git" <<EOF
#!/usr/bin/env bash
echo "git \$*" >> "$SANDBOX/log"
[[ "\$1" == "-C" ]] && shift 2
case "\$1" in
  cat-file) sha="\${3%^{commit\}}"; grep -qx "\$sha" "$SANDBOX/known-commits" 2>/dev/null ;;
  fetch) [[ -e "$SANDBOX/fetch-fails" ]] && exit 128
         echo "\${@: -1}" >> "$SANDBOX/known-commits" ;;
  worktree)
    case "\$2" in
      add) [[ -e "$SANDBOX/worktree-fails" ]] && exit 128
           path="\${@: -2:1}"; mkdir -p "\$path/apps/transport-daemon" "\$path/apps/transportctl"
           echo "\${@: -1}" > "\$path/HEAD-SHA" ;;
      remove) rm -rf "\${@: -1}" ;;
      prune) ;;
    esac ;;
esac
EOF
# cargo: records the tree it ran in (by its HEAD-SHA) and its target dir;
# cargo-fails-<bin> fails that build, cargo-no-output-<bin> succeeds
# without the binary.
cat > "$BIN/cargo" <<EOF
#!/usr/bin/env bash
bin=""; prev=""
for a in "\$@"; do [[ "\$prev" == "--bin" ]] && bin="\$a"; prev="\$a"; done
echo "cargo \$* sha=\$(cat HEAD-SHA 2>/dev/null) target=\${CARGO_TARGET_DIR-unset}" >> "$SANDBOX/log"
[[ -e "$SANDBOX/cargo-fails-\$bin" ]] && exit 101
[[ -e "$SANDBOX/cargo-no-output-\$bin" ]] && exit 0
mkdir -p "\$CARGO_TARGET_DIR/release"
printf '#!/bin/sh\necho %s %s\n' "\$bin" "\$(cat HEAD-SHA)" > "\$CARGO_TARGET_DIR/release/\$bin"
chmod +x "\$CARGO_TARGET_DIR/release/\$bin"
EOF
chmod +x "$BIN/git" "$BIN/cargo"
# PATH without rustup: the stubs, then the system's own tools, minus any
# directory that holds a real rustup.
SYS_PATH=""
IFS=: read -ra dirs <<<"$PATH"
for d in "${dirs[@]}"; do
    [[ -x "$d/rustup" ]] && continue
    SYS_PATH="${SYS_PATH:+$SYS_PATH:}$d"
done

reset() {
    rm -rf "$SANDBOX/out" "$SANDBOX/tmp" "$SANDBOX"/log "$SANDBOX"/known-commits \
           "$SANDBOX"/*-fails* "$SANDBOX"/cargo-no-output-*
    mkdir -p "$SANDBOX/tmp"
}
# run <list text> [args…]: the script against that list, into out/.
run() {
    printf '%s\n' "$1" > "$SANDBOX/list"; shift
    out="$(PATH="$BIN:$SYS_PATH" TMPDIR="$SANDBOX/tmp" RUNNER_TEMP="" \
          bash "$UNDER_TEST" --list "$SANDBOX/list" "$@" 2>&1)"
    rc=$?
}
scratch_empty() { [[ -z "$(ls -A "$SANDBOX/tmp")" ]]; }

echo "build_previous_builds.sh"

# --- builds an entry, from its own commit, both apps ------------------
reset
run "# a comment

stage13-45ba3928 $SHA_A   # trailing" "$SANDBOX/out"
if [[ $rc -eq 0 && -x "$SANDBOX/out/stage13-45ba3928/transport-daemon" && -x "$SANDBOX/out/stage13-45ba3928/transportctl" ]] \
   && [[ "$("$SANDBOX/out/stage13-45ba3928/transport-daemon")" == "transport-daemon $SHA_A" ]] \
   && [[ "$("$SANDBOX/out/stage13-45ba3928/transportctl")" == "transportctl $SHA_A" ]]; then
    pass "an entry gets both binaries, built from its own commit"
else fail "an entry gets both binaries, built from its own commit" "rc=$rc $out"; fi
if [[ "$(ls "$SANDBOX/out")" == "stage13-45ba3928" ]]; then pass "only the label's directory is left in <dir>"
else fail "only the label's directory is left in <dir>" "$(ls -A "$SANDBOX/out")"; fi
n="$(grep -c -- '^cargo build --release --locked --manifest-path apps/[a-z-]*/Cargo.toml --bin ' "$SANDBOX/log")"
if [[ "$n" -eq 2 ]] && grep -q -- '--manifest-path apps/transport-daemon/Cargo.toml --bin transport-daemon' "$SANDBOX/log" \
   && grep -q -- '--manifest-path apps/transportctl/Cargo.toml --bin transportctl' "$SANDBOX/log"; then
    pass "each app is built release, --locked, by its manifest path"
else fail "each app is built release, --locked, by its manifest path" "$(cat "$SANDBOX/log")"; fi
if grep '^cargo ' "$SANDBOX/log" | grep -q "target=$SANDBOX/tmp/previous-build\.[^/]*/target"; then
    pass "cargo's target directory is the run's own scratch"
else fail "cargo's target directory is the run's own scratch" "$(cat "$SANDBOX/log")"; fi
if grep -qx "git -C [^ ]* fetch --quiet --no-tags --depth 1 origin $SHA_A" "$SANDBOX/log"; then
    pass "a commit the clone lacks is fetched, depth 1"
else fail "a commit the clone lacks is fetched, depth 1" "$(cat "$SANDBOX/log")"; fi
if scratch_empty && grep -q '^git .* worktree remove --force ' "$SANDBOX/log"; then
    pass "the worktree and the scratch are removed"
else fail "the worktree and the scratch are removed" "$(ls -A "$SANDBOX/tmp"; cat "$SANDBOX/log")"; fi

# --- a commit already present is not fetched ---------------------------
reset
echo "$SHA_A" > "$SANDBOX/known-commits"
run "stage13-45ba3928 $SHA_A" "$SANDBOX/out"
if [[ $rc -eq 0 ]] && ! grep -q ' fetch ' "$SANDBOX/log"; then pass "a commit the clone holds is not fetched"
else fail "a commit the clone holds is not fetched" "rc=$rc $(cat "$SANDBOX/log")"; fi

# --- a complete entry is not rebuilt; an incomplete one is -------------
reset
run "stage13-45ba3928 $SHA_A" "$SANDBOX/out"
: > "$SANDBOX/log"
run "stage13-45ba3928 $SHA_A" "$SANDBOX/out"
if [[ $rc -eq 0 ]] && ! grep -q '^cargo ' "$SANDBOX/log" && ! grep -q 'worktree add' "$SANDBOX/log"; then
    pass "an entry whose two binaries are there is not rebuilt (the cache restore)"
else fail "an entry whose two binaries are there is not rebuilt (the cache restore)" "rc=$rc $(cat "$SANDBOX/log")"; fi
rm "$SANDBOX/out/stage13-45ba3928/transportctl"
echo stray > "$SANDBOX/out/stage13-45ba3928/stray"
: > "$SANDBOX/log"
run "stage13-45ba3928 $SHA_A" "$SANDBOX/out"
if [[ $rc -eq 0 && "$(grep -c '^cargo ' "$SANDBOX/log")" -eq 2 && -x "$SANDBOX/out/stage13-45ba3928/transportctl" \
      && ! -e "$SANDBOX/out/stage13-45ba3928/stray" ]]; then
    pass "an incomplete entry is removed and built again"
else fail "an incomplete entry is removed and built again" "rc=$rc $(ls -A "$SANDBOX/out/stage13-45ba3928") $(cat "$SANDBOX/log")"; fi
chmod -x "$SANDBOX/out/stage13-45ba3928/transport-daemon"
: > "$SANDBOX/log"
run "stage13-45ba3928 $SHA_A" "$SANDBOX/out"
if [[ $rc -eq 0 && "$(grep -c '^cargo ' "$SANDBOX/log")" -eq 2 ]]; then pass "a binary that is not executable counts as missing"
else fail "a binary that is not executable counts as missing" "rc=$rc $(cat "$SANDBOX/log")"; fi

# --- two entries: each from its own commit; others in <dir> untouched --
reset
mkdir -p "$SANDBOX/out/unlisted"
run "stage13-45ba3928 $SHA_A
stage99-01234567 $SHA_B" "$SANDBOX/out"
if [[ $rc -eq 0 && "$("$SANDBOX/out/stage99-01234567/transportctl")" == "transportctl $SHA_B" \
      && "$("$SANDBOX/out/stage13-45ba3928/transportctl")" == "transportctl $SHA_A" && -d "$SANDBOX/out/unlisted" ]] && scratch_empty; then
    pass "two entries are each built from their own commit; an unlisted directory is left alone"
else fail "two entries are each built from their own commit; an unlisted directory is left alone" "rc=$rc $out"; fi

# --- the list is refused whole, before anything is built ---------------
refused() {  # refused <name> <list> <expected message part>
    reset
    run "$2" "$SANDBOX/out"
    if [[ $rc -eq 1 && "$out" == *"$3"* ]] && ! grep -q '^cargo \|worktree add\| fetch ' "$SANDBOX/log" 2>/dev/null \
       && [[ -z "$(ls -A "$SANDBOX/out" 2>/dev/null)" ]]; then
        pass "refused before any build: $1"
    else fail "refused before any build: $1" "rc=$rc $out $(cat "$SANDBOX/log" 2>/dev/null)"; fi
}
refused "a short sha" "stage13-45ba3928 45ba3928" "is not a full 40-hex commit"
refused "an upper-case sha" "stage13-45ba3928 ${SHA_A^^}" "is not a full 40-hex commit"
refused "a label not ending in the sha's prefix" "stage13 $SHA_A" "does not end in -45ba3928"
refused "a label naming another commit" "stage13-01234567 $SHA_A" "does not end in -45ba3928"
refused "a label with a slash" "x/stage13-45ba3928 $SHA_A" "is not [a-z0-9]"
refused "a label starting with a dot" ".stage13-45ba3928 $SHA_A" "is not [a-z0-9]"
refused "a third field" "stage13-45ba3928 $SHA_A extra" "got more"
refused "a label listed twice, the second entry good" "stage13-45ba3928 $SHA_A
stage13-45ba3928 $SHA_A" "listed twice"
refused "an empty list" "# nothing here" "lists no entry"
refused "a bad line after a good one" "stage13-45ba3928 $SHA_A
stage99-01234567 0123" "is not a full 40-hex commit"

# --- a failure leaves no entry and no scratch --------------------------
failed() {  # failed <name> <sandbox flag file> <expected message part>
    reset
    touch "$SANDBOX/$2"
    run "stage13-45ba3928 $SHA_A" "$SANDBOX/out"
    if [[ $rc -eq 1 && "$out" == *"$3"* && -z "$(ls -A "$SANDBOX/out")" ]] && scratch_empty; then
        pass "fails, leaving no entry and no scratch: $1"
    else fail "fails, leaving no entry and no scratch: $1" "rc=$rc $out $(ls -A "$SANDBOX/out" "$SANDBOX/tmp")"; fi
}
failed "the fetch" fetch-fails "could not fetch"
failed "the worktree" worktree-fails "could not check"
failed "the second app's build" cargo-fails-transportctl "could not build transportctl"
failed "a build that leaves no binary" cargo-no-output-transport-daemon "left no executable"

# --- usage --------------------------------------------------------------
reset
out="$(PATH="$BIN:$SYS_PATH" bash "$UNDER_TEST" 2>&1)"; rc=$?
if [[ $rc -eq 2 ]]; then pass "no <dir> is a usage error (2)"; else fail "no <dir> is a usage error (2)" "rc=$rc $out"; fi
out="$(PATH="$BIN:$SYS_PATH" bash "$UNDER_TEST" --list 2>&1)"; rc=$?
if [[ $rc -eq 2 ]]; then pass "--list without a file is a usage error (2)"; else fail "--list without a file is a usage error (2)" "rc=$rc $out"; fi
out="$(PATH="$BIN:$SYS_PATH" bash "$UNDER_TEST" --help 2>&1)"; rc=$?
if [[ $rc -eq 0 && "$out" == *"INTERWEAVE_PREVIOUS_BUILDS"* && "$out" != *"<<< help"* ]]; then pass "--help prints the header"
else fail "--help prints the header" "rc=$rc $out"; fi
out="$(PATH="$BIN:$SYS_PATH" bash "$UNDER_TEST" --list "$SANDBOX/no-such-list" "$SANDBOX/out" 2>&1)"; rc=$?
if [[ $rc -eq 1 && "$out" == *"no such list"* ]]; then pass "a missing list fails (1)"; else fail "a missing list fails (1)" "rc=$rc $out"; fi

# --- the committed list itself -------------------------------------------
reset
out="$(PATH="$BIN:$SYS_PATH" TMPDIR="$SANDBOX/tmp" RUNNER_TEMP="" bash "$UNDER_TEST" "$SANDBOX/out" 2>&1)"; rc=$?
if [[ $rc -eq 0 && -x "$SANDBOX/out/stage13-45ba3928/transport-daemon" ]]; then
    pass "the committed previous-builds.txt is well-formed and names stage13-45ba3928"
else fail "the committed previous-builds.txt is well-formed and names stage13-45ba3928" "rc=$rc $out"; fi

if [[ $failures -gt 0 ]]; then
    echo "test_build_previous_builds: $failures failure(s)" >&2
    exit 1
fi
echo "test_build_previous_builds: all passed"
