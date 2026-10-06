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
#   * whether the INCLUDED Actions allowance is already spent — counted
#     over the current billing month and the organisation's PRIVATE
#     repositories only, the minutes the allowance covers.
#
#     Run from a PUBLIC repository (InterWeave is one), the second check
#     cannot hold a run: its runs on standard GitHub-hosted runners bill nothing
#     and draw on no allowance. The answer says so, quotes the private
#     repositories' usage as context, and exits 0 unless the platform is
#     down.
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
#   0  healthy — Actions operational and the allowance not exhausted,
#      or this repository is public (its runs cost nothing)
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

# The names, as a JSON array, of the repositories in a usage payload that
# are PRIVATE — the only ones whose minutes count toward the included
# allowance. One lookup per distinct repositoryName; a lookup that fails
# keeps the repository (counted — the conservative side).
private_repos_json() {
    local org="$1" usage="$2" name priv out="[]"
    while read -r name; do
        [[ -n "$name" ]] || continue
        # Bare ("gzapp") as this organisation's payload sends it, or
        # owner-qualified ("org/gzapp") as GitHub's documentation shows:
        # the lookup takes either, and the list keeps the name as sent.
        priv="$(gh api "repos/$org/${name#"$org"/}" --jq '.private' 2>/dev/null || echo "true")"
        [[ "$priv" == "false" ]] || out="$(jq -c --arg n "$name" '. + [$n]' <<<"$out")"
    done < <(printf '%s' "$usage" | jq -r '[.usageItems[]? | select(.product == "actions") | .repositoryName // empty] | unique | .[]' 2>/dev/null)
    printf '%s' "$out"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --org)   ORG="${2:-}"; [[ -n "$ORG" ]] || die "--org needs a value"; shift 2 ;;
        --included)
            INCLUDED="${2:-}"
            [[ -n "$INCLUDED" ]] || die "--included needs a value"
            shift 2 ;;
        --quiet) QUIET=1; shift ;;
        -h|--help)
            # The whole header comment, however long it grows: a fixed
            # line range had already cut the usage and the exit codes.
            awk 'NR > 3 && !/^#/ { exit } NR > 3 { sub(/^# ?/, ""); print }' "$0"
            exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
done

say() { [[ "$QUIET" -eq 1 ]] || printf '%s\n' "$*"; }

# A bad allowance is an invocation problem (exit 2) whatever the network
# says, so it is judged before any call: validated only once billing had
# answered, it vanished behind every earlier exit — an unreadable billing
# API, or a public repository's OK line.
if [[ -n "$INCLUDED" ]] && ! awk -v i="$INCLUDED" 'BEGIN { exit !(i + 0 > 0) }'; then
    die "INTERWEAVE_ACTIONS_INCLUDED_MINUTES must be a positive number, got '$INCLUDED'"
fi

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
        # THIS REPOSITORY'S VISIBILITY decides whether billing can say
        # anything about a run here. A public repository's runs on
        # standard GitHub-hosted runners bill nothing and draw on no allowance,
        # so the organisation's spend — another repository's — is
        # context, never a reason to hold a run here. Reported as
        # "$87 billing as overage, every further minute is money", it
        # held a reviewed InterWeave PR whose run billed 0 ms (#194,
        # 2026-10-06). A visibility that cannot be read is private: the
        # conservative side, the answer before this existed.
        public=""
        [[ "$(gh repo view --json isPrivate -q .isPrivate 2>/dev/null || true)" == "false" ]] && public="yes"
        self="$(gh repo view --json name -q .name 2>/dev/null || true)"
        # THE CURRENT BILLING MONTH, and only it. Unfiltered, the usage
        # endpoint returns per-month items for more than one period —
        # after a rollover, last month's rows sit beside this month's —
        # and a sum across everything reported two months as "this
        # period" (gzapp's copy, 5c735d88f). The request names the month
        # (UTC, which is what the billing dates are), and the sum keeps
        # only rows dated in it, so a server that ignored the parameters
        # could not put the previous month back.
        period="$(date -u +%Y-%m)"
        usage="$(gh api "/organizations/$ORG/settings/billing/usage?year=${period%-*}&month=$((10#${period#*-}))" 2>/dev/null || true)"
        if [[ -n "$usage" ]]; then
            reachable=1
            # THE MINUTE SKU, not every Actions charge. `mins` already
            # filters on unitType, so summing netAmount across the whole
            # product compared two different things: a billed Actions
            # STORAGE line would read as "runner overage is being paid
            # for" while minute runners had actually stopped. A row with
            # no date at all is counted: the filter excludes OTHER months,
            # it does not demand a field older payloads may lack.
            # PRIVATE repositories only. The included minutes cover
            # private repositories; a public repository's minutes are
            # free and ride in the same payload with a discountAmount
            # equal to their grossAmount. Summed in, they read as
            # allowance spent while every job runs (gzapp's copy,
            # fda5604d9: 51 049 of 50 000 reported with the private
            # repositories at 42 406). Each repositoryName is looked up
            # once; one that cannot be read counts (the conservative
            # side); a row with no repositoryName counts (older payloads).
            priv="$(private_repos_json "$ORG" "$usage")"
            net="$(printf '%s' "$usage" \
                | jq -r --arg p "$period" --argjson priv "$priv" '[.usageItems[]? | select(.product == "actions" and (.unitType == "Minutes") and (((.date // $p) | tostring)[0:7] == $p) and (.repositoryName as $r | ($r == null) or ($priv | index($r) != null))) | .netAmount] | add // 0' \
                2>/dev/null || echo 0)"
            mins="$(printf '%s' "$usage" \
                | jq -r --arg p "$period" --argjson priv "$priv" '[.usageItems[]? | select(.product == "actions" and (.unitType == "Minutes") and (((.date // $p) | tostring)[0:7] == $p) and (.repositoryName as $r | ($r == null) or ($priv | index($r) != null))) | .quantity] | add // 0' \
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

            # A public repository: the allowance above does not apply to a
            # run here, but its OWN billed minutes do. Standard runners
            # are free in a public repository; a larger runner is charged
            # there too, so money billed against this repository's minute
            # rows is a cost like any other. Decided on netAmount, not on
            # sku names, which the live payload does not show for larger
            # runners (#197 review, finding 2).
            if [[ -n "$public" ]]; then
                selfnet="$(printf '%s' "$usage" \
                    | jq -r --arg p "$period" --arg s "$self" --arg o "$ORG" '[.usageItems[]? | select(.product == "actions" and (.unitType == "Minutes") and (((.date // $p) | tostring)[0:7] == $p) and (.repositoryName == $s or .repositoryName == ($o + "/" + $s))) | .netAmount] | add // 0' \
                    2>/dev/null || echo 0)"
                if awk -v n="$selfnet" 'BEGIN { exit !(n > 0) }'; then
                    say "DEGRADED — this repository is public, yet \$${selfnet} of its own minutes bill this period: runners beyond the free standard ones are charged here too. Runs still start, so this blocks nothing; every further minute on them is money."
                    exit 1
                fi
                # Its name unread, its own rows were never matched: say so
                # rather than claim they bill nothing.
                own="none of its minutes bill"
                [[ -n "$self" ]] || own="its own billing could not be checked (name unread)"
                say "OK — ${ops_phrase}; this repository is public and ${own}, so its runs on standard runners cost nothing and draw on no allowance. (The organisation's private repositories: ${mins} minutes this period${billed:+, \$${net} billing as overage}.)"
                exit 0
            fi

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

            left="$(awk -v i="$INCLUDED" -v m="$mins" 'BEGIN { printf "%.0f", i - m }')"

            # BILLED IS DECIDED BEFORE THE REMAINDER IS CONSULTED, and
            # the order is the fix. Money can be moving while the general
            # allowance still has minutes left -- a separately billable
            # runner sku does exactly that -- and checking `m >= i` first
            # let that case fall through to the OK line below. The script
            # then told a caller to go ahead without mentioning that
            # every further minute costs, which is the one thing the
            # billed semantics exist to say.
            if [[ -n "$billed" ]]; then
                if awk -v i="$INCLUDED" -v m="$mins" 'BEGIN { exit !(m >= i) }'; then
                    say "DEGRADED — past the included allowance (${mins} of ${INCLUDED} minutes used) and \$${net} is billing as overage. Runs still start, so this blocks nothing; every further minute is money."
                else
                    say "DEGRADED — ${mins} of ${INCLUDED} minutes used, so the general allowance has room, and yet \$${net} of minutes is billing. Some usage is charged outside that allowance. Runs still start, so this blocks nothing; every further minute is money."
                fi
                exit 1
            fi

            if awk -v i="$INCLUDED" -v m="$mins" 'BEGIN { exit !(m >= i) }'; then
                say "DEGRADED — the included Actions allowance is spent (${mins} of ${INCLUDED} minutes used) and nothing is being billed, so this plan is not buying overage. Jobs stop getting runners: they fail in seconds with no steps and no logs. Nothing will merge until the period resets."
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

if [[ -n "${public:-}" ]]; then
    say "OK — ${ops_phrase}; this repository is public, so its runs on standard runners cost nothing. (Billing API unreadable: a larger runner's charges could not be checked.)"
    exit 0
fi
say "OK — ${ops_phrase}. (Allowance not checked: billing API unreadable.)"
exit 0
