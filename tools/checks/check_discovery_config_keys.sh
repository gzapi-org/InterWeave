#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Every provider config key the discovery schema documents must be a
# field on `DiscoveryProviderSettings`.
#
# WHY THIS EXISTS
#
# `DiscoveryProviderSettings` is `deny_unknown_fields`, and that turns a
# missing field into a REJECTED PROFILE rather than an ignored setting.
# A key the schema documents and the struct omits does not go unread —
# it makes the canonical example fail to deserialize, before validation
# runs, so the reasoned refusal an operator was meant to read is
# replaced by a serde error naming a key.
#
# This has now cost two P1 findings in one pull request. The first
# modelled none of the `kademlia` namespace; the second modelled it from
# a truncated read of the schema and stopped eight keys early, so the
# canonical profile still failed on the first field past where the grep
# had ended. Both times the reasoning was right and the extent was
# wrong, and nothing anywhere compared the two documents.
#
# The check is deliberately one-directional. It asks only whether the
# schema documents something the struct cannot represent. A field the
# struct carries and the schema does not is NOT an error here: the
# struct is shared by every provider type, and a stage may legitimately
# carry a field ahead of the schema being rewritten around it.
#
# Exit codes:
#   0  every documented key is representable
#   1  at least one documented key has no field
#   2  invocation error, or neither file could be read

set -uo pipefail

usage() {
    cat <<'USAGE'
check_discovery_config_keys.sh — the discovery schema and the settings
struct must agree on what a provider config may contain

Usage:
  bash tools/checks/check_discovery_config_keys.sh [--root DIR]

Options:
  --root DIR   repository root (default: the git toplevel)
  --help       this text

Checks that every `config:` key under `discovery.providers.types.*` in
architecture/config/config.schema.yaml has a matching `pub` field on
`DiscoveryProviderSettings` in crates/config/profile-config/src/lib.rs.
USAGE
}

ROOT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --root) ROOT="${2:-}"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) echo "check_discovery_config_keys: unknown option '$1'" >&2; exit 2 ;;
    esac
done

if [ -z "$ROOT" ]; then
    ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
fi
[ -n "$ROOT" ] || { echo "check_discovery_config_keys: no --root and not in a git repo" >&2; exit 2; }

SCHEMA="$ROOT/architecture/config/config.schema.yaml"
RUST="$ROOT/crates/config/profile-config/src/lib.rs"
[ -r "$SCHEMA" ] || { echo "check_discovery_config_keys: cannot read $SCHEMA" >&2; exit 2; }
[ -r "$RUST" ]   || { echo "check_discovery_config_keys: cannot read $RUST" >&2; exit 2; }

# The documented keys: every `config:` block under discovery's per-type
# section. The schema is indentation-structured prose rather than strict
# YAML (`list[...]`, `enum[...]` are notation), so this reads structure
# by indent rather than parsing it as YAML.
documented="$(awk '
    /^discovery:/                 { in_disc = 1; next }
    in_disc && /^[a-z_]+:/        { in_disc = 0 }
    !in_disc                      { next }
    /^      config:/              { in_cfg = 1; next }
    in_cfg && /^        [a-z_]+:/ {
        line = $0
        sub(/^ +/, "", line)
        sub(/:.*/, "", line)
        print line
        next
    }
    in_cfg && /^      [a-z_-]+:/  { in_cfg = 0 }
    in_cfg && /^    [a-z_-]+:/    { in_cfg = 0 }
' "$SCHEMA" | sort -u)"

[ -n "$documented" ] || {
    echo "check_discovery_config_keys: found no documented keys — the schema layout changed" >&2
    exit 2
}

# The struct's fields, read between its declaration and its closing brace.
fields="$(awk '
    /^pub struct DiscoveryProviderSettings \{/ { in_s = 1; next }
    in_s && /^\}/                              { in_s = 0 }
    in_s && /^    pub [a-z_]+:/ {
        line = $0
        sub(/^    pub /, "", line)
        sub(/:.*/, "", line)
        print line
    }
' "$RUST" | sort -u)"

[ -n "$fields" ] || {
    echo "check_discovery_config_keys: found no struct fields — the declaration moved" >&2
    exit 2
}

missing="$(comm -23 <(printf '%s\n' "$documented") <(printf '%s\n' "$fields"))"

if [ -n "$missing" ]; then
    printf 'check_discovery_config_keys: documented but not representable:\n' >&2
    printf '  %s\n' $missing >&2
    cat >&2 <<'WHY'

`DiscoveryProviderSettings` is deny_unknown_fields, so each of these
makes a documented profile fail to DESERIALIZE — before validation can
report anything. Add the field (parsing a value is not the same as
consuming it), or change the schema if the key is genuinely gone.
WHY
    exit 1
fi

printf 'check_discovery_config_keys: OK — %d documented key(s), all representable.\n' \
    "$(printf '%s\n' "$documented" | wc -l | tr -d ' ')"
