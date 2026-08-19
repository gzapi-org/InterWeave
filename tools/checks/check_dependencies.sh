#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_dependencies.sh
#
# >>> help
# Enforce the dependency policy in deny.toml: advisories, licences,
# bans, and sources.
#
#   tools/checks/check_dependencies.sh
#   tools/checks/check_dependencies.sh --config <path>
#
# Four questions, and the last is the one that costs least and matters
# most:
#
#   ADVISORIES — a known-vulnerable or YANKED version in the graph. A
#                yanked version is one its own author withdrew; building
#                on it needs a reason written down, not silence.
#   LICENCES   — an ALLOW-LIST. Anything whose terms are not named in
#                deny.toml fails. A deny-list only stops what someone
#                thought to name.
#   BANS       — wildcard version requirements, and native executables
#                shipped inside a crate. A precompiled binary in a
#                crates.io package is code nobody in the dependency chain
#                has read, running at compile time with the developer's
#                environment.
#   SOURCES    — crates.io and nowhere else. A git dependency is a moving
#                target with no version, no yank mechanism, and no
#                advisory database.
#
# WHY A WRAPPER RATHER THAN `cargo deny check` IN CI: so a missing tool
# and a policy violation are different answers. cargo-deny exits non-zero
# for both, and a repository that treats "the checker could not run" as
# "the dependencies are bad" teaches people to ignore the check.
#
# The advisory database is fetched from the network. A fetch failure is an
# ENVIRONMENT problem (exit 2), not a policy violation (exit 1), and this
# script says which.
#
# Options:
#   --config <path>  use this policy instead of deny.toml (self-test only)
#   -h, --help       this text
#
# Exit codes:
#   0  the dependency graph satisfies the policy
#   1  the policy is violated — read the cargo-deny output above
#   2  cargo-deny is not installed, or the advisory database is unreachable
# <<< help

set -uo pipefail

die() { printf '%s\n' "$*" >&2; exit 2; }

show_help() {
  sed -n '/^# >>> help$/,/^# <<< help$/p' "${BASH_SOURCE[0]}" \
    | sed '1d;$d' | sed 's/^# \{0,1\}//'
}

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CONFIG="$ROOT/deny.toml"

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) show_help; exit 0 ;;
    --config) shift; [ $# -gt 0 ] || die "--config needs a value"; CONFIG="$1" ;;
    *) die "check_dependencies: unexpected argument: $1" ;;
  esac
  shift
done

[ -f "$CONFIG" ] || die "check_dependencies: no policy at $CONFIG"

# `cargo deny` is a subcommand, so check for the binary the way cargo
# resolves it rather than trusting PATH alone.
if ! command -v cargo-deny >/dev/null 2>&1 && ! cargo deny --version >/dev/null 2>&1; then
  cat >&2 <<'MSG'
check_dependencies: cargo-deny is not installed.

  cargo install cargo-deny --locked

This is not a policy failure — the policy was not evaluated at all. CI
installs it explicitly; a local run needs it once.
MSG
  exit 2
fi

output="$(cd "$ROOT" && cargo deny --config "$CONFIG" check 2>&1)"
status=$?

printf '%s\n' "$output"

if [ "$status" -eq 0 ]; then
  printf '\ncheck_dependencies: OK — advisories, licences, bans, and sources all satisfied.\n'
  exit 0
fi

# Tell an unreachable advisory database apart from a real violation. The
# first is a bad afternoon on someone else's network; the second is a
# decision this repository has to make.
if printf '%s' "$output" | grep -qiE 'unable to fetch advisory|failed to fetch|could not.*advisory-db|network|dns error|connection refused'; then
  printf '\ncheck_dependencies: the advisory database could not be fetched.\n' >&2
  printf 'This is an environment problem, not a policy violation.\n' >&2
  exit 2
fi

printf '\ncheck_dependencies: the dependency policy is violated (see above).\n' >&2
exit 1
