#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Build the NAT topology SPIKE-004 phase B needs, using rootless podman.
#
# See README.md for what this does and does not establish. The short
# version: it builds a real kernel NAT whose MAPPING BEHAVIOUR is chosen
# rather than inherited, because that behaviour is what decides whether
# a hole punch can succeed — and phase A, on loopback, had no NAT at all.
set -euo pipefail

NET_PUB="${NET_PUB:-natm-pub}"
NET_LAN="${NET_LAN:-natm-lan}"
# THE SECOND NAT DOMAIN. A hole punch needs two peers each behind their
# OWN translation: one domain measures a mapping, two are required
# before a punch is a thing that can be attempted at all. The first
# version of this harness built one and described itself as the
# environment a punch needs, which was false. Review finding on PR #78.
NET_LAN_B="${NET_LAN_B:-natm-lan-b}"
IMAGE="${IMAGE:-interweave-natmatrix:1}"

# The NAT class to build. `eim` gives one external port per internal
# socket whatever the destination; `eds` allocates per destination.
# Those are the two rows that decide a hole punch, which is why they are
# the two this harness builds.
NAT_MODE="${NAT_MODE:-eim}"

log() { printf '  %s\n' "$*" >&2; }

up() {
  down >/dev/null 2>&1 || true
  podman network create "$NET_PUB" >/dev/null
  podman network create "$NET_LAN" >/dev/null
  podman network create "$NET_LAN_B" >/dev/null

  # THE OBSERVERS ARE TWO, and that is the measurement rather than
  # redundancy: one observer cannot tell an endpoint-independent mapping
  # from a per-destination one, because there is nothing to compare the
  # observed port against.
  for n in 1 2; do
    podman run -d --name "natm-obs$n" --network "$NET_PUB" \
      --entrypoint /bin/sh "$IMAGE" -c \
      "socat -u UDP-RECVFROM:9000,fork SYSTEM:'echo \$SOCAT_PEERADDR \$SOCAT_PEERPORT >> /seen.txt'" >/dev/null
  done

  podman run -d --name natm-router --network "$NET_PUB" --network "$NET_LAN" \
    --cap-add=NET_ADMIN --sysctl net.ipv4.ip_forward=1 \
    --entrypoint /bin/sh "$IMAGE" -c 'sleep infinity' >/dev/null

  podman run -d --name natm-peer --network "$NET_LAN" --cap-add=NET_ADMIN \
    --entrypoint /bin/sh "$IMAGE" -c 'sleep infinity' >/dev/null

  podman run -d --name natm-router-b --network "$NET_PUB" --network "$NET_LAN_B" \
    --cap-add=NET_ADMIN --sysctl net.ipv4.ip_forward=1 \
    --entrypoint /bin/sh "$IMAGE" -c 'sleep infinity' >/dev/null

  podman run -d --name natm-peer-b --network "$NET_LAN_B" --cap-add=NET_ADMIN \
    --entrypoint /bin/sh "$IMAGE" -c 'sleep infinity' >/dev/null

  # Addresses are read back rather than assumed: podman picks the
  # subnets, and a hardcoded guess here would fail as a NAT behaviour
  # instead of as a setup error.
  local router_lan router_pub
  router_lan=$(addr_on natm-router "$NET_LAN")
  router_pub=$(addr_on natm-router "$NET_PUB")
  log "router-a lan=$router_lan pub=$router_pub"

  # The peer's default route goes THROUGH the router, or nothing is
  # translated and every measurement below is of a direct path.
  #
  # The podman-supplied default is DELETED first. `ip route replace`
  # left both in place -- they differ by metric, so the kernel treats
  # them as distinct routes -- and which one wins then depends on a
  # metric this script never set.
  route_through natm-peer "$router_lan"

  configure_nat natm-router "$NET_PUB"

  local router_b_lan router_b_pub
  router_b_lan=$(addr_on natm-router-b "$NET_LAN_B")
  router_b_pub=$(addr_on natm-router-b "$NET_PUB")
  log "router-b lan=$router_b_lan pub=$router_b_pub"
  route_through natm-peer-b "$router_b_lan"
  configure_nat natm-router-b "$NET_PUB"

  log "NAT mode: $NAT_MODE (both domains)"
  record_environment natm-router natm-router-b
}

# The interface podman gave this container on that network.
#
# Derived by matching the ADDRESS rather than read from a field:
# `podman inspect`'s per-network object carries no interface name in
# 5.8, and asking for one yields an empty string that nft then accepts
# as `oifname ""` -- a rule matching nothing, installed without
# complaint. That is how the first version of this script reported a
# NAT it had not built.
iface_on() {
  local ctr="$1" net="$2" ip iface
  ip=$(addr_on "$ctr" "$net")
  [ -n "$ip" ] || { echo "no address for $ctr on $net" >&2; return 1; }
  iface=$(podman exec "$ctr" ip -o -4 addr show \
    | awk -v pfx="$ip/" '$4 ~ "^" pfx {print $2; exit}')
  [ -n "$iface" ] || { echo "no interface carrying $ip in $ctr" >&2; return 1; }
  printf '%s' "$iface"
}

addr_on() {
  local ctr="$1" net="$2"
  podman inspect "$ctr" --format "{{ (index .NetworkSettings.Networks \"$net\").IPAddress }}"
}

# Point a peer's only default route at its router.
#
# Asserted by GATEWAY, not by count: one default route pointing at
# podman's own gateway would satisfy a count check and translate
# nothing.
route_through() {
  local ctr="$1" gw="$2"
  podman exec "$ctr" sh -c \
    'while ip route del default 2>/dev/null; do :; done; true'
  podman exec "$ctr" ip route add default via "$gw"
  local got
  got=$(podman exec "$ctr" sh -c "ip route show default | awk '{print \$3}'" | tr -d '\r')
  [ "$got" = "$gw" ] || { echo "$ctr default route is via $got, expected $gw" >&2; return 1; }
}

# What performed the NAT, recorded because the harness cannot attribute
# a measurement without it.
#
# The IMAGE is digest-pinned, and the image is not what translates: the
# NAT is the host kernel's netfilter, and the eim/eds distinction is a
# `get_unique_tuple` port-selection behaviour that has changed across
# kernel releases. Pinning the image and not recording the kernel aimed
# the reproducibility argument at the wrong component. Review finding on
# PR #78.
#
# THE CONTAINER IS AN ARGUMENT, and it is called for BOTH routers. It
# reads `nft --version` from whichever router it is given rather than
# from a name written here -- the same hardcoding that made
# `configure_nat` build router B's rule from router A's interface,
# sitting inside the function whose whole job is attribution. The two
# routers run the one digest-pinned image, so the versions agree by
# construction; reading both is what makes that a measurement rather
# than an assumption. Review finding on PR #78.
#
# AND IT FAILS CLOSED. A command substitution inside an argument does
# not fire `set -e`: the status that counts is `printf`'s, so a failing
# `podman exec` used to log `nft    : ` and return 0 from the last
# statement of `up()`. An empty value here is exactly the unattributable
# run this function exists to prevent, and it was silent. Same hazard
# `configure_nat` hoists `oif` out of a heredoc to avoid.
record_environment() {
  local kernel podman_version
  kernel=$(uname -r)
  podman_version=$(podman --version)
  [ -n "$kernel" ] && [ -n "$podman_version" ] \
    || { echo "could not record the host that performed the NAT" >&2; return 1; }
  log "kernel : $kernel"
  log "podman : $podman_version"
  local ctr
  for ctr in "$@"; do
    local nft_version
    nft_version=$(podman exec "$ctr" nft --version)
    [ -n "$nft_version" ] \
      || { echo "$ctr: could not record its nft version" >&2; return 1; }
    log "nft    : $nft_version ($ctr)"
  done
}

configure_nat() {
  local ctr="$1" net="$2" rule
  case "$NAT_MODE" in
    eim)
      # ENDPOINT-INDEPENDENT MAPPING. One external port per internal
      # socket, reused for every destination — the case a hole punch is
      # designed to work through, and the kernel's default behaviour for
      # masquerade when it is not asked for anything else.
      rule="masquerade"
      ;;
    eds)
      # ENDPOINT-DEPENDENT (symmetric). `random` forces a fresh port
      # allocation per flow, so the same internal socket appears on a
      # different external port to each destination. This is the row
      # where DCUtR must FAIL and fall back to the relay — the case
      # loopback could never produce, because on loopback every punch
      # succeeds.
      rule="masquerade random"
      ;;
    *) echo "unknown NAT_MODE: $NAT_MODE" >&2; exit 2 ;;
  esac
  local oif
  # OUTSIDE the heredoc, so a failure fails the script. Inside a command
  # substitution in a heredoc, `set -e` does not fire and the empty
  # result becomes a rule that matches nothing.
  #
  # `$ctr` AND `$net`, NOT THE NAMES THIS FUNCTION WAS WRITTEN FOR. When
  # the second NAT domain arrived, the function grew parameters and this
  # line kept reading `natm-router` and `$NET_PUB` -- so router B was
  # given a rule naming router A's interface. Podman numbers `ethN` by
  # walking each container's OWN network map, and the two routers are
  # attached to different pairs of networks, so nothing made the names
  # agree.
  #
  # THE ASSERTION BELOW COULD NOT HAVE CAUGHT IT: it greps `$ctr` for the
  # string this function just wrote into `$ctr`, so it passes whether or
  # not that interface exists there. `iface_on` is what actually checks,
  # because it fails closed when no interface in that container carries
  # that address. Review finding on PR #78.
  oif=$(iface_on "$ctr" "$net")
  # `-i`, or the heredoc goes nowhere: `podman exec` does not attach
  # stdin by default, so `nft -f -` reads EOF immediately and exits 0
  # having installed nothing. The assertion below is what turned that
  # into a visible failure rather than a topology that quietly was not
  # one.
  podman exec -i "$ctr" nft -f - <<NFT
table inet nat {
  chain postrouting {
    type nat hook postrouting priority srcnat; policy accept;
    oifname "$oif" $rule
  }
}
NFT
  # ASSERT THE RULE LANDED. A topology harness that cannot tell whether
  # it built the topology reports loopback-quality evidence under a
  # phase-B heading, which is the one failure this spike exists to
  # avoid -- and the first version of this script did exactly that.
  # The MODE as well as the interface: a bare `oifname "eth0"` with no
  # statement would satisfy a check on the interface alone, and the
  # comment above says the rule landed rather than half of it.
  #
  # WHOLE LINE, not substring. `grep -q "oifname \"eth0\" masquerade"`
  # matches the line `oifname "eth0" masquerade random`, so an `eim`
  # assertion was satisfied by an `eds` ruleset -- the comment claimed
  # the mode and checked a prefix of it. `-x` against the trimmed line
  # is what makes the two modes distinguishable here. Review finding on
  # PR #78.
  podman exec "$ctr" nft list ruleset 2>/dev/null \
    | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' \
    | grep -qx "oifname \"$oif\" $rule" \
    || { echo "$ctr: NAT rule absent or not exactly '$rule' after configuring it" >&2; return 1; }
  log "$ctr: snat on $oif using: $rule"
}

down() {
  podman rm -f natm-obs1 natm-obs2 natm-router natm-peer \
    natm-router-b natm-peer-b >/dev/null 2>&1 || true
  podman network rm -f "$NET_PUB" "$NET_LAN" "$NET_LAN_B" >/dev/null 2>&1 || true
}

case "${1:-up}" in
  up) up ;;
  down) down ;;
  *) echo "usage: $0 [up|down]" >&2; exit 2 ;;
esac
