#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_shell_scripts.sh
#
# >>> help
# Every tracked shell script passes shellcheck at warning severity.
#
# WHY THIS EXISTS. Every shell script the repository owns — everything
# `git ls-files '*.sh'` returns, which today means the tree checks and
# their self-tests under `tools/`, the spike harnesses under `spikes/`,
# and the status line under `.claude/` — went unread by any check until
# this guard. The count is deliberately not written here: the success
# line below prints what was actually judged, and a number in prose is
# one more thing to falsify. Rust gets
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
# judgement rather than a default: `info` and `style` are advisory
# notes, and `warning` is where shellcheck starts saying "probably a
# bug". At `warning` the tree is clean. An earlier version of this
# paragraph justified the choice by SC2015 on the `A && B || { ...;
# exit N; }` idiom this repository uses on purpose, with counts -- and
# the self-test then showed that on the pinned 0.11.0 that idiom is
# EXEMPT from SC2015 at every severity, because the fail arm exits. So
# what the tree reports at `info` was not the reason, and it has not
# been measured on the pinned release; the self-test pins the threshold
# at both ends with a fixture that does fire SC2015 (a fail arm that
# continues), and the judgement stands on its own.
#
# WHAT SHELLCHECK READS BESIDES THE FILES, and why it is switched off:
# `SHELLCHECK_OPTS` in the environment and `.shellcheckrc` files (from
# each checked file's directory upward, and under `$HOME`) both change
# what is reported, silently. A developer with `-e SC2155` in either
# would get the OK line below asserting a threshold that was not the one
# applied. The guard unsets the variable and passes `--norc`, so a local
# run and a CI run judge the same thing.
#
# Usage:
#   bash tools/checks/check_shell_scripts.sh
#
# Exit codes:
#   0  every tracked shell script is clean at warning severity
#   1  shellcheck reported at least one finding — read its output above
#   2  the guard could not judge the tree: an argument was passed (it
#      takes none), the repository root could not be entered, shellcheck
#      is not installed, a temporary file could not be created, the
#      tracked file list is empty or could not be read, shellcheck could
#      not open a tracked file, or the severity was not one it accepts.
#      Never a finding, and never a pass.
# <<< help
set -uo pipefail

die() { printf '%s\n' "$*" >&2; exit 2; }

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi
# NO OTHER ARGUMENT, and an unknown one is refused rather than ignored:
# this guard's subject IS a file list, so `check_shell_scripts.sh
# some/script.sh` silently judging all of them while the caller believes
# one was singled out is the worst reading available. Review finding on
# PR #82.
[[ $# -eq 0 ]] || die "check_shell_scripts: unexpected argument: $1 (the guard takes none; it judges every tracked *.sh)"

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
the shape this repository refuses. CI installs a pinned shellcheck
before this step in both jobs that run it, so there a failed install
surfaces here as exit 2 and never as a silent skip; on a developer
machine this path means exactly what it says.
MISSING
    exit 2
fi

# TRACKED FILES, from git rather than `find`. A `find` would read build
# outputs, vendored copies and anything untracked a developer happens to
# have in the tree, so the set a guard judges would depend on whose
# machine it ran on.
# `-z` because `core.quotePath` (on by default) would otherwise emit a
# path with a special character double-quoted and C-escaped, and the
# linter would be handed the quoted spelling. None exists today.
# (A COMMENT MUST NOT BEGIN WITH THE WORD "shellcheck": the linter reads
# such a line as a directive and fails the file with SC1072/SC1073 at
# error severity -- which this file did, in both required jobs, on the
# run that first reached the linter.)
# And the listing's own status is read: `git ls-files` failing — not a
# repository, git absent — also yields an empty array, and "no tracked
# files" would name the wrong cause. Review findings on PR #82.
#
# THROUGH A FILE, NOT A VARIABLE. Bash command substitution cannot hold
# a NUL byte -- it drops them -- so `listing=$(git ls-files -z ...)`
# concatenated every path into one, `mapfile` made a one-element array,
# and shellcheck was asked to open a path that does not exist. That
# version turned both required jobs red on its first CI run, which is
# the correct outcome for a guard that could not read its own input,
# and the automated reviewer named the cause. A file keeps the NULs and
# `git`'s own exit status stays readable. Review finding on PR #82.
listing=$(mktemp) || die "check_shell_scripts: could not create a temporary file for the tracked set."
trap 'rm -f "$listing"' EXIT
# `--deduplicate`: a conflicted path is listed once per merge stage,
# which would lint it three times and inflate the count the OK line
# prints. A redirection that fails on the `mapfile` line -- the file
# removed underneath it -- is caught by its `|| die`, exit 2; the
# `scripts=()` before it is belt-and-braces so `${#scripts[@]}` can
# never trip `set -u` into exit 1, the finding code, whatever else
# changes here.
scripts=()
if ! git ls-files -z --deduplicate '*.sh' > "$listing"; then
    die "check_shell_scripts: git ls-files failed, so the tracked set could not be read (exit 2, not a pass)."
fi
mapfile -d '' -t scripts < "$listing" || die "check_shell_scripts: could not read the tracked set back (exit 2, not a pass)."

if [[ ${#scripts[@]} -eq 0 ]]; then
    echo "check_shell_scripts: no tracked *.sh files found — the guard would pass by looking at nothing." >&2
    exit 2
fi

# NO `-x`, DELIBERATELY. Following `source` targets would make the guard
# read files outside the tracked set -- including untracked or generated
# ones -- which is the machine-dependence the file list above is built to
# avoid. Nothing tracked sources anything today
# (`git grep -nE '^\s*(source|\.)\s+' -- '*.sh'` is empty), so the flag
# bought nothing and promised less than the comment beside it claimed.
# Add it back together with a rule about what may be sourced.
#
# The glob is extension-based, so a shell script named without `.sh`
# is invisible here. There are none today -- no tracked file outside
# `*.sh` carries a shell shebang -- and nothing would report it if one
# arrived. Review findings on PR #82.
#
# AND SHELLCHECK'S OWN EXIT CODE IS READ, not collapsed: 1 is findings,
# 2 is "some files could not be processed", 3 is shellcheck invoked with
# bad syntax, 4 is a bad option. (A syntax error IN A CHECKED FILE is a
# finding -- SC1072/SC1073 at error severity, exit 1 -- not exit 3.) The first version turned all of them
# into this guard's exit 1 with the findings message, so a tracked file
# missing from the worktree — mid-rebase, `git rm --cached` — or a typo
# in the severity read as "add a targeted disable". That is "looked at
# less than everything" wearing the "found something" code, the exact
# distinction the self-test's header argues is load-bearing.
# Review finding on PR #82.
#
# `SHELLCHECK_OPTS` and `.shellcheckrc` are neutralised — see the help.
unset SHELLCHECK_OPTS
shellcheck --norc --severity="$SEVERITY" "${scripts[@]}"
rc=$?
case $rc in
    0) ;;
    1)
        cat >&2 <<MISSING
check_shell_scripts: shellcheck reported findings at severity '$SEVERITY' (above).

A finding that is deliberate takes a targeted disable, and the
repository's convention is a reason on the same line:

  # shellcheck disable=SC2086 # word splitting is the point here: \$MODES is a list

Nothing here checks for the reason -- shellcheck honours the bare form
too -- so the reason is for the reader. A blanket disable at the top of
a file is not that, and neither is raising
INTERWEAVE_SHELLCHECK_SEVERITY to get past a warning.
MISSING
        exit 1
        ;;
    *)
        cat >&2 <<COULDNOT
check_shell_scripts: shellcheck could not judge the tracked set (its exit $rc; above).

That is not a finding and not a pass: a tracked *.sh it could not open,
or an invocation it did not accept -- most likely the severity
('$SEVERITY'; it takes error, warning, info, style).
COULDNOT
        exit 2
        ;;
esac

# THE VERSION IS PART OF THE RESULT. Severity classification belongs to
# the release, so "clean at warning" is a statement about one shellcheck;
# CI pins it, and printing it here is what makes a local run comparable
# to a CI one rather than merely similar.
echo "check_shell_scripts: OK — ${#scripts[@]} tracked shell scripts clean at severity '$SEVERITY' ($(shellcheck --version | sed -n 's/^version: /shellcheck /p'))."
