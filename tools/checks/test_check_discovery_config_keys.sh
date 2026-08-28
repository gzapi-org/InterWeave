#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Self-test for check_discovery_config_keys.sh.
#
# A guard that cannot fail is a guard that proves nothing, so the cases
# below build synthetic trees where the answer is known and assert the
# exit code — including the two "the file layout moved" cases, which
# must be invocation errors rather than a silent pass.

set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CHECK="$HERE/check_discovery_config_keys.sh"
failures=0

scaffold() {
    # $1 root, $2 extra schema keys, $3 extra struct fields
    mkdir -p "$1/architecture/config" "$1/crates/config/profile-config/src"
    cat > "$1/architecture/config/config.schema.yaml" <<YAML
discovery:
  providers: list[ProviderConfig, max=16]
  types:
    peer-cache:
      enabled: bool
      config:
        ttl: duration
$2
trust:
  allowlist: list[peer-id]
YAML
    cat > "$1/crates/config/profile-config/src/lib.rs" <<RUST
pub struct DiscoveryProviderSettings {
    pub ttl: Option<String>,
$3
}
RUST
}

expect() {
    local name="$1" want="$2" root="$3"
    bash "$CHECK" --root "$root" >/dev/null 2>&1
    local got=$?
    if [ "$got" != "$want" ]; then
        echo "FAIL $name: wanted exit $want, got $got" >&2
        failures=$((failures + 1))
    else
        echo "ok   $name"
    fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# 1. Agreement.
scaffold "$tmp/ok" "" ""
expect "agreeing schema and struct pass" 0 "$tmp/ok"

# 2. A documented key with no field — the case that cost two P1s.
scaffold "$tmp/missing" "        network_id: string" ""
expect "a documented key with no field fails" 1 "$tmp/missing"

# 3. A field the schema does not document is NOT an error: the struct is
#    shared across provider types and may lead the schema.
scaffold "$tmp/extra" "" "    pub carried_ahead: Option<u32>,"
expect "an undocumented field is allowed" 0 "$tmp/extra"

# 4/5. Layout moved: neither may pass silently.
mkdir -p "$tmp/noschema"
expect "an unreadable schema is an invocation error" 2 "$tmp/noschema"

scaffold "$tmp/norust" "" ""
printf 'pub struct SomethingElse {}\n' > "$tmp/norust/crates/config/profile-config/src/lib.rs"
expect "a moved struct declaration is an invocation error" 2 "$tmp/norust"

# 6. --help works and says the script's name.
if bash "$CHECK" --help 2>&1 | grep -q 'check_discovery_config_keys.sh'; then
    echo "ok   --help describes the script"
else
    echo "FAIL --help" >&2
    failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
    echo "test_check_discovery_config_keys: $failures failure(s)" >&2
    exit 1
fi
echo "test_check_discovery_config_keys: OK — 6 cases."
