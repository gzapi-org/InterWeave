#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Measure what the NAT actually did, and refuse to report a class it did
# not observe.
#
# The measurement is a comparison, not a reading. One internal socket
# sends to two observers on the public side; each reports the source it
# SAW. Two things fall out:
#
#   * whether translation happened at all — if an observer sees the
#     peer's own private address, there is no NAT and every conclusion
#     below would be a loopback result wearing a phase-B label;
#   * whether the mapping is endpoint-independent — the same external
#     port to both observers — or per-destination, which is the variable
#     that decides whether a hole punch can work.
set -euo pipefail

NET_PUB="${NET_PUB:-natm-pub}"
NET_LAN="${NET_LAN:-natm-lan}"
SRC_PORT="${SRC_PORT:-45000}"

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
[ -n "$peer_private" ] || { echo "no private address for $PEER" >&2; exit 1; }
[ -n "$router_pub" ] || { echo "no public address for $ROUTER" >&2; exit 1; }

podman exec natm-obs1 sh -c ': > /seen.txt'
podman exec natm-obs2 sh -c ': > /seen.txt'

# ONE INTERNAL ADDRESS:PORT to both observers, presented by two
# sequential sockets -- the loop runs `podman exec` twice, and
# `reuseaddr` is there because both bind the same port. That is what
# makes the comparison meaningful, and the distinction is not pedantic:
# RFC 4787 defines mapping behaviour over the internal tuple, not over a
# socket handle, so two sockets sharing one tuple measure the mapping,
# while two sockets on DIFFERENT ports would be allocated two external
# ports under any NAT and report symmetric behaviour everywhere. This
# said "ONE socket" directly above a loop that opens two. Review finding
# on PR #78.
for target in "$obs1" "$obs2"; do
  podman exec "$PEER" sh -c \
    "echo probe | socat -t1 - UDP-DATAGRAM:$target:9000,bind=:$SRC_PORT,reuseaddr" >/dev/null 2>&1 || true
done
sleep 1

seen1=$(podman exec natm-obs1 sh -c 'cat /seen.txt' | tail -1)
seen2=$(podman exec natm-obs2 sh -c 'cat /seen.txt' | tail -1)

# THE REPORT GOES TO STDERR, and stdout carries the machine-readable
# result: the class and the two ports it was derived from, one per line.
# A caller can capture that in a variable without swallowing the
# measurement -- an earlier `run.sh` read stdout and the transcript lost
# every number the README cites. This comment said "only the class" for
# one commit after `PORTS=` was added eighty lines below it, which is
# the pair-in-one-file shape CLAUDE.md §7 names.
{
  printf 'peer private   : %s %s\n' "$peer_private" "$SRC_PORT"
  printf 'router public  : %s\n' "$router_pub"
  printf 'observer 1 saw : %s\n' "${seen1:-<nothing>}"
  printf 'observer 2 saw : %s\n' "${seen2:-<nothing>}"
} >&2

[ -n "$seen1" ] && [ -n "$seen2" ] || {
  echo "VERDICT: NO DATA — an observer saw nothing, so nothing is measured" >&2
  exit 1
}

# Space-separated, because socat's `SYSTEM:` reads a colon as its own
# parameter separator and silently refused the address:port form.
ip1=${seen1% *}; port1=${seen1##* }
ip2=${seen2% *}; port2=${seen2##* }

# THE FIRST CONTROL ON THE OBSERVATION ITSELF — the no-data check above
# comes before it and asks whether there IS one. If the observers saw
# the peer's own private address, no translation happened and every
# other conclusion is void, so this is the check that stops a
# misconfigured topology being reported as a NAT matrix.
if [ "$ip1" = "$peer_private" ] || [ "$ip2" = "$peer_private" ]; then
  echo "VERDICT: NOT NATTED — an observer saw the peer's private address" >&2
  exit 1
fi

# THE ADDRESS IS PART OF THE MAPPING, and an earlier version checked
# only the ports: two different external ADDRESSES with the same port
# would have been reported as endpoint-independent. This also closes the
# gap the private-address control cannot see — translated, but by
# something other than the rule this harness installed.
if [ "$ip1" != "$router_pub" ] || [ "$ip2" != "$router_pub" ]; then
  echo "VERDICT: TRANSLATED BY SOMETHING ELSE — expected $router_pub, saw $ip1 and $ip2" >&2
  exit 1
fi

# A line with no space would make `port` the address, both would be the
# router's, and the classifier would say `eim` for any NAT at all.
#
# EACH PORT, NOT THE TWO CONCATENATED. `case "$port1$port2"` tested the
# JOINED string, so an empty `port1` with `port2=45000` gave the subject
# `45000` -- numeric and non-empty, so it passed -- and the comparison
# below then read the two as different and reported an endpoint-dependent
# mapping from one observation. Review finding on PR #78.
for port in "$port1" "$port2"; do
  case "$port" in
    *[!0-9]*|"") echo "VERDICT: MALFORMED — ports were '$port1' and '$port2'" >&2; exit 1 ;;
  esac
done

if [ "$port1" = "$port2" ]; then
  verdict="ENDPOINT-INDEPENDENT MAPPING (one external port for both destinations)"
  class=eim
else
  verdict="ENDPOINT-DEPENDENT MAPPING (a port per destination)"
  class=eds
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
# asked for. The two observed ports are the part no assertion here
# constrains -- they differ between runs and between domains, and they
# are what a reader can check the verdict against. Review finding on
# PR #78.
printf 'CLASS=%s\n' "$class"
printf 'PORTS=%s,%s\n' "$port1" "$port2"
