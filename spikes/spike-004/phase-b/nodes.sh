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
  # A PUBLIC-SIDE node is outside every LAN (`isolate` says why); one
  # behind a router reaches the pool only as far as its own LAN, through
  # the router, which is isolated in `fresh`.
  [ -n "$router" ] || isolate "$name"
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

# Wait until `name`'s log carries a line matching the extended regex --
# from line `since` on, when given, so a claim about what happens AFTER
# an event is not met by a line from before it.
await() {
  local name="$1" pattern="$2" what="$3" since="${4:-1}" waited=0
  until tail -n "+$since" "$WORK/out/$name.log" 2>/dev/null | grep -Eq -- "$pattern"; do
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
# A name may carry `@<line>` to start its window there, as `await`'s
# `since` does.
await_any() {
  local pattern="$1" what="$2" waited=0 spec name since; shift 2
  while :; do
    for spec in "$@"; do
      name="${spec%@*}"
      since=1
      [ "$spec" = "$name" ] || since="${spec##*@}"
      if tail -n "+$since" "$WORK/out/$name.log" 2>/dev/null | grep -Eq -- "$pattern"; then
        log "  ok     : $what ($name)"
        return 0
      fi
    done
    waited=$((waited + 1))
    [ "$waited" -le $((PATIENCE * 2)) ] || fail "$what: none of $* logged /$pattern/ in ${PATIENCE}s"
    sleep 0.5
  done
}

# Assert `name`'s log carries NO line matching the pattern -- from line
# `since` on, when given: a row that asserts something did not happen
# AFTER an event needs its window to start there, or an earlier,
# unrelated line matches (the address-less reservations every client is
# refused before its relays are verified, the first time this was run).
absent() {
  local name="$1" pattern="$2" what="$3" since="${4:-1}"
  ! tail -n "+$since" "$WORK/out/$name.log" | grep -Eq -- "$pattern" || {
    tail -n "+$since" "$WORK/out/$name.log" | grep -E -- "$pattern" | head -3 >&2
    fail "$what: $name logged /$pattern/"
  }
  log "  ok     : $what"
}

# The next line `name`'s log will write: where an `absent` window starts.
mark() { echo $(($(wc -l < "$WORK/out/$1.log") + 1)); }

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
  isolate natm-router natm-router-b
  rfc5382 natm-router natm-router-b
}

# THE ROUTERS HOLD AN UNSOLICITED INBOUND SYN SILENTLY, as RFC 5382 REQ-4
# requires of a NAT ("MUST NOT respond to an unsolicited inbound SYN
# packet for at least 6 seconds"). A plain Linux router answers one from
# its own stack with a RST, since nothing listens on the port: a punch's
# SYN that reached a router before that router's own outbound SYN had
# made the mapping was refused, the refusal tore down the dialling socket,
# and every punch gave up after three attempts ("Connection refused (os
# error 111)", measured 2026-09-26). The requirement exists for exactly
# that: simultaneous open. Replies to translated traffic never reach this
# hook -- they are reverse-translated and forwarded -- so the rule changes
# only what the router itself would have answered. The NAT rows run
# without it: they send UDP, and REQ-4 is about TCP.
rfc5382() {
  local ctr oif
  for ctr in "$@"; do
    oif=$(podman exec "$ctr" ip -o -4 addr show \
      | awk -v pfx="$(ip_on "$ctr" natm-pub)/" '$4 ~ "^" pfx {print $2; exit}')
    [ -n "$oif" ] || fail "$ctr: no interface carries its public address"
    podman exec -i "$ctr" nft -f - <<NFT
table inet rfc5382
flush table inet rfc5382
table inet rfc5382 {
  chain input {
    type filter hook input priority filter; policy accept;
    iifname "$oif" tcp flags & (syn | ack) == syn ct state new drop
  }
}
NFT
    podman exec "$ctr" nft list chain inet rfc5382 input | grep -q "iifname \"$oif\"" \
      || fail "$ctr: the RFC 5382 rule did not land"
  done
}

# THE PRIVATE RANGE IS UNREACHABLE FROM OUTSIDE A LAN, as on the
# Internet. Rootless podman attaches every network's bridge to one
# namespace that routes between them with forwarding on, so without this
# a packet router A forwards toward LAN B's private address reaches LAN
# B's peer directly and never crosses router B: every endpoint-dependent
# punch "succeeded", ten of ten, over a path no NAT was on (measured
# 2026-09-26, and the reason this exists). ADR-0052 admits a peer's
# private candidate beside a private listener of the family, and both
# ends have one, so the punch dials exactly that address. A blackhole for
# the pool the LANs come from, on every router and every public-side
# container: a router keeps its own LAN, whose connected route is more
# specific. The NAT rows do not run this -- nothing in them sends to a
# LAN from outside it.
isolate() {
  local ctr
  for ctr in "$@"; do
    podman exec "$ctr" ip route add blackhole "$LAN_POOL"
  done
}
LAN_POOL="${LAN_POOL:-10.89.0.0/16}"

# The two relays, each an AutoNAT server and each probing the other, so
# each ends VerifiedPublic and hands out reservations that carry an
# address. The arguments: flags for r1 alone, for r2 alone, and the trust
# both hold for the rows' clients. Sets R1, R2, A1, A2.
relays() {
  local r1_flags="${1:-}" r2_flags="${2:-}" trust="${3:-}"
  R1=$(keygen r1); R2=$(keygen r2)
  node_ctr natm-node-r1 natm-pub
  node_ctr natm-node-r2 natm-pub
  A1=$(ip_on natm-node-r1 natm-pub); A2=$(ip_on natm-node-r2 natm-pub)
  # shellcheck disable=SC2086 # word splitting is the point: each is a flag list
  node_run natm-node-r1 r1 --listen /ip4/0.0.0.0/tcp/4001 --relay-server --autonat-server \
    --autonat "$R2@/ip4/$A2/tcp/4001" --distinct 1 --infra "$R2" $trust $r1_flags
  # shellcheck disable=SC2086 # word splitting is the point: each is a flag list
  node_run natm-node-r2 r2 --listen /ip4/0.0.0.0/tcp/4001 --relay-server --autonat-server \
    --autonat "$R1@/ip4/$A1/tcp/4001" --distinct 1 --infra "$R1" $trust $r2_flags
}

# The circuit address `target` holds on relay `relay` at `addr`.
circuit() { printf '/ip4/%s/tcp/4001/p2p/%s/p2p-circuit/p2p/%s' "$2" "$1" "$3"; }

# A client behind `router` on `lan`, as log `name`, reserving on both
# relays; `extra` adds flags.
client() {
  local ctr="$1" name="$2" lan="$3" router="$4" extra="${5:-}"
  node_ctr "$ctr" "$lan" "$router"
  # shellcheck disable=SC2086 # word splitting is the point: a flag list
  node_run "$ctr" "$name" --listen /ip4/0.0.0.0/tcp/4001 \
    --relay "$R1@/ip4/$A1/tcp/4001" --relay "$R2@/ip4/$A2/tcp/4001" \
    --infra "$R1" --infra "$R2" $extra
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
  relays "" "" "--infra $c"
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

# ITEM 2, the first half: relay loss. The client holds a reservation on
# each relay; r1 is killed. RELAY.md section 10: one reservation lost, the
# client stays reachable through the other. THE CONTROL is the same
# dialer asking for the client through the dead relay's circuit, which
# must fail while the live one's succeeds.
row_loss() {
  log "== row loss: a relay lost under a reserved client =="
  fresh
  record
  local c d
  c=$(keygen c); d=$(keygen d)
  relays "" "" "--infra $c --infra $d"
  client natm-node-c c natm-lan natm-router "--data $d"
  verified
  await c "RelayStandingChanged \{ standing: Satisfied, active: 2, target: 2" \
    "the client holds a reservation on each relay"
  local killed_at
  killed_at=$(mark c)
  podman kill natm-node-r1 >/dev/null
  log "  killed : r1"
  await c "RelayReservationChanged \{ relay: TransportIdentity\(\"$R1\"\), outcome: (Lost|Failed)" \
    "the reservation on the killed relay is reported gone"
  await c "RelayStandingChanged \{ standing: Partial, active: 1, target: 2" \
    "the client's standing drops to one of two, not to none"
  node_ctr natm-node-d natm-lan-b natm-router-b
  node_run natm-node-d d --relay-transport --data "$c" --infra "$R1" --infra "$R2" \
    --dial "$c@$(circuit "$R1" "$A1" "$c")" --dial "$c@$(circuit "$R2" "$A2" "$c")" \
    --dial-after-ms 2000
  await d "Connected \{ peer: TransportIdentity\(\"$c\"\), path: Relayed" \
    "a dialer behind router B reaches the client over the surviving relay"
  await d "DialFailed \{ peer: Some\(TransportIdentity\(\"$c\"\)\), detail: \".*$A1" \
    "the control: the same dialer through the dead relay fails"
  absent c "RelayReservationChanged \{ relay: TransportIdentity\(\"$R2\"\), outcome: (Lost|Failed|Released)" \
    "the surviving reservation was not lost after r1 was" "$killed_at"
  log "ROW loss: PASS"
}

# ITEM 2, the second half: capacity denial. r1 holds one reservation at
# most; two clients, one per NAT domain, ask both relays. RELAY.md
# section 10: a server at capacity is an explicit operational rejection,
# and the denied client stays reachable through the other relay. THE
# CONTROL is the same two clients against an r1 with room for both.
row_capacity() {
  local cap="$1"
  log "== row capacity: r1 holds $cap reservation(s), two clients ask =="
  fresh
  record
  local c1 c2
  c1=$(keygen c1); c2=$(keygen c2)
  relays "--max-reservations $cap" "" "--infra $c1 --infra $c2"
  # THE CLIENTS START ONLY ONCE THE RELAYS ARE VERIFIED. Started earlier,
  # a client's first ask is accepted with no address, refused by the
  # client, and still held by the relay against its ceiling until it
  # closes -- which is the early row's measurement, and would make this
  # one a measurement of that instead of the ceiling.
  verified
  client natm-node-c1 c1 natm-lan natm-router
  client natm-node-c2 c2 natm-lan-b natm-router-b
  await c1 "RelayReservationChanged \{ relay: TransportIdentity\(\"$R2\"\), outcome: Accepted" \
    "client 1 holds a reservation on r2"
  await c2 "RelayReservationChanged \{ relay: TransportIdentity\(\"$R2\"\), outcome: Accepted" \
    "client 2 holds a reservation on r2"
  if [ "$cap" -ge 2 ]; then
    await c1 "RelayStandingChanged \{ standing: Satisfied, active: 2" "client 1 holds both"
    await c2 "RelayStandingChanged \{ standing: Satisfied, active: 2" "client 2 holds both"
    absent r1 "ReservationDenied" "the control: r1, with room for both, denies nobody"
  else
    await r1 "RelayServed \{ peer: TransportIdentity\(\"($c1|$c2)\"\), destination: None, outcome: ReservationDenied" \
      "r1 at its ceiling denies the second client, explicitly"
    await_any "RelayStandingChanged \{ standing: Satisfied, active: 2" \
      "the client r1 accepted holds both reservations" c1 c2
    local held=0 n
    for n in c1 c2; do
      grep -Eq "RelayReservationChanged \{ relay: TransportIdentity\(\"$R1\"\), outcome: Accepted" "$WORK/out/$n.log" \
        && held=$((held + 1))
    done
    [ "$held" -eq 1 ] || fail "r1 at a ceiling of one accepted $held clients"
    log "  ok     : exactly one client holds r1, and both hold r2"
  fi
  log "ROW capacity($cap): PASS"
}

# ITEM 3: a network-interface change, MEASURED. The client, reserved on
# both relays, has its LAN interface taken away (`podman network
# disconnect`) and, a window later, given back (`connect`, with its
# default route put back through the router, as a lease would).
#
# `contracts/CONNECTIVITY.md` says a network change invalidates affected
# evidence and rebuilds ephemeral path state, relay reservations among
# it. As built, step 10 reports the change (`NetworkChanged`) and closes
# nothing, and the substrate runs no keepalive, so an idle connection
# over a vanished interface is noticed by neither end. This row asserts
# the report and records, for a fixed window after each step, what the
# client and the relays did -- the numbers go to the record, which is
# where the divergence from the contract is decided. Moving a container
# between networks is not a laptop leaving Wi-Fi; README.md says so.
IFCHANGE_WINDOW="${IFCHANGE_WINDOW:-120}"
row_ifchange() {
  log "== row ifchange: the client's interface taken away and given back =="
  fresh
  record
  local c old_addr new_addr gw since r1_since r2_since
  c=$(keygen c)
  relays "" "" "--infra $c"
  verified
  client natm-node-c c natm-lan natm-router \
    "--autonat $R1@/ip4/$A1/tcp/4001 --autonat $R2@/ip4/$A2/tcp/4001"
  await c "RelayStandingChanged \{ standing: Satisfied, active: 2, target: 2" \
    "the client holds a reservation on each relay"
  old_addr=$(ip_on natm-node-c natm-lan)
  since=$(mark c); r1_since=$(mark r1); r2_since=$(mark r2)
  podman network disconnect natm-lan natm-node-c
  log "  change : disconnected natm-lan ($old_addr)"
  await c "NetworkChanged \{ removed: \[[^]]*/ip4/$old_addr/tcp/4001" \
    "the runtime reports the removed address as a network change" "$since"
  sleep "$IFCHANGE_WINDOW"
  ifchange_measure "in the ${IFCHANGE_WINDOW}s after removal" "$c" "$since" "$r1_since" "$r2_since"

  since=$(mark c); r1_since=$(mark r1); r2_since=$(mark r2)
  podman network connect natm-lan natm-node-c
  gw=$(ip_on natm-router natm-lan)
  podman exec natm-node-c sh -c "while ip route del default 2>/dev/null; do :; done; ip route add default via $gw"
  new_addr=$(ip_on natm-node-c natm-lan)
  log "  change : reconnected natm-lan ($new_addr), default via $gw"
  await c "NetworkChanged \{ removed: \[[^]]*\], added: \[[^]]*/ip4/$new_addr/tcp/4001" \
    "the new address is reported as added" "$since"
  sleep "$IFCHANGE_WINDOW"
  ifchange_measure "in the ${IFCHANGE_WINDOW}s after reconnection" "$c" "$since" "$r1_since" "$r2_since"
  grep -q "^ID $c\$" "$WORK/out/c.log" || fail "the client's identity changed"
  log "  ok     : the same PeerId throughout"
  log "ROW ifchange: MEASURED"
}

# What the client and the relays logged about the client's reservations
# and connections from the given lines on.
ifchange_measure() {
  local window="$1" c="$2" since="$3" r1_since="$4" r2_since="$5" pat n
  for pat in "RelayReservationChanged .*outcome: (Lost|Failed)" \
             "RelayReservationChanged .*outcome: Accepted" \
             "RelayStandingChanged \{ standing: Satisfied" \
             "Disconnected \{ peer" \
             "ConnectivityChanged"; do
    n=$(tail -n "+$since" "$WORK/out/c.log" | grep -Ec -- "$pat" || true)
    log "  measure: client, $window: $n x /$pat/"
  done
  log "  measure: client, $window: last $(grep '^RES' "$WORK/out/c.log" | tail -n 1)"
  for r in "r1:$r1_since" "r2:$r2_since"; do
    n=$(tail -n "+${r#*:}" "$WORK/out/${r%%:*}.log" | grep -Ec -- "Disconnected \{ peer: TransportIdentity\(\"$c\"\)|peer: TransportIdentity\(\"$c\"\), destination: None, outcome: Reservation(Closed|TimedOut)" || true)
    log "  measure: ${r%%:*}, $window: $n x the client's connection or reservation ending"
  done
}

# ITEM 4: hole-punch outcomes, per NAT mapping class. Each trial is a
# target behind router A reserved on r1 and a dialer behind router B that
# reaches it over r1's circuit, both with DCUtR on; the trial's outcome
# is the first terminal `HolePunch` either end reports. FRESH IDENTITIES
# AND CONTAINERS PER TRIAL: DCUtR's five-minute cooldown is per peer, and
# a reused pair would be declined rather than measured. The relays' trust
# is set at start, so every trial's pair is generated first.
#
# A RATE AGAINST THESE CLASSES, NOT A POPULATION: which fraction of real
# peers sits behind which class is not something a container can say, and
# README.md carries that limit beside the numbers. Both domains share one
# NAT_MODE -- `topology.sh` builds no mixed pair -- so the pairings are
# eim/eim and eds/eds.
PUNCH_TRIALS="${PUNCH_TRIALS:-10}"
row_punch() {
  local mode="$1" n="$PUNCH_TRIALS" i trust="" ok=0 outcome tally=""
  log "== row punch: $n trials, NAT_MODE=$mode on both domains =="
  NAT_MODE="$mode" fresh
  record
  for i in $(seq 1 "$n"); do
    keygen "t$i" > "$WORK/keys/t$i.peer"
    keygen "d$i" > "$WORK/keys/d$i.peer"
    trust="$trust --infra $(cat "$WORK/keys/t$i.peer") --infra $(cat "$WORK/keys/d$i.peer")"
  done
  relays "" "" "$trust"
  verified
  for i in $(seq 1 "$n"); do
    outcome=$(punch_trial "$i")
    log "  trial  : $i $outcome"
    tally="$tally $outcome"
    [ "$outcome" != Succeeded ] || ok=$((ok + 1))
  done
  log "  measure: NAT_MODE=$mode: $ok of $n punches succeeded; outcomes:$(printf '%s\n' $tally | sort | uniq -c | awk '{printf " %s=%s", $2, $1}')"
  log "ROW punch($mode): MEASURED"
}

# One trial; prints its outcome: the first terminal HolePunch outcome
# either end reported, or `NoOutcome` if neither did within the horizon.
punch_trial() {
  local i="$1" t d since
  t=$(cat "$WORK/keys/t$i.peer"); d=$(cat "$WORK/keys/d$i.peer")
  node_ctr natm-node-t natm-lan natm-router
  node_ctr natm-node-d natm-lan-b natm-router-b
  node_run natm-node-t "t$i" --listen /ip4/0.0.0.0/tcp/4001 --relay "$R1@/ip4/$A1/tcp/4001" \
    --infra "$R1" --infra "$R2" --data "$d" --dcutr --stability-ms 2000
  await "t$i" "RelayReservationChanged \{ relay: TransportIdentity\(\"$R1\"\), outcome: Accepted" \
    "trial $i: the target holds a reservation on r1" >&2
  node_run natm-node-d "d$i" --listen /ip4/0.0.0.0/tcp/4001 --relay-transport --dcutr --stability-ms 2000 \
    --data "$t" --infra "$R1" --dial "$t@$(circuit "$R1" "$A1" "$t")" --dial-after-ms 500
  local terminal="HolePunch \{ peer: TransportIdentity\(\"($t|$d)\"\), outcome: (Succeeded|Unstable|Failed|TimedOut|Abandoned|Declined)"
  local waited=0 line
  while :; do
    line=$(grep -h -m1 -E -- "$terminal" "$WORK/out/t$i.log" "$WORK/out/d$i.log" 2>/dev/null | head -n 1 || true)
    [ -z "$line" ] || break
    waited=$((waited + 1))
    [ "$waited" -le 240 ] || { line=""; break; }
    sleep 0.5
  done
  local outcome=NoOutcome
  [ -z "$line" ] || outcome=$(printf '%s\n' "$line" | sed -E 's/.*outcome: ([A-Za-z]+).*/\1/')
  # A SUCCESS COUNTS ONLY WITH THE PATH IT PROMISES: the dialer's path to
  # the target moving from Relayed to Direct for the punch, which step 9
  # announces only once the direct connection has held for the stability
  # interval. `Succeeded` without it is its own outcome, not a success.
  if [ "$outcome" = Succeeded ]; then
    waited=0
    until grep -Eq "PeerPathChanged \{ peer: TransportIdentity\(\"$t\"\), previous: Relayed, current: Direct, reason: HolePunched" "$WORK/out/d$i.log"; do
      waited=$((waited + 1))
      [ "$waited" -le 40 ] || { outcome=SucceededNoPath; break; }
      sleep 0.5
    done
  fi
  podman rm -f natm-node-t natm-node-d >/dev/null 2>&1 || true
  echo "$outcome"
}

# A FINDING, MEASURED RATHER THAN ASSERTED AWAY: a relay serves
# reservations before AutoNAT has verified any address of its own (the
# server forces `Status::Enable`, relay_server_driver.rs), so a client
# that asks early is accepted with no address, refuses the reservation
# (`NoAddressesInReservation`), and the relay goes on counting it against
# its ceiling until it closes. Two clients against a ceiling of two: both
# first asks accepted and unusable, every later ask denied
# `ResourceLimitExceeded`, until the phantoms close. The row asserts that
# shape and prints how long the ceiling was held by reservations nobody
# could use; the numbers go to the record, not to a threshold.
row_early() {
  log "== row early: reservations asked before the relays are verified =="
  fresh
  record
  local c1 c2
  c1=$(keygen c1); c2=$(keygen c2)
  relays "--max-reservations 2" "" "--infra $c1 --infra $c2"
  client natm-node-c1 c1 natm-lan natm-router
  client natm-node-c2 c2 natm-lan-b natm-router-b
  await c1 "RelayReservationChanged \{ relay: TransportIdentity\(\"$R1\"\), outcome: Failed, addresses: \[\], detail: Some\(\"Failed to get Reservation" \
    "client 1's early reservation on r1 is refused by the client itself"
  await r1 "outcome: ReservationDenied \{ status: \"ResourceLimitExceeded\"" \
    "r1 then denies an ask for want of room, with nothing usable held"
  verified
  await_any "RelayReservationChanged \{ relay: TransportIdentity\(\"$R1\"\), outcome: Accepted, addresses: \[\"/ip4/$A1" \
    "a usable reservation on r1, once the phantoms have closed" c1 c2
  local first_accept first_denial last_denial usable
  first_accept=$(grep -m1 -E "RelayServed .*outcome: ReservationAccepted" "$WORK/out/r1.log" | awk '{print $2}')
  first_denial=$(grep -m1 -E "outcome: ReservationDenied" "$WORK/out/r1.log" | awk '{print $2}')
  last_denial=$(grep -E "outcome: ReservationDenied" "$WORK/out/r1.log" | tail -n 1 | awk '{print $2}')
  usable=$(grep -h -m1 -E "RelayReservationChanged \{ relay: TransportIdentity\(\"$R1\"\), outcome: Accepted" \
    "$WORK/out/c1.log" "$WORK/out/c2.log" | awk '{print $2}' | sort -n | head -n 1)
  log "  measure: r1 first accepted an (address-less) reservation at ${first_accept} ms"
  log "  measure: r1 denied for want of room from ${first_denial} ms to ${last_denial} ms"
  log "  measure: $(grep -c "outcome: ReservationDenied" "$WORK/out/r1.log") denials, none for a usable reservation held"
  log "  measure: the first usable reservation on r1 arrived at ${usable} ms of the client's run"
  log "ROW early: MEASURED"
}

main() {
  # REBUILT EVERY RUN, as `run.sh` rebuilds the matrix image: a stale
  # local tag would otherwise be what is measured.
  podman build -q -t interweave-natmatrix:1 -f Containerfile . >/dev/null
  podman build -q -t "$IMG_NODE" -f Containerfile.node . >/dev/null
  case "${1:-}" in
    services) row_services ;;
    loss) row_loss ;;
    capacity) row_capacity 1; row_capacity 2 ;;
    early) row_early ;;
    ifchange) row_ifchange ;;
    punch) row_punch eim; row_punch eds ;;
    punch-eim) row_punch eim ;;
    punch-eds) row_punch eds ;;
    all) row_services; row_loss; row_capacity 1; row_capacity 2; row_early; row_ifchange; row_punch eim; row_punch eds ;;
    *) echo "usage: $0 services|loss|capacity|early|ifchange|punch|all" >&2; exit 2 ;;
  esac
  teardown
}

trap 'teardown >/dev/null 2>&1 || true' EXIT
main "$@"
