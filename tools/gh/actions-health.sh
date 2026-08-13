#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/actions-health.sh
#
# Is it worth spending CI minutes right now?
#
# Answers in one line and one exit code, before you push, re-run, or
# re-trigger anything. It exists because on 2026-08-06 two PRs were
# re-run into a GitHub Actions outage that had already been declared —
# every job died in "Set up job" or was cancelled with zero steps, and
# the only thing that changed was the clock.
#
# It checks the two things that make a run pointless and that the PR
# itself cannot tell you:
#
#   * the Actions component on githubstatus.com — unauthenticated, so it
#     answers even when the token is the problem;
#   * whether the INCLUDED Actions allowance is already spent.
#
#     The billing API reports usage, never the plan's limit, so the limit
#     is CONFIGURED, not discovered: $INTERWEAVE_ACTIONS_INCLUDED_MINUTES,
#     set in .claude/settings.json (or --included N, or the environment).
#     No plan size is hardcoded anywhere in this repo — the number lives
#     in that one setting, and changing plan means changing it there.
#
#     With the setting present this reports the exact remaining minutes.
#     Without it, the allowance size is unknown and the script says so
#     rather than guessing: usage alone cannot tell you how much is left.
#
#     netAmount > 0 is checked too. It means overage is actually being
#     billed, which is a true "allowance gone" signal — but only on a
#     plan that purchases overage. A plan that does not is never billed;
#     GitHub simply stops handing out runners, so net stays 0 while
#     everything dies. Usage-against-limit is the check that fires there.
#
# Deliberately NOT wired into anything automatically. A network call on
# every push would cost more, in latency and in noise, than the rare
# outage it guards against. This is a thing you run when CI is behaving
# oddly, or before a deliberately expensive action — a full re-run, a
# queue re-trigger, a big fan-out.
#
# Usage:
#   tools/gh/actions-health.sh              # this repo's owner
#   tools/gh/actions-health.sh --org NAME   # explicit owner
#   tools/gh/actions-health.sh --quiet      # exit code only
#   tools/gh/actions-health.sh --included N # override the configured allowance
#
# Exit codes:
#   0  healthy — Actions operational and the allowance not exhausted
#   1  degraded — spending minutes now is likely wasted (reason on stdout)
#   2  invocation problem, or neither source could be read
#
# The distinction between 1 and 2 matters: 1 is a fact about GitHub, 2 is
# "this script could not find out", and treating the second as the first
# would stop work for no reason.

set -uo pipefail

ORG=""
QUIET=0
# The plan's included minutes. Configured, never hardcoded — see the header.
INCLUDED="${INTERWEAVE_ACTIONS_INCLUDED_MINUTES:-}"

die() { echo "actions-health: $*" >&2; exit 2; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --org)   ORG="${2:-}"; [[ -n "$ORG" ]] || die "--org needs a value"; shift 2 ;;
        --included)
            INCLUDED="${2:-}"
            [[ -n "$INCLUDED" ]] || die "--included needs a value"
            shift 2 ;;
        --quiet) QUIET=1; shift ;;
        -h|--help)
            sed -n '3,46p' "$0" | sed 's/^# \{0,1\}//'
            exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
done

say() { [[ "$QUIET" -eq 1 ]] || printf '%s\n' "$*"; }

command -v jq >/dev/null 2>&1 || die "jq is required"

reachable=0

# ── 1. Is Actions up? ───────────────────────────────────────────────
if command -v curl >/dev/null 2>&1; then
    summary="$(curl -fsS --max-time 10 \
        https://www.githubstatus.com/api/v2/summary.json 2>/dev/null || true)"
    if [[ -n "$summary" ]]; then
        reachable=1
        status="$(printf '%s' "$summary" \
            | jq -r '[.components[]? | select(.name == "Actions") | .status] | first // "unknown"' \
            2>/dev/null || echo unknown)"
        if [[ "$status" != "operational" && "$status" != "unknown" ]]; then
            # Name the incident too — "major_outage" alone does not say
            # whether anyone is working on it.
            inc="$(printf '%s' "$summary" \
                | jq -r '[.incidents[]? | .name] | first // ""' 2>/dev/null || true)"
            say "DEGRADED — GitHub Actions is ${status}${inc:+ (${inc})}. Runs will fail or never start; do not spend minutes."
            exit 1
        fi
    fi
fi

# ── 2. Is the included allowance spent? ─────────────────────────────
if command -v gh >/dev/null 2>&1; then
    [[ -n "$ORG" ]] || ORG="$(gh repo view --json owner -q .owner.login 2>/dev/null || true)"
    if [[ -n "$ORG" ]]; then
        usage="$(gh api "/organizations/$ORG/settings/billing/usage" 2>/dev/null || true)"
        if [[ -n "$usage" ]]; then
            reachable=1
            net="$(printf '%s' "$usage" \
                | jq -r '[.usageItems[]? | select(.product == "actions") | .netAmount] | add // 0' \
                2>/dev/null || echo 0)"
            mins="$(printf '%s' "$usage" \
                | jq -r '[.usageItems[]? | select(.product == "actions" and (.unitType == "Minutes")) | .quantity] | add // 0' \
                2>/dev/null || echo 0)"
            if awk -v n="$net" 'BEGIN { exit !(n > 0) }'; then
                say "DEGRADED — the included Actions allowance is spent (${mins} minutes used, \$${net} now billing). On a plan that cannot buy more, green code will not merge."
                exit 1
            fi

            # No allowance configured: usage alone cannot say what is left,
            # so report the usage and decline to guess at the remainder.
            if [[ -z "$INCLUDED" ]]; then
                say "OK — Actions operational; ${mins} minutes used this period. (Remaining unknown: set INTERWEAVE_ACTIONS_INCLUDED_MINUTES in .claude/settings.json.)"
                exit 0
            fi
            if ! awk -v i="$INCLUDED" 'BEGIN { exit !(i + 0 > 0) }'; then
                die "INTERWEAVE_ACTIONS_INCLUDED_MINUTES must be a positive number, got '$INCLUDED'"
            fi

            left="$(awk -v i="$INCLUDED" -v m="$mins" 'BEGIN { printf "%.0f", i - m }')"
            if awk -v i="$INCLUDED" -v m="$mins" 'BEGIN { exit !(m >= i) }'; then
                say "DEGRADED — the included Actions allowance is spent (${mins} of ${INCLUDED} minutes used). Jobs stop getting runners: they fail in seconds with no steps and no logs. Nothing will merge until the period resets."
                exit 1
            fi
            say "OK — Actions operational; ${mins} of ${INCLUDED} minutes used this period, ${left} remaining."
            exit 0
        fi
    fi
fi

if [[ "$reachable" -eq 0 ]]; then
    die "could not read githubstatus.com or the billing API — health unknown"
fi

say "OK — Actions operational. (Allowance not checked: billing API unreadable.)"
exit 0
