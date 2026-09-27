#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# SPIKE-010's domain and node rows: the shipping mDNS mechanism (`node/`,
# pinned by revision) in containers on multicast domains this script
# builds and MEASURES before any node runs.
#
#   NODE_BIN=<node built from node/ at its pin> WORK=<scratch dir> \
#     ./domains.sh <row>   # row: env | discover | path | host | crafted | ifchange | all
#
# THREE DOMAINS, two bridges. Rootless podman bridges carry link-local
# multicast between their containers (measured by `env`, first). What
# BLOCKS multicast comes in two shapes, and they are not the same fact to
# a node, so both are rows:
#
# - PATH-BLOCKED (`mc-block`): every member drops multicast arriving on
#   its interface. A sender's send succeeds and nothing arrives -- which
#   to the node is exactly an empty LAN. `providers/mdns.md` §Failure:
#   silence is never the degraded signal, so the row asserts no discovery
#   AND no degradation.
# - HOST-BLOCKED (one node on `mc-carry`): the node's own host drops its
#   outgoing multicast, so its send fails (EPERM, measured 2026-09-27) --
#   the failure ADR-0053 rule 5 surfaces as `MdnsInterfaceFailed` and the
#   composer maps onto the provider's degraded state.
#
# The multicast flag on an interface is NOT a way to block: with it off,
# a send and a join still succeed and the packet still arrives (measured
# 2026-09-27). Nothing here relies on it.
#
# EVERY ROW ASSERTS AGAINST A CONTROL: the env row's carrying probe and a
# unicast datagram for the blocking one, the address added through the
# command path and a dial while connected for the route book and the
# funnel standing still, the static provider and a dial beside the
# degraded mDNS provider, a lawful announcement beside the refused one.

set -euo pipefail
cd "$(dirname "$0")"

NODE_BIN="${NODE_BIN:?NODE_BIN: the node built from node/ at its pin (cargo build --release --locked)}"
WORK="${WORK:?WORK: a scratch directory outside the tree, for keys and logs}"
# The node image SPIKE-004 phase B builds (`Containerfile.node` there):
# fedora-minimal with iproute, the node bind-mounted in. The tool image
# (`interweave-natmatrix:1`, phase B's `Containerfile`) carries socat and
# nftables for the probes and the drops.
IMG_NODE="${IMG_NODE:-interweave-phaseb-node:1}"
IMG_TOOL="${IMG_TOOL:-interweave-natmatrix:1}"
CARRY=mc-carry
BLOCK=mc-block
PATIENCE="${PATIENCE:-60}"

log() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# Every container this script starts carries this label, and teardown
# removes by it: a container started inside a command substitution (the
# probes) is in a subshell, where a list of names would never see it.
LABEL=interweave-spike010=1

teardown() {
  local ids
  ids=$(podman ps -aq --filter "label=$LABEL")
  # shellcheck disable=SC2086  # one id per word, by construction
  [ -z "$ids" ] || podman rm -f -t 0 $ids >/dev/null 2>&1 || true
  podman network rm "$CARRY" "$BLOCK" >/dev/null 2>&1 || true
}
trap teardown EXIT

record() {
  log "  kernel : $(uname -r)"
  log "  podman : $(podman --version)"
  log "  netbe  : $(podman info --format '{{.Host.NetworkBackend}} {{.Host.NetworkBackendInfo.Version}}' 2>/dev/null || echo '?')"
  log "  node   : sha256 $(sha256sum "$NODE_BIN" | awk '{print $1}')"
  log "  pin    : $(sed -n 's/^interweave-transport-libp2p = .*rev = "\([0-9a-f]*\)".*/\1/p' node/Cargo.toml)"
  log "  image  : $(podman image inspect "$IMG_NODE" --format '{{.Id}}') (node), $(podman image inspect "$IMG_TOOL" --format '{{.Id}}') (tool)"
}

fresh() {
  teardown
  podman network create --internal "$CARRY" >/dev/null
  podman network create --internal "$BLOCK" >/dev/null
  # Every row starts from nothing: an identity is never overwritten, and a
  # log from an earlier row must not satisfy this one's await.
  rm -rf "$WORK/keys" "$WORK/out"
  mkdir -p "$WORK/keys" "$WORK/out"
  # The identity store refuses a directory wider than 0700.
  chmod 700 "$WORK/keys"
}

ip_on() { podman inspect "$1" --format "{{(index .NetworkSettings.Networks \"$2\").IPAddress}}"; }

# A node container on `net`. `:z` because SELinux enforces on this host.
node_ctr() {
  local name="$1" net="$2"
  podman rm -f -t 0 "$name" >/dev/null 2>&1 || true
  podman run -d --name "$name" --network "$net" --label "$LABEL" \
    -v "$NODE_BIN:/node:ro,z" -v "$WORK/keys:/keys:ro,z" -v "$WORK/out:/out:z" \
    --entrypoint sleep "$IMG_NODE" infinity >/dev/null
}

tool_ctr() {
  local name="$1" net="$2"
  podman rm -f -t 0 "$name" >/dev/null 2>&1 || true
  podman run -d --name "$name" --network "$net" --label "$LABEL" --cap-add NET_ADMIN "$IMG_TOOL" sleep infinity >/dev/null
}

# Drop multicast in `ctr`'s own network namespace, from a sidecar that
# shares it -- the node image carries no nftables. `in`: arriving (the
# path-blocked domain); `out`: leaving (the host-blocked node).
drop_multicast() {
  local ctr="$1" dir="$2" hook
  case "$dir" in in) hook=input ;; out) hook=output ;; *) fail "drop_multicast: $dir" ;; esac
  podman run --rm --network "container:$ctr" --label "$LABEL" --cap-add NET_ADMIN "$IMG_TOOL" sh -c \
    "nft add table inet mcdrop && nft add chain inet mcdrop c '{ type filter hook $hook priority 0; }' && nft add rule inet mcdrop c ip daddr 224.0.0.0/24 drop" \
    || fail "$ctr: the $dir drop did not land"
}

keygen() { "$NODE_BIN" keygen "$WORK/keys/$1.id"; }

node_run() {
  local ctr="$1" name="$2"; shift 2
  : > "$WORK/out/$name.log"
  podman exec -d "$ctr" sh -c "/node run --identity /keys/$name.id $* > /out/$name.log 2>&1" >/dev/null
}

# Whether `name`'s log carries a line matching the extended regex at or
# after line `since`: one awk over the file, never `tail | grep -q`, whose
# early exit under pipefail reads a present match as absent (SPIKE-004
# phase B, #127's F2). The pattern goes through the environment, since
# `-v` would unescape `\(`.
matches() {
  local name="$1" pattern="$2" since="${3:-1}"
  [ -f "$WORK/out/$name.log" ] || return 1
  P="$pattern" awk -v s="$since" 'NR >= s && $0 ~ ENVIRON["P"] { found = 1; exit } END { exit !found }' \
    "$WORK/out/$name.log"
}

await() {
  local name="$1" pattern="$2" what="$3" since="${4:-1}" waited=0
  until matches "$name" "$pattern" "$since"; do
    waited=$((waited + 1))
    if [ "$waited" -gt $((PATIENCE * 2)) ]; then
      tail -n 15 "$WORK/out/$name.log" >&2 || true
      fail "$what: $name never logged /$pattern/ in ${PATIENCE}s"
    fi
    sleep 0.5
  done
  log "  ok     : $what"
}

absent() {
  local name="$1" pattern="$2" what="$3" since="${4:-1}"
  if matches "$name" "$pattern" "$since"; then
    P="$pattern" awk -v s="$since" 'NR >= s && $0 ~ ENVIRON["P"] && n++ < 3' "$WORK/out/$name.log" >&2
    fail "$what: $name logged /$pattern/"
  fi
  log "  ok     : $what"
}

mark() { echo $(( $(wc -l < "$WORK/out/$1.log") + 1 )); }

# The last report line of `kind` (CAND/HEALTH/STORE) in `name`'s log.
last() { grep "^$2 " "$WORK/out/$1.log" | tail -n 1 || true; }

esc() { printf '%s' "${1//./\\.}"; }

# ENVIRONMENT, measured before any node runs: a plain UDP probe to
# 224.0.0.251:5353 from one container, an observer joined to the group in
# another. The carrying domain delivers it -- the control for the
# blocking one, which must not.
probe() {
  local net="$1" drop="$2" s o sip
  s="probe-s-$net"; o="probe-o-$net"
  tool_ctr "$s" "$net"; tool_ctr "$o" "$net"
  [ "$drop" = yes ] && drop_multicast "$o" in
  podman exec -d "$o" sh -c 'rm -f /tmp/got; timeout 8 socat -u UDP4-RECV:5353,reuseaddr,ip-add-membership=224.0.0.251:eth0 OPEN:/tmp/got,creat,append' >/dev/null
  sleep 1
  sip=$(ip_on "$s" "$net")
  podman exec "$s" sh -c "echo probe-$net | socat -u - UDP4-DATAGRAM:224.0.0.251:5353,ip-multicast-if=$sip,ip-multicast-ttl=1" \
    || fail "$net: the probe's send failed"
  # THE OBSERVER'S OWN CONTROL: a UNICAST datagram to it on the same
  # port, which no drop here touches (they match 224.0.0.0/24). Its
  # arrival is what shows the observer was listening, so an empty
  # multicast result means "nothing arrived", never "nobody listened".
  podman exec "$s" sh -c "echo unicast-$net | socat -u - UDP4-DATAGRAM:$(ip_on "$o" "$net"):5353" \
    || fail "$net: the unicast control's send failed"
  sleep 2
  podman exec "$o" sh -c 'cat /tmp/got 2>/dev/null || true'
}

row_env() {
  log "== row env: the domains, measured before any node =="
  fresh
  record
  local got
  got=$(probe "$CARRY" no)
  [ "$got" = "probe-$CARRY
unicast-$CARRY" ] || fail "the carrying domain did not deliver the probe and the control: [$got]"
  log "  ok     : $CARRY carries 224.0.0.251:5353 (the probe arrived, then the unicast control)"
  got=$(probe "$BLOCK" yes)
  [ "$got" = "unicast-$BLOCK" ] || fail "the blocking domain: expected the unicast control alone: [$got]"
  log "  ok     : $BLOCK blocks it (the send succeeded; only the unicast control arrived, so the observer was listening)"
  log "ROW env: PASS"
}

# Two nodes on the carrying domain discover each other, and each candidate
# reaches the DiscoveryManager through the provider, attributed to mdns
# (guarantee 12). NOTHING mDNS LEARNED REACHES A DIAL (guarantee 13), shown
# two ways, both after the discovery:
#
# - a `DialPeer` -- which dials from the runtime's route book
#   (`ConnectionManager`) alone -- finds no address;
# - the mDNS wrapper's TWO DOORS stay shut: the root funnel counts every
#   address any behaviour offers a dial (ADR-0052 rule 5), and around the
#   control dial -- b's address given through the command path, b not yet
#   connected -- that count does not move. Were the wrapper to let the
#   crate's `NewExternalAddrOfPeer` through (door 1), request-response
#   would offer b's address; were it to forward the crate's pending-dial
#   answer (door 2), the crate would: either moves it.
#
# THE CONTROLS: the same dial connects once the command path has given
# the book the address; and a second dial while connected, which the
# behaviour holding the connection offers its address to, DOES move the
# funnel -- so the counter is live on this path, and its standing still
# around the control dial is the doors, not a dead counter.
row_discover() {
  log "== row discover: two nodes, one carrying domain =="
  fresh
  record
  local a b aip bip
  a=$(keygen a); b=$(keygen b)
  node_ctr n-a "$CARRY"; node_ctr n-b "$CARRY"
  aip=$(ip_on n-a "$CARRY"); bip=$(ip_on n-b "$CARRY")
  node_run n-b b --listen /ip4/0.0.0.0/tcp/4001 --data "$a" --run-for-s 60
  node_run n-a a --listen /ip4/0.0.0.0/tcp/4001 --data "$b" \
    --dial-peer "$b" --dial-after-ms 15000 \
    --add-address "$b@/ip4/$bip/tcp/4001" --add-after-ms 20000 --redial-after-ms 30000 --run-for-s 60
  await a "^CAND [0-9]+ $b sources=mdns addrs=.*\"/ip4/$(esc "$bip")/tcp/4001\"" \
    "a holds b as an mdns candidate at b's address, through the provider and the manager"
  await b "^CAND [0-9]+ $a sources=mdns addrs=.*\"/ip4/$(esc "$aip")/tcp/4001\"" \
    "b holds a likewise"
  await a "^HEALTH [0-9]+ mdns=Some\(Healthy\)" "a's mdns provider is healthy"
  await a "^DIALPEER [0-9]+ $b Ok\(Err\(NoKnownAddress\)\)" \
    "a's route book holds NO address for b, though mdns found it"
  local cand_ms dial_ms
  cand_ms=$(grep -m1 -E "^CAND [0-9]+ $b sources=mdns" "$WORK/out/a.log" | awk '{print $2}' || true)
  dial_ms=$(grep -m1 -E "^DIALPEER " "$WORK/out/a.log" | awk '{print $2}' || true)
  [ -n "$cand_ms" ] && [ -n "$dial_ms" ] && [ "$cand_ms" -lt "$dial_ms" ] \
    || fail "the route-book dial ($dial_ms ms) did not follow the discovery ($cand_ms ms)"
  log "  ok     : and that dial came after the discovery ($cand_ms ms < $dial_ms ms)"
  await a "^DIALPEER [0-9]+ $b Ok\(Ok\(\(\)\)\)" \
    "the control: the same dial succeeds once the command path gave the book b's address"
  await a "^FUNNEL [0-9]+ after contributed=" "the funnel is read around that dial"
  local before after
  before=$(awk '$1 == "FUNNEL" && $3 == "before" {sub("contributed=", "", $4); print $4; exit}' "$WORK/out/a.log")
  after=$(awk '$1 == "FUNNEL" && $3 == "after" {sub("contributed=", "", $4); print $4; exit}' "$WORK/out/a.log")
  [ "$before" = "$after" ] \
    || fail "a behaviour offered the control dial $((after - before)) address(es): a door of the mDNS wrapper is open"
  log "  ok     : no behaviour offered that dial an address (funnel $before -> $after): both doors shut"
  await a "Connected \{ peer: TransportIdentity\(\"$b\"\)" "and a connects to b"
  await a "^FUNNEL [0-9]+ after-redial contributed=" "a dials b again while connected"
  before=$(awk '$1 == "FUNNEL" && $3 == "before-redial" {sub("contributed=", "", $4); print $4; exit}' "$WORK/out/a.log")
  after=$(awk '$1 == "FUNNEL" && $3 == "after-redial" {sub("contributed=", "", $4); print $4; exit}' "$WORK/out/a.log")
  [ "$after" -gt "$before" ] || fail "the funnel did not move for a dial a behaviour had an address for ($before -> $after): the counter is not live"
  log "  ok     : the funnel's control: that dial was offered the connection's address ($before -> $after)"
  log "  measure: a: $(last a STORE)"
  log "ROW discover: PASS"
}

# The path-blocked domain: both nodes drop multicast arriving. Nothing is
# discovered, and the provider stays HEALTHY -- silence is not the
# degraded signal (`providers/mdns.md` §Failure). The control is
# `discover`, the same two nodes on the carrying domain.
row_path() {
  log "== row path: two nodes on the path-blocked domain =="
  fresh
  record
  keygen p >/dev/null; keygen q >/dev/null
  node_ctr n-p "$BLOCK"; node_ctr n-q "$BLOCK"
  drop_multicast n-p in; drop_multicast n-q in
  node_run n-q q --listen /ip4/0.0.0.0/tcp/4001 --run-for-s 30
  node_run n-p p --listen /ip4/0.0.0.0/tcp/4001 --run-for-s 30
  await p "^END [0-9]+ run-for elapsed" "p ran its 30 s, six query intervals"
  absent p "MdnsDiscovered" "p's runtime discovered nothing"
  absent p "^PUSHREFUSED " "and its provider refused nothing"
  absent p "sources=mdns" "so the manager holds no mdns candidate"
  absent p "MdnsInterfaceFailed|MdnsUnavailable" "and reported no failure: its sends succeed"
  absent p "mdns=Some\(Degraded\)" "so the provider is not degraded by silence"
  log "  measure: p: $(last p HEALTH)"
  log "ROW path: PASS"
}

# The host-blocked node: its own host drops its outgoing multicast. The
# send fails, the runtime reports the interface, the provider is
# DEGRADED -- and the static provider, a dial and the node itself carry
# on (`providers/mdns.md` §Failure).
row_host() {
  log "== row host: a node whose host drops its outgoing multicast =="
  fresh
  record
  local h a aip
  h=$(keygen h); a=$(keygen a)
  node_ctr n-a "$CARRY"; node_ctr n-h "$CARRY"
  aip=$(ip_on n-a "$CARRY")
  drop_multicast n-h out
  node_run n-a a --listen /ip4/0.0.0.0/tcp/4001 --data "$h" --run-for-s 40
  node_run n-h h --listen /ip4/0.0.0.0/tcp/4001 --data "$a" \
    --static "$a@/ip4/$aip/tcp/4001" \
    --dial-peer "$a" --dial-after-ms 2000 \
    --add-address "$a@/ip4/$aip/tcp/4001" --add-after-ms 4000 --run-for-s 30
  await h "MdnsInterfaceFailed \{ address: [0-9.]+, detail: \"Operation not permitted" \
    "h's runtime reports its interface: the send was refused"
  await h "^HEALTH [0-9]+ mdns=Some\(Degraded\) static=Some\(Healthy\)" \
    "the mdns provider is degraded; the static provider is not"
  await h "^CAND [0-9]+ $a sources=[^ ]*static-bootstrap" "the static provider's candidate stands"
  await h "Connected \{ peer: TransportIdentity\(\"$a\"\)" "the transport is unaffected: h connects to a"
  await h "^END [0-9]+ run-for elapsed" "and h ran to its end: nothing panicked or exited"
  log "ROW host: PASS"
}

# A crafted announcement whose address the learn-site boundary refuses
# (ADR-0052) is dropped there with its class; a lawful one from the same
# sender is not. A CIRCUIT address: the crate rewrites an announced
# address's first host to the packet's source, so a loopback literal
# would arrive as the sender's own IP, while the circuit marker survives.
row_crafted() {
  log "== row crafted: a refused announcement beside a lawful one =="
  fresh
  record
  local a x y r s sip
  a=$(keygen a); x=$(keygen x); y=$(keygen y); r=$(keygen r)
  node_ctr n-a "$CARRY"; node_ctr n-s "$CARRY"
  sip=$(ip_on n-s "$CARRY")
  node_run n-a a --listen /ip4/0.0.0.0/tcp/4001 --run-for-s 40
  # HEALTH, not STORE: the learn-site counts carry no mdns entry until
  # something has been counted.
  await a "^HEALTH " "a is reporting"
  : > "$WORK/out/s.log"
  podman exec n-s sh -c "/node announce --from-ip $sip --peer $x --addr /ip4/$sip/tcp/4001/p2p/$r/p2p-circuit --count 3" >> "$WORK/out/s.log" 2>&1
  podman exec n-s sh -c "/node announce --from-ip $sip --peer $y --addr /ip4/$sip/tcp/4002 --count 3" >> "$WORK/out/s.log" 2>&1
  grep -q '^SENDFAIL' "$WORK/out/s.log" && fail "the announcer could not send: $(grep -m1 SENDFAIL "$WORK/out/s.log")"
  await a "^STORE [0-9]+ mdns admitted=[0-9]+ refused=[^ ]*relayed:[1-9]" \
    "the circuit announcement is refused at the learn site, as relayed"
  await a "^CAND [0-9]+ $y sources=mdns addrs=.*\"/ip4/$(esc "$sip")/tcp/4002\"" \
    "the lawful announcement from the same sender reaches the manager"
  absent a "^CAND [0-9]+ $x " "the refused peer never became a candidate"
  log "  measure: a: $(last a STORE)"
  log "ROW crafted: PASS"
}

# A node whose interface goes away and comes back: it rediscovers its
# peer on the new interface. The control is the discovery before the
# change.
row_ifchange() {
  log "== row ifchange: a node's interface taken away and given back =="
  fresh
  record
  local a since old new
  a=$(keygen a); keygen c >/dev/null
  node_ctr n-a "$CARRY"; node_ctr n-c "$CARRY"
  node_run n-a a --listen /ip4/0.0.0.0/tcp/4001 --run-for-s 120
  node_run n-c c --listen /ip4/0.0.0.0/tcp/4001 --run-for-s 120
  await c "MdnsDiscovered .*$a" "c discovers a before the change"
  old=$(ip_on n-c "$CARRY")
  since=$(mark c)
  podman network disconnect "$CARRY" n-c
  log "  change : disconnected $CARRY"
  await c "NetworkChanged \{ removed: \[[^]]" "c reports the removal" "$since"
  sleep 10
  since=$(mark c)
  podman network connect "$CARRY" n-c
  new=$(ip_on n-c "$CARRY")
  log "  change : reconnected $CARRY ($old -> $new)"
  [ "$new" != "$old" ] || fail "the reconnection kept the address $old: not a move"
  await c "MdnsDiscovered .*$a" "c rediscovers a on the interface that came back, at a new address" "$since"
  sleep 5
  absent c "Mdns(InterfaceFailed|WatcherFailed|RebuildFailed|Unavailable)" \
    "and reports no mdns failure since the reconnection" "$since"
  log "ROW ifchange: PASS"
}

main() {
  # THE IMAGES ARE BUILT, not assumed: SPIKE-004 phase B's Containerfiles,
  # rebuilt every run for phase B's reason -- a stale local tag would be
  # measured instead, silently. `record()` prints what was built.
  podman build -q -t "$IMG_TOOL" -f ../spike-004/phase-b/Containerfile ../spike-004/phase-b >/dev/null \
    || fail "building $IMG_TOOL"
  podman build -q -t "$IMG_NODE" -f ../spike-004/phase-b/Containerfile.node ../spike-004/phase-b >/dev/null \
    || fail "building $IMG_NODE"
  case "${1:-}" in
    env) row_env ;;
    discover) row_discover ;;
    path) row_path ;;
    host) row_host ;;
    crafted) row_crafted ;;
    ifchange) row_ifchange ;;
    all) row_env; row_discover; row_path; row_host; row_crafted; row_ifchange ;;
    *) fail "usage: $0 env|discover|path|host|crafted|ifchange|all" ;;
  esac
}

main "$@"
