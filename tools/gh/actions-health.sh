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
#     netAmount on the MINUTE sku is checked too, and it means the
#     opposite of what it looks like. Billed overage proves runners are
#     still being SERVED — the plan is buying them. It is a cost, so it
#     is degraded and never a block. The plan that actually blocks is the
#     one that never bills: net stays 0 while GitHub quietly stops
#     handing out runners and every job dies in seconds. That is why the
#     block is inferred from "past the allowance AND nothing billed".
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
# What the final line may claim. Only a status actually EXTRACTED from
# the summary earns the word "operational"; until then the tool has not
# read GitHub's opinion of Actions and must not report one.
ops_phrase="Actions health unread"

# ── 1. Is Actions up? ───────────────────────────────────────────────
if command -v curl >/dev/null 2>&1; then
    summary="$(curl -fsS --max-time 10 \
        https://www.githubstatus.com/api/v2/summary.json 2>/dev/null || true)"
    if [[ -n "$summary" ]]; then
        status="$(printf '%s' "$summary" \
            | jq -r '[.components[]? | select(.name == "Actions") | .status] | first // ""' \
            2>/dev/null || echo "")"
        # REACHED IS NOT UNDERSTOOD. A non-empty body that is not this
        # schema — a captive-portal login page, a proxy error page, a
        # version bump that moved the component — parses to nothing.
        # Marking the source reachable on the body ALONE meant a script
        # that had learned nothing went on to report "Actions
        # operational" whenever billing was also unreadable: the exit-2
        # health-unknown case wearing a green answer, from the one tool
        # whose job is deciding whether a CI run is worth spending.
        if [[ -n "$status" ]]; then
            reachable=1
            ops_phrase="Actions operational"
        fi
        if [[ -n "$status" && "$status" != "operational" ]]; then
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
            # THE MINUTE SKU, not every Actions charge. `mins` already
            # filters on unitType, so summing netAmount across the whole
            # product compared two different things: a billed Actions
            # STORAGE line would read as "runner overage is being paid
            # for" while minute runners had actually stopped.
            net="$(printf '%s' "$usage" \
                | jq -r '[.usageItems[]? | select(.product == "actions" and (.unitType == "Minutes")) | .netAmount] | add // 0' \
                2>/dev/null || echo 0)"
            mins="$(printf '%s' "$usage" \
                | jq -r '[.usageItems[]? | select(.product == "actions" and (.unitType == "Minutes")) | .quantity] | add // 0' \
                2>/dev/null || echo 0)"

            # A NONZERO net IS NOT A BLOCK, and reading it as one halted
            # work on exactly the plan where nothing was wrong. Money
            # moving proves runs are still being SERVED: an organisation
            # that purchases overage bills and keeps handing out runners.
            # The plan that blocks is the one that never bills — net
            # stays 0 while every job dies in seconds with no steps.
            # The two facts point in opposite directions.
            #
            # Neither the spending limit nor the overage setting is
            # exposed by any API readable here, so the block is inferred
            # from what is: past the configured allowance AND nothing
            # billed. Billed overage is reported as a COST instead.
            billed=""
            awk -v n="$net" 'BEGIN { exit !(n > 0) }' && billed="yes"

            # No allowance configured: usage alone cannot say what is left,
            # so report the usage and decline to guess at the remainder.
            if [[ -z "$INCLUDED" ]]; then
                # Billed minutes are DEGRADED even here. The remainder is
                # unknown; the cost is not. Reporting money already
                # moving on an exit-0 line says the expensive thing out
                # loud and then tells every caller to go ahead.
                if [[ -n "$billed" ]]; then
                    say "DEGRADED — ${mins} minutes used this period and \$${net} of it is billing as overage. Runs still start, so this blocks nothing; every further minute is money. (Allowance size unknown: set INTERWEAVE_ACTIONS_INCLUDED_MINUTES in .claude/settings.json.)"
                    exit 1
                fi
                say "OK — ${ops_phrase}; ${mins} minutes used this period. (Remaining unknown: set INTERWEAVE_ACTIONS_INCLUDED_MINUTES in .claude/settings.json.)"
                exit 0
            fi
            if ! awk -v i="$INCLUDED" 'BEGIN { exit !(i + 0 > 0) }'; then
                die "INTERWEAVE_ACTIONS_INCLUDED_MINUTES must be a positive number, got '$INCLUDED'"
            fi

            left="$(awk -v i="$INCLUDED" -v m="$mins" 'BEGIN { printf "%.0f", i - m }')"
            if awk -v i="$INCLUDED" -v m="$mins" 'BEGIN { exit !(m >= i) }'; then
                if [[ -n "$billed" ]]; then
                    say "DEGRADED — past the included allowance (${mins} of ${INCLUDED} minutes used) and \$${net} is billing as overage. Runs still start, so this blocks nothing; every further minute is money."
                else
                    say "DEGRADED — the included Actions allowance is spent (${mins} of ${INCLUDED} minutes used) and nothing is being billed, so this plan is not buying overage. Jobs stop getting runners: they fail in seconds with no steps and no logs. Nothing will merge until the period resets."
                fi
                exit 1
            fi
            say "OK — ${ops_phrase}; ${mins} of ${INCLUDED} minutes used this period, ${left} remaining."
            exit 0
        fi
    fi
fi

if [[ "$reachable" -eq 0 ]]; then
    die "could not read githubstatus.com or the billing API — health unknown"
fi

say "OK — ${ops_phrase}. (Allowance not checked: billing API unreadable.)"
exit 0
