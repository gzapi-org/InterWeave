#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Self-test for check_ipc_schemas_are_tested.sh.
#
# Each case builds the shape the guard must refuse or accept rather than
# asserting on the OK message; the inventory case is the one the guard
# exists for, since an inventory names every schema by construction.

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
GUARD="$SCRIPT_DIR/check_ipc_schemas_are_tested.sh"
[ -f "$GUARD" ] || { echo "test: guard not found at $GUARD" >&2; exit 1; }

failures=0
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

ok()  { echo "  ✓ $1"; }
bad() { echo "  ✗ $1" >&2; failures=$((failures + 1)); }

run_code() { bash "$GUARD" --root "$1" >/dev/null 2>&1; printf '%s' "$?"; }
run()      { bash "$GUARD" --root "$1" 2>&1; }

# A tree holding the two schemas `hello` and `close`, and a test file
# whose body is $2.
tree() {
    local root="$1" body="$2"
    mkdir -p "$root/architecture/contracts/schemas/ipc" "$root/crates/api/ipc-protocol/tests"
    printf '{}\n' > "$root/architecture/contracts/schemas/ipc/hello.schema.json"
    printf '{}\n' > "$root/architecture/contracts/schemas/ipc/close.schema.json"
    printf '{}\n' > "$root/architecture/contracts/schemas/ipc/manifest.json"
    printf '%s\n' "$body" > "$root/crates/api/ipc-protocol/tests/schema_agreement.rs"
}

expect() {
    local want="$1" root="$2" what="$3"
    if [ "$(run_code "$root")" = "$want" ]; then ok "$what"; else bad "$what (want exit $want): $(run "$root")"; fi
}

FULL='architecture/contracts/schemas/ipc'

printf 'test_check_ipc_schemas_are_tested\n'

echo "both schemas named at a reading site pass, in either spelling"
R="$TMP/pass"; tree "$R" "fn t() { validator(\"$FULL/hello.schema.json\"); schema(\"ipc/close.schema.json\"); }"
expect 0 "$R" "full path and schemas-relative path both count"

echo "a schema no test names fails, and is named in the output"
R="$TMP/unnamed"; tree "$R" "fn t() { validator(\"$FULL/hello.schema.json\"); }"
expect 1 "$R" "close is unnamed"
case "$(run "$R")" in
    *"ipc/close.schema.json"*) ok "  and the output names it" ;;
    *) bad "output should name ipc/close.schema.json: $(run "$R")" ;;
esac
case "$(run "$R")" in
    *"ipc/hello.schema.json: "*) bad "hello is named and must not be reported" ;;
    *) ok "  and does not report the named one" ;;
esac

echo "an inventory of the directory does not count"
# The case the guard is for: a list asserted equal to the directory names
# every schema the moment it exists.
R="$TMP/inventory"; tree "$R" "const IPC_SCHEMAS: [&str; 2] = [
    \"$FULL/close.schema.json\",
    \"$FULL/hello.schema.json\",
];
fn t() { validator(\"$FULL/hello.schema.json\"); }"
expect 1 "$R" "close named only in IPC_SCHEMAS fails"

echo "a line after the inventory counts again"
R="$TMP/after"; tree "$R" "pub(crate) static ALL_SCHEMAS: [&str; 1] = [
    \"$FULL/close.schema.json\",
];
fn t() { validator(\"$FULL/hello.schema.json\"); schema(\"ipc/close.schema.json\"); }"
expect 0 "$R" "the inventory ends at its ];"

echo "a comment is not a name"
R="$TMP/comment"; tree "$R" "// reads ipc/close.schema.json
/// and \`ipc/close.schema.json\` too
    // validator(\"ipc/close.schema.json\");
fn t() { validator(\"$FULL/hello.schema.json\"); }"
expect 1 "$R" "close named only in comments fails"

echo "a literal that merely ends in the name is not it"
R="$TMP/suffix"; tree "$R" "fn t() { validator(\"$FULL/hello.schema.json\"); x(\"ipc/not-close.schema.json\"); y(\"xipc/close.schema.json\"); }"
expect 1 "$R" "not-close and xipc/close do not name close"

echo "a src/ file counts only inside its #[cfg(test)] module"
R="$TMP/src"; tree "$R" "fn t() { validator(\"$FULL/hello.schema.json\"); }"
mkdir -p "$R/crates/api/ipc-protocol/src"
printf 'const DOC: &str = "ipc/close.schema.json";\n' > "$R/crates/api/ipc-protocol/src/frame.rs"
expect 1 "$R" "a name in production code does not count"
printf 'const DOC: &str = "x";\n#[cfg(test)]\nmod tests { fn t() { schema("ipc/close.schema.json"); } }\n' \
    > "$R/crates/api/ipc-protocol/src/frame.rs"
expect 0 "$R" "a name in the unit-test module counts"

echo "a root tests/ suite counts; spikes/ and target/ do not"
R="$TMP/where"; tree "$R" "fn t() { validator(\"$FULL/hello.schema.json\"); }"
mkdir -p "$R/spikes/spike-9/tests" "$R/crates/api/ipc-protocol/target/debug/tests"
printf 'fn t() { schema("ipc/close.schema.json"); }\n' > "$R/spikes/spike-9/tests/a.rs"
printf 'fn t() { schema("ipc/close.schema.json"); }\n' > "$R/crates/api/ipc-protocol/target/debug/tests/a.rs"
expect 1 "$R" "spikes/ and target/ do not vouch"
mkdir -p "$R/tests/ipc-v2/tests"
printf 'fn t() { schema("ipc/close.schema.json"); }\n' > "$R/tests/ipc-v2/tests/wire.rs"
expect 0 "$R" "tests/ipc-v2/tests/ does"

echo "a schema in a subdirectory is checked by its relative path"
R="$TMP/nested"; tree "$R" "fn t() { validator(\"$FULL/hello.schema.json\"); schema(\"ipc/close.schema.json\"); }"
mkdir -p "$R/architecture/contracts/schemas/ipc/admin"
printf '{}\n' > "$R/architecture/contracts/schemas/ipc/admin/status.schema.json"
expect 1 "$R" "ipc/admin/status is unnamed"
printf 'fn u() { schema("ipc/admin/status.schema.json"); }\n' >> "$R/crates/api/ipc-protocol/tests/schema_agreement.rs"
expect 0 "$R" "and named, it passes"

echo "nothing to check is a failure, not a pass"
R="$TMP/noschema"; mkdir -p "$R/crates/x/tests"; printf 'fn t() {}\n' > "$R/crates/x/tests/a.rs"
expect 1 "$R" "no ipc schema"
case "$(run "$R")" in
    *"nothing to check"*) ok "  and says so rather than reporting a phantom schema" ;;
    *) bad "an empty schema directory should say nothing to check: $(run "$R")" ;;
esac
R="$TMP/notest"; mkdir -p "$R/architecture/contracts/schemas/ipc"
printf '{}\n' > "$R/architecture/contracts/schemas/ipc/hello.schema.json"
expect 1 "$R" "no test source"

echo "invocation errors"
if [ "$(run_code "$TMP/nope")" = "2" ]; then ok "a missing root"; else bad "missing root should exit 2"; fi
bash "$GUARD" --bogus >/dev/null 2>&1; rc=$?
if [ "$rc" = 2 ]; then ok "an unknown option"; else bad "unknown option should exit 2, got $rc"; fi
# Guarded by `timeout`: the failure mode is an infinite loop.
timeout 5 bash "$GUARD" --root >/dev/null 2>&1; rc=$?
case "$rc" in
    2) ok "--root with no value" ;;
    124) bad "--root with no value hung" ;;
    *) bad "--root with no value exited $rc" ;;
esac
case "$(bash "$GUARD" --help 2>&1)" in
    *"--root <dir>"*"Exit codes"*) ok "--help prints the help block" ;;
    *) bad "--help should print the help block" ;;
esac

echo "the real repository passes its own guard"
REPO="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
if [ "$(run_code "$REPO")" = "0" ]; then ok "this repository"; else bad "this repository fails: $(run "$REPO")"; fi

echo
if [ "$failures" -eq 0 ]; then
    echo "test_check_ipc_schemas_are_tested: OK — all assertions passed."
    exit 0
fi
echo "test_check_ipc_schemas_are_tested: FAILED — $failures assertion(s) failed." >&2
exit 1
