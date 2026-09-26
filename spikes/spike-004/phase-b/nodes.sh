#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# SPIKE-004 phase B's NODE rows: the shipping substrate (`node/`, pinned
# by revision) in containers on the NAT matrix `topology.sh` builds.
#
#   NODE_BIN=<node built from node/ at its pin> WORK=<scratch dir> \
#     ./nodes.sh <row>        # row: services | loss | capacity | ifchange | punch | cost | all
#
# `topology.sh` builds and tears the matrix down for each row, with the
# public side on a routable-looking range (PUB_SUBNET, below): AutoNAT
# probes only an address `is_probeable_address` calls public, and a relay
# hands out a reservation's addresses only once AutoNAT has verified one
# -- until then it accepts reservations that carry none, and the client
# refuses every one of them (`libp2p-relay` 0.22.0
# `NoAddressesInReservation`). So every row starts from two relays that
# verify EACH OTHER, which is also item 1's own claim.
#
# EVERY ROW ASSERTS, AND EVERY ASSERTION HAS A CONTROL: an expected event
# that never comes fails the row with the tail of the log that did not
# carry it, and each row's claim is set against a run where it must NOT
# hold. A row prints `ROW <name>: PASS` last; anything else is a failure.
#
# What the rows are, and the limits they carry, are README.md's
# "The node rows"; this file does not restate them.
set -euo pipefail
cd "$(dirname "$0")"

NODE_BIN="${NODE_BIN:?NODE_BIN: the node built from node/ at its pin (cargo build --release --locked)}"
WORK="${WORK:?WORK: a scratch directory outside the tree, for keys and logs}"
IMG_NODE="interweave-phaseb-node:1"
# 11.0.0.0/24: public by `is_public_v4`'s rules, and routed nowhere on the
# host -- rootless podman networks live in their own namespace.
export PUB_SUBNET="${PUB_SUBNET:-11.0.0.0/24}"
# How long a row waits for an event before failing. Generous, because the
# relays' mutual AutoNAT verification alone took 35-40 s on this host.
PATIENCE="${PATIENCE:-150}"

log() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

NODES=()

record() {
  local sha
  sha=$(sha256sum "$NODE_BIN" | awk '{print $1}')
  log "  node   : sha256 $sha"
  log "  pin    : $(sed -n 's/^interweave-transport-libp2p = .*rev = "\([0-9a-f]*\)".*/\1/p' node/Cargo.toml)"
  log "  image  : $(podman image inspect "$IMG_NODE" --format '{{.Id}}')"
}

ip_on() { podman inspect "$1" --format "{{(index .NetworkSettings.Networks \"$2\").IPAddress}}"; }

# A container for one node, on `net`; behind `router` when one is named,
# with its only default route through it, as the matrix's peers have.
node_ctr() {
  local name="$1" net="$2" router="${3:-}"
  podman rm -f "$name" >/dev/null 2>&1 || true
  # `:z` BECAUSE SELINUX ENFORCES on this host: without the relabel the
  # container cannot read the binary or the keys, `podman exec -d` starts
  # a shell that fails where nobody looks, and a row waits for a log that
  # is never written.
  podman run -d --name "$name" --network "$net" --cap-add NET_ADMIN \
    -v "$NODE_BIN:/node:ro,z" -v "$WORK/keys:/keys:ro,z" -v "$WORK/out:/out:z" \
    --entrypoint sleep "$IMG_NODE" infinity >/dev/null
  NODES+=("$name")
  if [ -n "$router" ]; then
    local gw got
    gw=$(ip_on "$router" "$net")
    podman exec "$name" sh -c "while ip route del default 2>/dev/null; do :; done; ip route add default via $gw"
    got=$(podman exec "$name" sh -c "ip route show default | awk '{print \$3}'")
    [ "$got" = "$gw" ] || fail "$name: default route via $got, expected $gw"
  fi
}

keygen() { "$NODE_BIN" keygen "$WORK/keys/$1.id"; }

# Start `/node run` in container `ctr` as log `name`, with the given flags.
node_run() {
  local ctr="$1" name="$2"; shift 2
  : > "$WORK/out/$name.log"
  podman exec -d "$ctr" sh -c "/node run --identity /keys/$name.id $* > /out/$name.log 2>&1" >/dev/null
}

# Wait until `name`'s log carries a line matching the extended regex.
await() {
  local name="$1" pattern="$2" what="$3" waited=0
  until grep -Eq -- "$pattern" "$WORK/out/$name.log" 2>/dev/null; do
    waited=$((waited + 1))
    if [ "$waited" -gt $((PATIENCE * 2)) ]; then
      tail -n 15 "$WORK/out/$name.log" >&2 || true
      fail "$what: $name never logged /$pattern/ in ${PATIENCE}s"
    fi
    sleep 0.5
  done
  log "  ok     : $what"
}

# Wait until ANY of the named logs carries a line matching the pattern.
await_any() {
  local pattern="$1" what="$2" waited=0 name; shift 2
  while :; do
    for name in "$@"; do
      if grep -Eq -- "$pattern" "$WORK/out/$name.log" 2>/dev/null; then
        log "  ok     : $what ($name)"
        return 0
      fi
    done
    waited=$((waited + 1))
    [ "$waited" -le $((PATIENCE * 2)) ] || fail "$what: none of $* logged /$pattern/ in ${PATIENCE}s"
    sleep 0.5
  done
}

# Assert `name`'s log carries NO line matching the pattern.
absent() {
  local name="$1" pattern="$2" what="$3"
  ! grep -Eq -- "$pattern" "$WORK/out/$name.log" || {
    grep -E -- "$pattern" "$WORK/out/$name.log" | head -3 >&2
    fail "$what: $name logged /$pattern/"
  }
  log "  ok     : $what"
}

teardown() {
  [ "${#NODES[@]}" -eq 0 ] || podman rm -f "${NODES[@]}" >/dev/null 2>&1 || true
  NODES=()
  ./topology.sh down
}

fresh() {
  teardown
  rm -rf "$WORK/keys" "$WORK/out"
  mkdir -p "$WORK"
  mkdir -m 700 "$WORK/keys"
  mkdir -p "$WORK/out"
  NAT_MODE="${NAT_MODE:-eim}" ./topology.sh up
}

# The two relays, each an AutoNAT server and each probing the other, so
# each ends VerifiedPublic and hands out reservations that carry an
# address. `relay_flags` adds to both. Sets R1, R2, A1, A2.
relays() {
  local relay_flags="${1:-}" trust="${2:-}"
  R1=$(keygen r1); R2=$(keygen r2)
  node_ctr natm-node-r1 natm-pub
  node_ctr natm-node-r2 natm-pub
  A1=$(ip_on natm-node-r1 natm-pub); A2=$(ip_on natm-node-r2 natm-pub)
  node_run natm-node-r1 r1 --listen /ip4/0.0.0.0/tcp/4001 --relay-server --autonat-server \
    --autonat "$R2@/ip4/$A2/tcp/4001" --distinct 1 --infra "$R2" $trust $relay_flags
  node_run natm-node-r2 r2 --listen /ip4/0.0.0.0/tcp/4001 --relay-server --autonat-server \
    --autonat "$R1@/ip4/$A1/tcp/4001" --distinct 1 --infra "$R1" $trust $relay_flags
}

verified() {
  await r1 "ConnectivityChanged \{ direct_inbound: VerifiedPublic, verified_addresses: \[\"/ip4/$A1/tcp/4001\"\]" \
    "r1 verified public at its own address, by r2's probe"
  await r2 "ConnectivityChanged \{ direct_inbound: VerifiedPublic, verified_addresses: \[\"/ip4/$A2/tcp/4001\"\]" \
    "r2 verified public at its own address, by r1's probe"
}

# ITEM 1: two relay and probe services. The client behind router A
# reserves on both and asks both to probe it.
row_services() {
  log "== row services: two relay + probe services =="
  fresh
  record
  local c
  c=$(keygen c)
  relays "" "--infra $c"
  node_ctr natm-node-c natm-lan natm-router
  node_run natm-node-c c --listen /ip4/0.0.0.0/tcp/4001 \
    --relay "$R1@/ip4/$A1/tcp/4001" --relay "$R2@/ip4/$A2/tcp/4001" \
    --autonat "$R1@/ip4/$A1/tcp/4001" --autonat "$R2@/ip4/$A2/tcp/4001" \
    --distinct 2 --infra "$R1" --infra "$R2"
  verified
  # THE CONTROL FOR THE PROBES: servers that reached each other (above)
  # dial the client back and are refused by its NAT -- so the client's
  # verdict below is the NAT's, not a server that can reach nobody. ONE
  # server, not both: the client's crate picks one of its dialled servers
  # at random per probe (resource-limits.md's AutoNAT rows), so which of
  # the two serves it in a window is chance, measured once as r2 twice
  # and r1 never. That both SERVE is the mutual verification above.
  local pub_a
  pub_a=$(ip_on natm-router natm-pub)
  await_any "AutonatProbeServed \{ client: TransportIdentity\(\"$c\"\), address: \"/ip4/$pub_a/tcp/[0-9]+\", reached: false" \
    "a relay probed the client at router A's public address and was not let through" r1 r2
  await c "RelayReservationChanged \{ relay: TransportIdentity\(\"$R1\"\), outcome: Accepted, addresses: \[\"/ip4/$A1/tcp/4001/p2p/$R1/p2p-circuit/p2p/$c\"\]" \
    "a reservation on r1, carrying r1's verified address"
  await c "RelayReservationChanged \{ relay: TransportIdentity\(\"$R2\"\), outcome: Accepted, addresses: \[\"/ip4/$A2/tcp/4001/p2p/$R2/p2p-circuit/p2p/$c\"\]" \
    "a reservation on r2, carrying r2's"
  await c "RelayStandingChanged \{ standing: Satisfied, active: 2, target: 2" \
    "the client's standing is satisfied, two of two"
  absent c "direct_inbound: VerifiedPublic" \
    "the client behind the NAT is never verified public"
  log "ROW services: PASS"
}

main() {
  # REBUILT EVERY RUN, as `run.sh` rebuilds the matrix image: a stale
  # local tag would otherwise be what is measured.
  podman build -q -t interweave-natmatrix:1 -f Containerfile . >/dev/null
  podman build -q -t "$IMG_NODE" -f Containerfile.node . >/dev/null
  case "${1:-}" in
    services) row_services ;;
    all) row_services ;;
    *) echo "usage: $0 services|all" >&2; exit 2 ;;
  esac
  teardown
}

trap 'teardown >/dev/null 2>&1 || true' EXIT
main "$@"
