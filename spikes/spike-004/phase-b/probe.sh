#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Measure what the NAT actually did, and refuse to report a class it did
# not observe.
#
# The measurement is a comparison, not a reading. One internal
# address:port sends to two observers on the public side -- two
# sequential sockets binding the same port -- and each observer reports
# the source it SAW. Two things fall out:
#
#   * whether translation happened at all — if an observer sees the
#     peer's own private address, there is no NAT and every conclusion
#     below would be a loopback result wearing a phase-B label;
#   * whether the mapping is endpoint-independent — the same external
#     port to both observers — or per-destination, which is ONE of the
#     two variables deciding whether a hole punch SUCCEEDS. Filtering is
#     the other, and this harness neither configures nor measures it, so
#     a row licenses a claim about the mapping a punch would face and
#     not about the punch SUCCEEDING. With BOTH peers behind `eds` --
#     what this harness builds, one `NAT_MODE` for both domains --
#     failure does follow from the mapping, which is the asymmetry the
#     `eds` row relies on. That is not a property of `eds` as such: a
#     punch succeeds if EITHER direction lands, so an `eds` peer facing
#     a full-cone one can still connect on its own dial. Mapping does
#     not gate the ATTEMPT either: `DCUTR.md` §2's eligibility list
#     does not mention NAT class.
set -euo pipefail

# NO ARGUMENTS, CHECKED FIRST. The trial list is an environment
# variable, and `set --` below would silently discard anything passed
# positionally. Checked before the four `podman inspect` reads, because
# `./probe.sh --help` with the topology down otherwise dies inside
# `addr_on` when `podman inspect` fails on a container that does not
# exist, and never reaches this. (That is a different failure from the
# nil-pointer one described further down, which is the
# attached-but-not-on-that-network case.) Review finding on PR #78.
[ "$#" -eq 0 ] \
  || { echo "probe.sh takes no arguments; the trial list is the SRC_PORTS environment variable" >&2; exit 2; }

NET_PUB="${NET_PUB:-natm-pub}"
NET_LAN="${NET_LAN:-natm-lan}"

addr_on() {
  podman inspect "$1" --format "{{ (index .NetworkSettings.Networks \"$2\").IPAddress }}"
}

PEER="${PEER:-natm-peer}"
ROUTER="${ROUTER:-natm-router}"
LAN="${LAN:-$NET_LAN}"

obs1=$(addr_on natm-obs1 "$NET_PUB")
obs2=$(addr_on natm-obs2 "$NET_PUB")
peer_private=$(addr_on "$PEER" "$LAN")
router_pub=$(addr_on "$ROUTER" "$NET_PUB")
# ALL FOUR, not the two that happened to be guarded. An observer with no
# usable address makes `socat UDP-DATAGRAM::9000` fail, `|| true`
# swallows it, and the row reports NO DATA -- the verdict this script
# calls its most alarming, arriving for a setup error rather than an
# environment failure.
#
# WHAT ACTUALLY CATCHES THE COMMON CASE IS THE ASSIGNMENT. Measured on
# podman 5.8: a container not attached to the named network makes
# `podman inspect` exit non-zero (`nil pointer evaluating
# *define.InspectAdditionalNetwork.IPAddress`), so `set -e` fires at
# `addr_on` before any guard is read. These four cover the attached-but-
# addressless case and, as much, keep the four reads consistent -- two
# of them were guarded and two were not, on no principle. Review finding
# on PR #78.
[ -n "$obs1" ] || { echo "no public address for natm-obs1" >&2; exit 1; }
[ -n "$obs2" ] || { echo "no public address for natm-obs2" >&2; exit 1; }
[ -n "$peer_private" ] || { echo "no private address for $PEER" >&2; exit 1; }
[ -n "$router_pub" ] || { echo "no public address for $ROUTER" >&2; exit 1; }

# TRIED FROM MORE THAN ONE SOURCE PORT, and that is not thoroughness --
# it is the difference between a class this script observed and one it
# guessed.
#
# `masquerade random` allocates a port per flow at random, so the two
# destinations CAN be handed the same port by coincidence: roughly one
# row in 64512. Classifying from a single pair then reports `eim` for a
# correctly-built endpoint-dependent NAT, which fails a good row under
# `EXPECT` and reports the wrong class outright without it. Codex review
# on PR #78.
#
# Two trials make a false `eim` need the coincidence twice, and they are
# legitimate rather than a repeat: endpoint-independence must hold for
# EVERY internal socket, so a second bound port is a second instance of
# the property, not a second look at the first. `eds` needs only one
# trial to disagree, which is the safe asymmetry -- the harness cannot
# talk itself into the class that does not rule a punch out.
SRC_PORTS="${SRC_PORTS:-45000 45001}"

# THE TRIAL LIST IS VALIDATED BEFORE ANY TRIAL RUNS, because `class`
# starts at `eim` and only a disagreeing trial moves it. Zero trials
# therefore reported ENDPOINT-INDEPENDENT, matched `EXPECT=eim`, and
# exited 0 having measured nothing -- and `SRC_PORTS=" "` reached that,
# being set and non-null so `:-` did not substitute. That was the
# `MODES=" "` defect this harness had already found one file over,
# reproduced inside the fix for the classifier. `run.sh` happened to
# catch it through the empty `PORTS=`; the standalone invocation the
# README documents did not.
#
# TWO DISTINCT PORTS, not merely two tokens. One trial re-opens the
# single-pair coincidence the trials exist to close, and a repeated port
# is the same internal tuple twice -- conntrack hands the second trial
# the first one's mapping, so it would be literally the repeat the
# comment below says it is not.
#
# NUMERIC AND NORMALISED FIRST, because distinctness of the SPELLING is
# not distinctness of the port: `45000` and `045000` are two tokens and
# one tuple. `$((10#...))` collapses them, and a token that is not a number
# at all would otherwise reach `socat`, fail, be swallowed by the
# `|| true` on the send, and surface as NO DATA -- the wrong diagnosis
# for a caller's typo, and the one this script calls its most alarming.
#
# GLOB DISABLED for the split. An unquoted expansion is pathname
# expansion as well as word splitting, so a token containing `*` would
# be replaced by matching filenames in the working directory.
# Review findings on PR #78.
set -f
# shellcheck disable=SC2086
set -- $SRC_PORTS
[ "$#" -ge 2 ] \
  || { echo "SRC_PORTS named $# trial(s); eim cannot be observed from fewer than two" >&2; exit 2; }
normalised=""
for src in "$@"; do
  case "$src" in
    ''|*[!0-9]*) echo "SRC_PORTS holds '$src', which is not a port number" >&2; exit 2 ;;
  esac
  # BASE TEN EXPLICITLY. `$((045000))` is OCTAL -- 18944 -- so the
  # normalisation meant to collapse two spellings of one port silently
  # produced a different port. Caught by the mutation check for the
  # duplicate guard, which accepted `45000 045000` and printed
  # `45000 18944`.
  port=$((10#$src))
  # A PORT, NOT MERELY DIGITS. `*[!0-9]*` admits `0`, `70000` and a
  # twenty-digit token that wraps to something negative in 64-bit
  # arithmetic. `70000` and the wrapped one reach `socat`, fail to bind,
  # are swallowed by the `|| true` on the send, and surface as NO DATA --
  # the misdiagnosis this guard's own comment says it exists to prevent,
  # with an error message already claiming "not a port number".
  # `0` is worse than a failure: `bind=:0` binds ANY port, so the two
  # sequential sockets get two different ones, the "one internal tuple"
  # premise the comparison rests on is gone, and a correct `eim`
  # topology measures as `eds`. Review finding on PR #78.
  [ "$port" -ge 1 ] && [ "$port" -le 65535 ] \
    || { echo "SRC_PORTS holds '$src', which is not a port in 1-65535" >&2; exit 2; }
  normalised="$normalised $port"
done
# shellcheck disable=SC2086
set -- $normalised
# `set -f` COVERS BOTH SPLITS. The second one expands only arithmetic
# results today, so leaving it unprotected made its safety depend on the
# numeric guard above rather than on the split itself.
set +f
[ "$#" -eq "$(printf '%s\n' "$@" | sort -u | wc -l)" ] \
  || { echo "SRC_PORTS names one port twice, so a trial would re-measure one tuple: $*" >&2; exit 2; }

# Measure once from one bound source port, and print `p1 p2`.
#
# Every control lives here, so each trial is checked rather than only
# the last: a trial that saw the private address, an address that is not
# the router's, or a malformed port fails the whole probe.
measure_from() {
  local src="$1" seen1 seen2 attempt ip1 ip2 port1 port2 port

  podman exec natm-obs1 sh -c ': > /seen.txt'
  podman exec natm-obs2 sh -c ': > /seen.txt'

  # ONE INTERNAL ADDRESS:PORT to both observers, presented by two
  # sequential sockets -- the loop runs `podman exec` twice, and both
  # bind the same port. That is what makes the comparison meaningful,
  # and the distinction is not pedantic: RFC 4787 defines mapping
  # behaviour over the internal tuple, not over a socket handle, so two
  # sockets sharing one tuple measure the mapping, while two sockets on
  # DIFFERENT ports would be allocated two external ports under any NAT
  # and report symmetric behaviour everywhere. This said "ONE socket"
  # directly above a loop that opens two. Review finding on PR #78.
  local target
  for target in "$obs1" "$obs2"; do
    podman exec "$PEER" sh -c \
      "echo probe | socat -t1 - UDP-DATAGRAM:$target:9000,bind=:$src" >/dev/null 2>&1 || true
  done

  # POLLED, NOT SLEPT ONCE. socat's `fork` hands each datagram to a
  # SYSTEM: handler that appends asynchronously, so on a loaded host the
  # write can land after any fixed wait -- and the read that followed
  # then reported NO DATA with both datagrams delivered. A false
  # negative here reads as "the topology is not a NAT", which is the
  # most alarming verdict this script has. Codex review on PR #78.
  #
  # Bounded in ATTEMPTS rather than seconds, because each one is a
  # `podman exec` and costs far more than the sleep beside it.
  seen1=""; seen2=""; attempt=0
  while [ -z "$seen1" ] || [ -z "$seen2" ]; do
    seen1=$(podman exec natm-obs1 sh -c 'cat /seen.txt' | tail -1)
    seen2=$(podman exec natm-obs2 sh -c 'cat /seen.txt' | tail -1)
    [ -n "$seen1" ] && [ -n "$seen2" ] && break
    attempt=$((attempt + 1))
    [ "$attempt" -le 50 ] || break
    sleep 0.1
  done

  {
    printf 'from port %s\n' "$src"
    printf '  peer private   : %s %s\n' "$peer_private" "$src"
    printf '  router public  : %s\n' "$router_pub"
    printf '  observer 1 saw : %s\n' "${seen1:-<nothing>}"
    printf '  observer 2 saw : %s\n' "${seen2:-<nothing>}"
  } >&2

  [ -n "$seen1" ] && [ -n "$seen2" ] || {
    echo "VERDICT: NO DATA — an observer saw nothing, so nothing is measured" >&2
    return 1
  }

  # Space-separated, because socat's `SYSTEM:` reads a colon as its own
  # parameter separator and silently refused the address:port form.
  ip1=${seen1% *}; port1=${seen1##* }
  ip2=${seen2% *}; port2=${seen2##* }

  # THE FIRST CONTROL ON THE OBSERVATION ITSELF — the no-data check
  # above comes before it and asks whether there IS one. If the
  # observers saw the peer's own private address, no translation
  # happened and every other conclusion is void, so this is the check
  # that stops a misconfigured topology being reported as a NAT matrix.
  if [ "$ip1" = "$peer_private" ] || [ "$ip2" = "$peer_private" ]; then
    echo "VERDICT: NOT NATTED — an observer saw the peer's private address" >&2
    return 1
  fi

  # THE ADDRESS IS PART OF THE MAPPING, and an earlier version checked
  # only the ports: two different external ADDRESSES with the same port
  # would have been reported as endpoint-independent. This also closes
  # the gap the private-address control cannot see — translated, but by
  # something other than the rule this harness installed.
  if [ "$ip1" != "$router_pub" ] || [ "$ip2" != "$router_pub" ]; then
    echo "VERDICT: TRANSLATED BY SOMETHING ELSE — expected $router_pub, saw $ip1 and $ip2" >&2
    return 1
  fi

  # A line with no space would make `port` the address, both would be
  # the router's, and the classifier would say `eim` for any NAT at all.
  #
  # EACH PORT, NOT THE TWO CONCATENATED. `case "$port1$port2"` tested
  # the JOINED string, so an empty `port1` with `port2=45000` gave the
  # subject `45000` -- numeric and non-empty, so it passed -- and the
  # comparison then read the two as different and reported an
  # endpoint-dependent mapping from one observation. Review finding on
  # PR #78.
  for port in "$port1" "$port2"; do
    case "$port" in
      *[!0-9]*|"") echo "VERDICT: MALFORMED — ports were '$port1' and '$port2'" >&2; return 1 ;;
    esac
  done

  printf '%s %s' "$port1" "$port2"
}

# `eim` REQUIRES EVERY TRIAL TO AGREE; one disagreement is `eds`.
class=eim
observed=""
for src in "$@"; do
  pair=$(measure_from "$src")
  p1=${pair% *}; p2=${pair#* }
  [ "$p1" = "$p2" ] || class=eds
  observed="${observed:+$observed;}$p1,$p2"
done

if [ "$class" = eim ]; then
  verdict="ENDPOINT-INDEPENDENT MAPPING (one external port for both destinations, in every trial)"
else
  verdict="ENDPOINT-DEPENDENT MAPPING (a port per destination)"
fi
printf 'VERDICT: %s\n' "$verdict" >&2

# The harness asserts against what it was ASKED to build, so a NAT that
# silently behaves as the other class is a failure rather than a
# footnote.
if [ -z "${EXPECT:-}" ]; then
  # SAID OUT LOUD rather than passing quietly. A typo in the variable
  # name would otherwise print a verdict and exit 0 having asserted
  # nothing, which is the shape this harness exists to refuse.
  echo "UNASSERTED: no EXPECT given, so the class above was reported and not checked" >&2
elif [ "$EXPECT" != "$class" ]; then
  echo "MISMATCH: topology was configured for $EXPECT and measured as $class" >&2
  exit 1
fi

# THE CLASS AND THE PORTS IT WAS DERIVED FROM. The class alone is a
# restatement of `EXPECT`: this script exits non-zero on any other
# value, so a caller that prints only the class prints back what it
# asked for. The observed ports are the part no assertion here
# constrains -- no check requires any particular value -- and they are
# what a reader can check the verdict against. They are not always
# DIFFERENT: an `eim` row reports the bound port twice, which is the
# observation that makes it `eim`. Saying "nothing here decides what
# they are" was too strong: this script sets the source ports, and an
# `eim` NAT that can preserve them reports them back. One group per
# trial, separated by `;`. Review findings on PR #78.
printf 'CLASS=%s\n' "$class"
printf 'PORTS=%s\n' "$observed"
