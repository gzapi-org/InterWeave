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

obs1=$(addr_on natm-obs1 "$NET_PUB")
obs2=$(addr_on natm-obs2 "$NET_PUB")
peer_private=$(addr_on natm-peer "$NET_LAN")

podman exec natm-obs1 sh -c ': > /seen.txt'
podman exec natm-obs2 sh -c ': > /seen.txt'

# ONE socket to both observers. Binding the source port is what makes
# the comparison meaningful: two different sockets would be allocated
# two external ports under ANY NAT, and the test would report symmetric
# behaviour everywhere.
for target in "$obs1" "$obs2"; do
  podman exec natm-peer sh -c \
    "echo probe | socat -t1 - UDP-DATAGRAM:$target:9000,bind=:$SRC_PORT,reuseaddr" >/dev/null 2>&1 || true
done
sleep 1

seen1=$(podman exec natm-obs1 sh -c 'cat /seen.txt' | tail -1)
seen2=$(podman exec natm-obs2 sh -c 'cat /seen.txt' | tail -1)

printf 'peer private   : %s %s\n' "$peer_private" "$SRC_PORT"
printf 'observer 1 saw : %s\n' "${seen1:-<nothing>}"
printf 'observer 2 saw : %s\n' "${seen2:-<nothing>}"

[ -n "$seen1" ] && [ -n "$seen2" ] || {
  echo "VERDICT: NO DATA — an observer saw nothing, so nothing is measured" >&2
  exit 1
}

# Space-separated, because socat's `SYSTEM:` reads a colon as its own
# parameter separator and silently refused the address:port form.
ip1=${seen1% *}; port1=${seen1##* }
ip2=${seen2% *}; port2=${seen2##* }

# THE CONTROL, and it comes first. If the observers saw the peer's own
# private address, no translation happened and every other conclusion is
# void — this is the check that stops a misconfigured topology being
# reported as a NAT matrix.
if [ "$ip1" = "$peer_private" ] || [ "$ip2" = "$peer_private" ]; then
  echo "VERDICT: NOT NATTED — an observer saw the peer's private address" >&2
  exit 1
fi

if [ "$port1" = "$port2" ]; then
  verdict="ENDPOINT-INDEPENDENT MAPPING (one external port for both destinations)"
  class=eim
else
  verdict="ENDPOINT-DEPENDENT MAPPING (a port per destination)"
  class=eds
fi
printf 'VERDICT: %s\n' "$verdict"

# The harness asserts against what it was ASKED to build, so a NAT that
# silently behaves as the other class is a failure rather than a
# footnote.
if [ -n "${EXPECT:-}" ] && [ "$EXPECT" != "$class" ]; then
  echo "MISMATCH: topology was configured for $EXPECT and measured as $class" >&2
  exit 1
fi
