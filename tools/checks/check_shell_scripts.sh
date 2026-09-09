#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Every tracked shell script passes shellcheck at warning severity.
#
# WHY THIS EXISTS. The repository owns 44 shell scripts — the tree checks
# and their self-tests under `tools/`, and the spike harnesses under
# `spikes/` — and until this guard, no check read any of them. Rust gets
# `clippy -D warnings`, Python gets its own checks, and shell got the
# reviewer's eye and nothing else. Three of the four review rounds on
# SPIKE-004 phase B found defects in shell that no automated check
# looked at.
#
# WHAT IT WOULD AND WOULD NOT HAVE CAUGHT, stated plainly so the guard is
# not credited with more than it does. It catches misquoting, masked exit
# statuses, unreachable code, unused assignments, misuse of `[` — the
# mechanical classes. It would NOT have caught the defect that prompted
# it: comparing two ports as strings rather than as numbers is
# semantically wrong and syntactically perfect, and no linter knows which
# a variable holds. That one needed the review.
#
# SEVERITY IS `warning`, NOT `style` OR `info`, and the threshold is a
# judgement rather than a default. At `info` the tree reports 207
# findings, 159 of them SC2015 on the `A && B || { fail; }` idiom this
# repository uses deliberately and correctly — C runs when B is false,
# which is the intent every time. Gating on that would mean rewriting 39
# scripts to satisfy a note about a construct they use on purpose. At
# `warning` the tree is clean, and every finding the threshold admits is
# one shellcheck thinks is probably a bug.
#
# Usage:
#   bash tools/checks/check_shell_scripts.sh
#
# Exit codes:
#   0  every tracked shell script is clean at warning severity
#   1  shellcheck reported at least one finding — read its output above
#   2  shellcheck is not installed, or the tracked file list is empty
set -uo pipefail

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    sed -n '4,38p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
fi

cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 2

# THE SEVERITY LIVES HERE, once, because the self-test asserts the same
# threshold and a value bumped in one place only would leave the test
# exercising a guard that no longer exists.
SEVERITY="${INTERWEAVE_SHELLCHECK_SEVERITY:-warning}"

if ! command -v shellcheck >/dev/null 2>&1; then
    cat >&2 <<'MISSING'
check_shell_scripts: shellcheck is not installed.

  Fedora:        sudo dnf install ShellCheck
  Debian/Ubuntu: sudo apt-get install shellcheck
  macOS:         brew install shellcheck

On Qubes, install it in the TEMPLATE rather than an AppVM: an AppVM's
/usr is reset on every reboot, so a package installed there is gone by
the next run and this guard would start reporting "not installed" again.

Exit 2 rather than 0: a guard that passes because it could not run is
the shape this repository refuses. CI has shellcheck preinstalled on its
ubuntu runners, so this path is a developer-machine state and never a
silent skip in CI.
MISSING
    exit 2
fi

# TRACKED FILES, from git rather than `find`. A `find` would read build
# outputs, vendored copies and anything untracked a developer happens to
# have in the tree, so the set a guard judges would depend on whose
# machine it ran on.
mapfile -t scripts < <(git ls-files '*.sh')

if [[ ${#scripts[@]} -eq 0 ]]; then
    echo "check_shell_scripts: no tracked *.sh files found — the guard would pass by looking at nothing." >&2
    exit 2
fi

# `-x` so a script that sources another is checked against what it
# actually sources rather than against an unknown function.
if ! shellcheck --severity="$SEVERITY" -x "${scripts[@]}"; then
    cat >&2 <<MISSING
check_shell_scripts: shellcheck reported findings at severity '$SEVERITY' (above).

A finding that is deliberate takes a targeted disable WITH a reason on
the line above it:

  # shellcheck disable=SC2086 # word splitting is the point here: \$MODES is a list

A blanket disable at the top of a file is not that, and neither is
raising INTERWEAVE_SHELLCHECK_SEVERITY to get past a warning.
MISSING
    exit 1
fi

echo "check_shell_scripts: OK — ${#scripts[@]} tracked shell scripts clean at severity '$SEVERITY'."
