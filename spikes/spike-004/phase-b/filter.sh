#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Measure the NAT's FILTERING behaviour, which is the half `probe.sh`
# does not touch.
#
# RFC 4787 classifies a NAT by mapping AND filtering, and mapping alone
# says nothing about whether a hole punch succeeds: an `eds` peer facing
# a full-cone one still connects on its own dial. Until this existed the
# harness measured mapping and the READMEs said filtering was "neither
# configured nor measured", so no row licensed a claim about a punch at
# all. This is the other half.
#
# THE MEASUREMENT IS WHICH OF THREE SOURCES REACHES THE PEER. One
# internal socket sends to the prober, creating a mapping; then three
# datagrams are aimed at the mapped external port, each tagged:
#
#   * from the prober's addressed port      -- the CONTROL. The endpoint
#     the peer actually sent to, so conntrack must admit it. If this one
#     does not arrive the measurement is broken and nothing below means
#     anything, which is why it is sent and asserted rather than assumed.
#   * from the prober's OTHER port          -- same address, different
#     port. Admitted by an address-dependent filter, refused by an
#     address-and-port-dependent one.
#   * from an observer                      -- a different address
#     entirely. Admitted only by an endpoint-independent filter.
#
# All three inside one socket lifetime, so one mapping is classified
# rather than three. The control is sent FIRST and is believed not to
# widen what the others meet -- the entry it matches is the one the
# peer's own packet made -- but that is read from netfilter rather than
# measured, which is the move this directory retracts elsewhere. What
# supports it is weaker and real: the `conntrack` row measures `apdf`,
# and a control that widened the mapping would have shown as `eif` and
# failed that row. Review finding on PR #81.
set -euo pipefail

NET_PUB="${NET_PUB:-natm-pub}"
NET_LAN="${NET_LAN:-natm-lan}"
PEER="${PEER:-natm-peer}"
ROUTER="${ROUTER:-natm-router}"
LAN="${LAN:-$NET_LAN}"
PROBER="${PROBER:-natm-filt}"
ALT_SOURCE="${ALT_SOURCE:-natm-obs2}"
PROBE_PORT="${PROBE_PORT:-9001}"
PROBE_PORT_ALT="${PROBE_PORT_ALT:-9002}"
ALT_SOURCE_PORT="${ALT_SOURCE_PORT:-9500}"
SRC_PORT="${SRC_PORT:-45000}"

# How long the peer's socket stays open for the three probes to arrive.
HOLD_SECONDS="${HOLD_SECONDS:-6}"

[ "$#" -eq 0 ] \
  || { echo "filter.sh takes no arguments; it is configured by environment" >&2; exit 2; }

# THREE DISTINCT ENDPOINTS, or two probes are the control wearing another
# name: `PROBE_PORT_ALT=$PROBE_PORT` makes SAME_ADDRESS the control, and
# `ALT_SOURCE=$PROBER` makes OTHER_ADDRESS the same address. Either
# misclassifies UPWARD, toward the permissive end.
# Review finding on PR #81.
# COMPARED AS NUMBERS, NOT AS SPELLINGS. `PROBE_PORT_ALT=09001` against
# `PROBE_PORT=9001` is two strings and one UDP port, so a string
# comparison admits it and SAME_ADDRESS becomes the control wearing
# another name -- an `address-restricted` row would then match `adf` on
# two packets from the same endpoint.
#
# `probe.sh` HAS THE SAME FIX for the same reason, at its `SRC_PORTS`
# guard: `045000` and `45000` are two spellings of one tuple, and a
# `sort -u` over spellings does not see it. Base ten EXPLICITLY in both,
# because `$((045000))` is OCTAL -- 18944, a different port -- and
# `$((09001))` is not a valid literal at all. An earlier version of this
# comment cited `profile-config` instead; that crate is Rust, parses
# ports with `parse::<u16>()`, and carries no such case, so the citation
# pointed at nothing a reader could check.
# Codex review on PR #81.
# HOLD_SECONDS TOO, because it is the one number fed to arithmetic on the
# HOST shell rather than to a container: `$((HOLD_SECONDS * 20 + 40))`
# evaluates whatever it is handed. The guard that validated four ports
# and not this was one variable short in the commit that added it.
# Review finding on PR #81.
case "${HOLD_SECONDS:-}" in
  ''|*[!0-9]*) echo "HOLD_SECONDS holds '${HOLD_SECONDS:-}', which is not a number of seconds" >&2; exit 2 ;;
esac
HOLD_SECONDS=$((10#$HOLD_SECONDS))
[ "$HOLD_SECONDS" -ge 1 ] && [ "$HOLD_SECONDS" -le 120 ] \
  || { echo "HOLD_SECONDS holds '$HOLD_SECONDS'; the window must be 1-120 seconds" >&2; exit 2; }

for port_name in PROBE_PORT PROBE_PORT_ALT ALT_SOURCE_PORT SRC_PORT; do
  eval "port_value=\$$port_name"
  case "$port_value" in
    ''|*[!0-9]*) echo "$port_name holds '$port_value', which is not a port number" >&2; exit 2 ;;
  esac
  port_value=$((10#$port_value))
  # THE SPELLING THE CALLER TYPED, not the normalised value: reporting
  # `070000` as "holds '70000'" names a number nobody wrote.
  [ "$port_value" -ge 1 ] && [ "$port_value" -le 65535 ] \
    || { eval "echo \"$port_name holds '\$$port_name', which is not a port in 1-65535\" >&2"; exit 2; }
  eval "$port_name=$port_value"
done
[ "$PROBE_PORT" -ne "$PROBE_PORT_ALT" ] \
  || { echo "PROBE_PORT and PROBE_PORT_ALT must be different ports, or SAME_ADDRESS is the control" >&2; exit 2; }
# A PRE-CHECK ON THE NAMES; the check that holds is on the resolved
# addresses, below, because `podman inspect` takes a name, a full ID or
# a short ID for the same container, and two spellings of one container
# pass a string comparison. The port half of this guard was fixed for the
# same reason one commit earlier, and the address half kept comparing
# spellings with the resolved value one variable away. Review finding on
# PR #81.
[ "$ALT_SOURCE" != "$PROBER" ] \
  || { echo "ALT_SOURCE and PROBER must differ, or OTHER_ADDRESS is the same address" >&2; exit 2; }

addr_on() {
  podman inspect "$1" --format "{{ (index .NetworkSettings.Networks \"$2\").IPAddress }}"
}

prober=$(addr_on "$PROBER" "$NET_PUB")
alt=$(addr_on "$ALT_SOURCE" "$NET_PUB")
peer_private=$(addr_on "$PEER" "$LAN")
router_pub=$(addr_on "$ROUTER" "$NET_PUB")
for pair in "prober:$prober" "alt:$alt" "peer:$peer_private" "router:$router_pub"; do
  [ -n "${pair#*:}" ] || { echo "no address for ${pair%%:*}" >&2; exit 1; }
done
# THE CHECK THE NAME COMPARISON ABOVE STANDS IN FOR. Two references to
# one container resolve to one address, and OTHER_ADDRESS sent from the
# prober's own address matches the address-restricted forward -- so
# `adf` reads as `eif`, and on a `full-cone` row a broken observer reads
# as a passing control.
[ "$alt" != "$prober" ] \
  || { echo "ALT_SOURCE ($ALT_SOURCE) and PROBER ($PROBER) resolve to the same address $alt, so OTHER_ADDRESS is not a different address" >&2; exit 2; }

# LEARN THE MAPPED PORT FIRST, because under `eds` it is not the bound
# one and the three probes have to be aimed somewhere real. The prober
# listens only for this, and the listener is stopped before the control
# is sent -- the control must come FROM the port the peer addressed, and
# a listener holding it makes that bind fail.
podman exec "$PROBER" sh -c ': > /seen.txt'
podman exec -d "$PROBER" sh -c \
  "socat -u UDP-RECVFROM:$PROBE_PORT,fork SYSTEM:'echo \$SOCAT_PEERADDR \$SOCAT_PEERPORT >> /seen.txt'" \
  >/dev/null
waited=0
while ! podman exec "$PROBER" ss -uln 2>/dev/null | grep -q ":$PROBE_PORT "; do
  waited=$((waited + 1))
  [ "$waited" -le 100 ] \
    || { echo "$PROBER: never bound UDP $PROBE_PORT" >&2; exit 1; }
  sleep 0.1
done

podman exec "$PEER" sh -c \
  "echo learn | socat -t1 - UDP-DATAGRAM:$prober:$PROBE_PORT,bind=:$SRC_PORT,reuseaddr" \
  >/dev/null 2>&1 || true
seen=""
attempt=0
while [ -z "$seen" ]; do
  seen=$(podman exec "$PROBER" sh -c 'cat /seen.txt' | tail -1)
  [ -n "$seen" ] && break
  attempt=$((attempt + 1))
  [ "$attempt" -le 50 ] || break
  sleep 0.1
done
[ -n "$seen" ] || {
  echo "VERDICT: NO MAPPING — the prober saw nothing, so there is nothing to filter" >&2
  exit 1
}
seen_ip=${seen% *}
mapped=${seen##* }
{
  printf 'peer private   : %s %s\n' "$peer_private" "$SRC_PORT"
  printf 'prober saw     : %s %s\n' "$seen_ip" "$mapped"
} >&2
[ "$seen_ip" = "$router_pub" ] || {
  echo "VERDICT: TRANSLATED BY SOMETHING ELSE — expected $router_pub, saw $seen_ip" >&2
  exit 1
}
case "$mapped" in
  ''|*[!0-9]*) echo "VERDICT: MALFORMED — mapped port was '$mapped'" >&2; exit 1 ;;
esac

# THE LISTENER GOES AWAY so its port can be a SOURCE.
podman exec "$PROBER" sh -c 'pkill socat' >/dev/null 2>&1 || true
waited=0
while podman exec "$PROBER" ss -uln 2>/dev/null | grep -q ":$PROBE_PORT "; do
  waited=$((waited + 1))
  [ "$waited" -le 100 ] \
    || { echo "$PROBER: UDP $PROBE_PORT never freed" >&2; exit 1; }
  sleep 0.1
done

# The peer reopens the socket and holds it while the three probes land.
podman exec "$PEER" sh -c ': > /filtered.txt'
podman exec -d "$PEER" sh -c \
  "echo hold | socat -t$HOLD_SECONDS - UDP-DATAGRAM:$prober:$PROBE_PORT,bind=:$SRC_PORT,reuseaddr > /filtered.txt 2>&1" \
  >/dev/null
# ASSERTED, NOT SLEPT, which is this directory's rule and was a `sleep 1`
# standing in for it: the socket has to be bound before a probe is aimed
# at the mapping, or the probe races the bind.
waited=0
while ! podman exec "$PEER" ss -uln 2>/dev/null | grep -q ":$SRC_PORT "; do
  waited=$((waited + 1))
  [ "$waited" -le 100 ] \
    || { echo "$PEER: the holding socket never bound UDP $SRC_PORT" >&2; exit 1; }
  sleep 0.1
done

# EVERY SEND'S STATUS IS KEPT, not discarded.
#
# `|| true` on all three was the first version, and it reproduced the
# exact defect the CONTROL exists to prevent -- for the two probes that
# actually discriminate. A `socat` that cannot bind, or an `ALT_SOURCE`
# container that has exited, produces the same observable as a NAT that
# refused the packet: nothing arrives, the classifier falls through to
# `apdf`, and `apdf` is what the default row EXPECTS. A probe that never
# left would have reported a clean pass. The control closes this for its
# own socket only, and says nothing about the other containers at all.
# Review finding on PR #81.
sent=""
failed=""
send_from() {
  local container="$1" port="$2" tag="$3"
  if podman exec "$container" sh -c \
    "echo $tag | socat -t1 - UDP-DATAGRAM:$router_pub:$mapped,bind=:$port,reuseaddr" \
    >/dev/null 2>&1; then
    sent="${sent:+$sent,}$tag"
  else
    failed="${failed:+$failed,}$tag(from $container:$port)"
  fi
}
send_from "$PROBER" "$PROBE_PORT" CONTROL
send_from "$PROBER" "$PROBE_PORT_ALT" SAME_ADDRESS
send_from "$ALT_SOURCE" "$ALT_SOURCE_PORT" OTHER_ADDRESS
[ -z "$failed" ] || {
  echo "VERDICT: NOT SENT — $failed could not be sent, so an absence below would not be a refusal" >&2
  exit 1
}

# STILL OPEN AFTER THE SENDS, or the window closed under them. Three
# `podman exec` round trips plus socat's own half-close wait fit inside
# `HOLD_SECONDS` comfortably on an idle host and not necessarily on a
# loaded one -- and a probe arriving after the socket closed is absent
# for a reason that is not filtering. Checked rather than budgeted for,
# since this directory's own rule is asserted-not-slept.
# Review finding on PR #81.
podman exec "$PEER" sh -c 'pgrep socat >/dev/null' \
  || { echo "VERDICT: WINDOW CLOSED — the holding socket exited before the probes landed; raise HOLD_SECONDS" >&2; exit 1; }

# Wait for the socket to close rather than sleeping past it.
waited=0
while podman exec "$PEER" sh -c 'pgrep socat' >/dev/null 2>&1; do
  waited=$((waited + 1))
  [ "$waited" -le $((HOLD_SECONDS * 20 + 40)) ] \
    || { echo "$PEER: the holding socket never closed" >&2; exit 1; }
  sleep 0.1
done
arrived=$(podman exec "$PEER" sh -c 'cat /filtered.txt')
{
  printf 'peer received  : %s\n' "$(echo "$arrived" | tr '\n' ' ' | sed 's/ *$//')"
} >&2

have() { printf '%s' "$arrived" | grep -q "^$1$"; }

# THE CONTROL DECIDES WHETHER ANYTHING BELOW IS A MEASUREMENT. Without
# it, "nothing arrived" reads as strict filtering when it is equally
# consistent with a probe that never left -- which is exactly what
# happened while an observer's own listener was silently refusing the
# bind.
have CONTROL || {
  echo "VERDICT: NO CONTROL — the endpoint the peer addressed could not reach it, so nothing here is measured" >&2
  exit 1
}

# A DIFFERENT ADDRESS ADMITTED WHILE THE SAME ONE IS NOT is no RFC 4787
# class, and reporting it as `eif` would be the upward misclassification
# this script's guards exist to prevent -- the permissive answer for an
# observation that supports none. Review finding on PR #81.
if have OTHER_ADDRESS && ! have SAME_ADDRESS; then
  echo "VERDICT: ANOMALOUS — a different address was admitted while the same address was not; that is no filtering class" >&2
  exit 1
fi

if have OTHER_ADDRESS; then
  verdict="ENDPOINT-INDEPENDENT FILTERING (a full cone: any source reaches the mapping)"
  class=eif
elif have SAME_ADDRESS; then
  verdict="ADDRESS-DEPENDENT FILTERING (an address-restricted cone: the address must match, the port need not)"
  class=adf
else
  verdict="ADDRESS-AND-PORT-DEPENDENT FILTERING (a port-restricted cone: only the addressed endpoint reaches the mapping)"
  class=apdf
fi
printf 'VERDICT: %s\n' "$verdict" >&2

if [ -z "${EXPECT_FILTER:-}" ]; then
  echo "UNASSERTED: no EXPECT_FILTER given, so the class above was reported and not checked" >&2
elif [ "$EXPECT_FILTER" != "$class" ]; then
  echo "MISMATCH: expected filtering $EXPECT_FILTER and measured $class" >&2
  exit 1
fi

printf 'FILTER=%s\n' "$class"
printf 'ADMITTED=%s\n' "$(echo "$arrived" | tr '\n' ',' | sed 's/,*$//')"
printf 'SENT=%s\n' "$sent"
