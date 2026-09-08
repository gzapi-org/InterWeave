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

# THE ROUTER LIST, IN ONE PLACE. `record_environment` and `down` read it
# from here instead of each carrying their own copy, so adding a third
# NAT domain means remembering one site rather than two.
#
# THAT IS ALL IT IS. It is not a check and catches nothing: `up()` still
# names each router at its `podman run`, and nothing verifies that those
# and this list agree. A third router created in `up()` and forgotten
# here goes unrecorded and unremoved, and the run still passes -- as it
# also would for its peer and its LAN, which this does not reach at all.
# The previous version of this comment said a single site was what
# catches that. Review finding on PR #78.
ROUTERS="natm-router natm-router-b"
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
  await_listener natm-obs1
  await_listener natm-obs2

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
  # shellcheck disable=SC2086
  record_environment $ROUTERS
}

# Wait until an observer is actually listening on UDP 9000.
#
# `podman run -d` reports the CONTAINER started, not the application
# ready: the shell may not have execed socat yet, and the port is not
# bound until it has. `probe.sh` sends each datagram exactly once, and
# UDP drops what arrives at a closed port silently -- so a slow start
# produced a row that failed `NO DATA` with the topology perfectly
# correct. Codex review on PR #78.
#
# ASSERTED, NOT SLEPT. A fixed wait long enough to be safe on a loaded
# host is dead time on every run, and one short enough to be tolerable
# is the same race with a smaller window. This polls the listener the
# container actually holds and fails closed when it never appears.
await_listener() {
  local ctr="$1" waited=0
  while ! podman exec "$ctr" ss -uln 2>/dev/null | grep -q ':9000 '; do
    waited=$((waited + 1))
    # BOUNDED IN ATTEMPTS, not seconds: each one is a `podman exec`,
    # which costs far more than the sleep beside it, so a deadline in
    # wall-clock would be a deadline on podman rather than on socat.
    [ "$waited" -le 100 ] \
      || { echo "$ctr: socat never bound UDP 9000 (100 attempts)" >&2; return 1; }
    sleep 0.1
  done
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
# THE ROUTERS ARE ARGUMENTS, and EVERY router that translated must be
# named. It reads `nft --version` from each one rather than from a name
# written here -- the same hardcoding that made `configure_nat` build
# router B's rule from router A's interface, sitting inside the function
# whose whole job is attribution. Moving that name from the body to the
# call site fixed one caller and left the function able to record half a
# topology and return 0, which is exactly the unattributable run it
# exists to prevent.
#
# THE COUNT GUARD IS A FLOOR AND NOTHING MORE. It catches that
# regression -- one router named, or none -- and it cannot catch a third
# domain added without updating the caller, because two arguments still
# satisfy it. Nothing here catches that; `$ROUTERS` only reduces the
# number of places to remember. Saying the guard caught it was this file
# claiming a check it does not perform, one paragraph after deleting
# three guards for being unable to fail. Review finding on PR #78.
#
# WHAT MAKES THIS FAIL CLOSED IS THE ASSIGNMENT, not a non-empty check.
# A command substitution inside a `printf` argument does not fire
# `set -e`: the status that counts is `printf`'s, so a failing
# `podman exec` logged `nft    : ` and returned 0 from the last
# statement of `up()`. Hoisting it into an assignment is the fix. A
# `[ -n ... ]` guard on `uname -r`, `podman --version` or `nft
# --version` would be an assertion that cannot fail -- none of the three
# exits 0 while printing nothing -- so there is none. The socat line
# below IS guarded, because it filters its output and a filter that
# matches nothing succeeds. Review findings on PR #78.
record_environment() {
  [ "$#" -ge 2 ] \
    || { echo "record_environment: name every router that translated" >&2; return 1; }
  local kernel podman_version ctr nft_version
  kernel=$(uname -r)
  podman_version=$(podman --version)
  log "kernel : $kernel"
  log "podman : $podman_version"
  for ctr in "$@"; do
    nft_version=$(podman exec "$ctr" nft --version)
    log "nft    : $nft_version ($ctr)"
  done
  # SOCAT TOO, because it is what produces the measurement, and it is
  # read from an OBSERVER rather than from a router -- the one name
  # written into this function rather than passed to it, because the
  # observers are not per-domain. The log line says which.
  #
  # RECORDED EVEN THOUGH THE CONTAINERFILE NOW PINS IT. The pin says
  # what should be installed; this says what was. They agreeing is the
  # useful fact, and it is only checkable if both exist.
  #
  # GUARDED, UNLIKE THE THREE ABOVE, and the difference is the `sed`.
  # `podman exec` failing is caught by the assignment, as it is for the
  # others -- but `sed -n '2p'` exits 0 printing NOTHING whenever socat's
  # banner has fewer than two lines or changes shape, which would log
  # `socat  : ` and return 0 from `up()`. That is the record-half-a-
  # topology failure this function was rewritten to close, in a narrower
  # form. Review finding on PR #78.
  local socat_version
  socat_version=$(podman exec natm-obs1 socat -V | sed -n 's/^socat version /&/p')
  [ -n "$socat_version" ] \
    || { echo "natm-obs1: socat printed no version line" >&2; return 1; }
  log "socat  : $socat_version (natm-obs1)"
}

configure_nat() {
  local ctr="$1" net="$2" rule
  case "$NAT_MODE" in
    eim)
      # ENDPOINT-INDEPENDENT MAPPING: one external port for every
      # destination, which is the case a hole punch is designed to work
      # through. Plain `masquerade` PRESERVES the source port when it is
      # free rather than sharing one mapping by design, and those
      # coincide under this harness's conditions -- one peer, one bound
      # port at a time. `probe.sh` decides the class by observing it,
      # which is why this comment does not have to be a claim about
      # kernel behaviour.
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
  # given a rule naming router A's interface. The names are neither
  # reliably the same across the two routers nor stable across
  # recreation -- observed across runs, and within one run between its
  # two rows, since every row recreates every container. That is the
  # observation; an explanation resting on the routers being attached to
  # different pairs of networks stood here for a round and would predict
  # a stable answer the runs contradict.
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
  #
  # DECLARED AND FLUSHED FIRST, so the TABLE holds what this heredoc
  # says -- `flush table inet nat` is table-scoped, and calling that the
  # ruleset was wrong by one level. "Exactly" would be a claim the
  # assertion below does not make: it reads one chain and only that
  # chain's `oifname` rules, so a second chain added to this heredoc
  # would go unchecked. `nft -f -` ADDS: run twice on one container with
  # different modes, the table would carry both rules, the whole-line
  # assertion below would find the one it asked for, and the earlier
  # rule would win at runtime. Not reachable while every row recreates
  # the containers -- this is for whoever adds an in-place reconfigure.
  # `table inet nat` on its own line creates it when absent, so the
  # flush cannot fail on a fresh container.
  podman exec -i "$ctr" nft -f - <<NFT
table inet nat
flush table inet nat
table inet nat {
  chain postrouting {
    type nat hook postrouting priority srcnat; policy accept;
    oifname "$oif" $rule
  }
}
NFT
  # ASSERT THE RULE LANDED, AND THAT IT IS THE ONLY `oifname` RULE. A topology
  # harness that cannot tell whether it built the topology reports
  # loopback-quality evidence under a phase-B heading, which is the one
  # failure this spike exists to avoid -- and the first version of this
  # script did exactly that.
  #
  # THE CHAIN'S RULES, COMPARED TO ONE LINE, rather than a `grep` over
  # the ruleset. The `sed` keeps only lines beginning `oifname`, so the
  # chain's own declaration -- `type nat hook postrouting priority
  # srcnat; policy accept;` -- is NOT compared, and neither would a rule
  # written some other way be. That is worth knowing before trusting
  # this to catch a hook or policy change: it would not. Three things it
  # does check, none of which held before:
  #
  #   * the MODE is checked, not a prefix of it. `grep -q 'oifname
  #     "eth0" masquerade'` matches the line `oifname "eth0" masquerade
  #     random`, so an `eim` assertion was satisfied by an `eds`
  #     ruleset.
  #   * the FLUSH above is checked. `nft -f -` adds; without the flush a
  #     second run with a different mode leaves both rules and a `grep`
  #     still finds the one it asked for, while the earlier rule wins at
  #     runtime. Comparing the whole chain to one line is what makes
  #     deleting the flush fail this assertion instead of passing it.
  #   * the SCOPE is no longer wider than the thing being asserted.
  #     `nft list ruleset` spans every table, so a matching line in any
  #     other one used to satisfy the check. The read-back is now
  #     narrower than the flush above it rather than wider -- chain
  #     against table -- which is the safe direction.
  #
  # `nft`'s own failure is also distinguished from an absent rule: the
  # old form discarded stderr and reported "rule absent" for a chain it
  # could not read. Review findings on PR #78.
  local installed
  installed=$(podman exec "$ctr" nft list chain inet nat postrouting) \
    || { echo "$ctr: could not read back the nat chain" >&2; return 1; }
  installed=$(printf '%s\n' "$installed" | sed -n 's/^[[:space:]]*\(oifname .*\)$/\1/p')
  [ "$installed" = "oifname \"$oif\" $rule" ] \
    || { echo "$ctr: nat chain holds [$installed], expected [oifname \"$oif\" $rule]" >&2; return 1; }
  log "$ctr: snat on $oif using: $rule"
}

down() {
  # shellcheck disable=SC2086
  podman rm -f natm-obs1 natm-obs2 natm-peer natm-peer-b $ROUTERS \
    >/dev/null 2>&1 || true
  podman network rm -f "$NET_PUB" "$NET_LAN" "$NET_LAN_B" >/dev/null 2>&1 || true
}

case "${1:-up}" in
  up) up ;;
  down) down ;;
  *) echo "usage: $0 [up|down]" >&2; exit 2 ;;
esac
