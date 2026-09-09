#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Run every row of the matrix and exit non-zero if any one of them is
# not what it was configured to be.
#
# This is the whole harness from a caller's point of view: build the
# image, build each topology, measure it, assert the measurement matches
# the configuration, tear down. It reports a class it OBSERVED or it
# fails; there is no path where it prints a row it did not measure.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE="${IMAGE:-interweave-natmatrix:1}"

MODES="${MODES:-eim eds}"

# THE ROW SET IS VALIDATED BEFORE ANYTHING RUNS. It is not the only such
# check and this comment has now been wrong about that THREE times --
# "the only check that can fail", then "the only check on this script's
# INPUT", each falsified by the next commit to add one. So it stops
# counting: `FILTER_MODE` is validated below, `$ports` and `$filtering`
# are asserted in `probe_domain`, and everything else here fails the run
# by failing -- the build, `topology.sh up`, each probe.
# `MODES` is a caller-supplied filter, so
# `MODES=" "` is set and non-null -- `:-` does not substitute, the loop
# runs zero times, and every after-the-fact tally then agrees with
# itself about nothing. The previous version printed `1 matrix rows,
# each measured and matched, all distinct:` and exited 0 in exactly that
# case, because `printf '%s\n'` with no arguments still prints the
# format once. Review finding on PR #78.
# THE SPLIT HAPPENS ONCE. `set --` and a later `for mode in $MODES`
# would be two independent word-splits of the same string -- the object
# validated and the object iterated would not be the same object, which
# is the shape half the findings in this directory have had.
# GLOB DISABLED for the split, because an unquoted expansion is pathname
# expansion as well as word splitting: a mode containing `*` would be
# replaced by matching filenames in the working directory rather than
# reaching `topology.sh`'s unknown-mode arm. Review finding on PR #78.
set -f
# shellcheck disable=SC2086 # word splitting is the point: MODES is a list, and set -f keeps globs out of it
set -- $MODES
set +f
[ "$#" -ge 1 ] || { echo "MODES named no rows, so nothing would be measured" >&2; exit 2; }

# THE FILTERING CLASS EACH ROW IS BUILT FOR, and `conntrack` is the
# honest default: it is what masquerade alone gives, and it is the row a
# deployment actually meets. `address-restricted` and `full-cone` exist
# so the classifier has a positive control for every branch -- with
# conntrack alone it can only answer one way, which is indistinguishable
# from a constant. They need `NAT_MODE=eim`, since a static forward
# cannot name a per-flow mapped port.
FILTER_MODE="${FILTER_MODE:-conntrack}"
case "$FILTER_MODE" in
  conntrack) EXPECT_FILTER=apdf ;;
  address-restricted) EXPECT_FILTER=adf ;;
  full-cone) EXPECT_FILTER=eif ;;
  *) echo "unknown FILTER_MODE: $FILTER_MODE" >&2; exit 2 ;;
esac
# AND AGAINST THE ROW SET, here rather than inside `topology.sh`: the
# control modes install a static forward and so need `eim`, and checking
# that only when the row is built meant `FILTER_MODE=full-cone ./run.sh`
# measured the whole `eim` row before aborting on `eds`. Loud, but late.
# AND BEFORE THE BUILD AND THE TRAP, with the `MODES` check: this block
# sat seventy lines down, after `podman build` and after the EXIT trap
# was armed, so a typo in `FILTER_MODE` paid for a build and then tore
# down a topology the caller had standing from `topology.sh up`. Nothing
# in it depends on the build. Review finding on PR #81.
# WHAT THIS DOES AND DOES NOT MOVE: only `run.sh`'s OWN input is checked
# here. The ports and `HOLD_SECONDS` are validated by `filter.sh` and
# `SRC_PORTS` by `probe.sh`, each when it runs -- so `ALT_SOURCE_PORT=9000
# ./run.sh` still pays for the build, a topology and domain A's mapping
# before it exits 2. Late, but no longer destructive: by then the standing
# topology is this run's own, so the trap tears down nothing the caller
# built. Hoisting those checks would mean duplicating them; they stay
# where the value is used. Review finding on PR #81.
if [ "$FILTER_MODE" != conntrack ]; then
  for mode in "$@"; do
    [ "$mode" = eim ] \
      || { echo "FILTER_MODE=$FILTER_MODE needs MODES=eim; a static forward cannot name a per-flow mapped port (got '$mode')" >&2; exit 2; }
  done
fi
export FILTER_MODE

# The two LANs the two NAT domains sit on, named the same way
# `topology.sh` names them so a caller overriding one overrides both.
NET_LAN="${NET_LAN:-natm-lan}"
NET_LAN_B="${NET_LAN_B:-natm-lan-b}"

# BUILT EVERY RUN, not skipped when a tag exists. `interweave-natmatrix:1`
# is exactly the moving tag the Containerfile argues against: once
# anything with that name is present locally — an older build, another
# branch — the Containerfile would never be consulted again and the run
# would measure something this directory does not describe. The build is
# layer-cached, so the cost is a cache check.
podman build -q -t "$IMAGE" -f "$here/Containerfile" "$here" >/dev/null

# Leaked containers and networks otherwise outlive a failed row, and a
# Ctrl-C leaves every container behind -- seven once the filtering prober
# joined them, and a number here is one more thing to falsify. `up` calls
# `down` first, so a
# later run self-heals -- but only a later run.
trap '"$here/topology.sh" down >/dev/null 2>&1 || true' EXIT

# AND NOTHING IS ASSERTED ACROSS ROWS, deliberately. Two attempts at a
# cross-row claim have now been vacuous: a count of loop iterations
# against the literal the loop was written over, and a distinctness
# check that `probe.sh` had already made unfailable -- it asserts
# `class == EXPECT` per row and exits non-zero on a mismatch, so by the
# time control reaches here every class equals its own distinct mode by
# construction. The per-row assertion is what carries the claim; saying
# so is truer than a third tally.
measured=""

# Measure one NAT domain and record what it answered.
#
# A FAILING PROBE ALREADY STOPS THE RUN: `reported=$(... probe.sh)`
# below is a simple command whose status is the substitution's, so
# `set -e` fires and a non-zero `probe.sh` never reaches the lines that
# parse it. (The `class=` and `ports=` assignments under it ARE
# pipelines; they run only after the probe has succeeded. An earlier
# version of this comment credited `pipefail` with stopping the run,
# which was true when the probe itself was piped through `sed`, and a
# later one illustrated the point with a `class=$(probe.sh)` that does
# not exist in this file.)
#
# THE PORTS ARE GUARDED -- the second and last assertion in this file --
# and the class deliberately is not. `probe.sh`
# cannot print `CLASS=` without `PORTS=` today -- both are unconditional
# on its only exit-0 path -- but that is a contract across two files,
# and the summary below is worth reading only because of the ports. An
# empty one would print `natm-peer=eim()`, exit 0, and look like a pass:
# the same shape as the row set that measured nothing. Review finding on
# PR #78.
probe_domain() {
  local peer="$1" router="$2" lan="$3" expect="$4" reported class ports filtering
  printf -- '-- %s behind %s --\n' "$peer" "$router"
  reported=$(PEER="$peer" ROUTER="$router" LAN="$lan" EXPECT="$expect" \
    "$here/probe.sh")
  class=$(printf '%s\n' "$reported" | sed -n 's/^CLASS=//p')
  ports=$(printf '%s\n' "$reported" | sed -n 's/^PORTS=//p')
  [ -n "$ports" ] \
    || { echo "$peer: probe.sh reported no ports beside its class" >&2; return 1; }

  # BOTH HALVES OF RFC 4787, measured on the same domain. Mapping alone
  # says nothing about whether a punch succeeds -- filtering is the other
  # input -- and the harness reported one and called the other a
  # non-goal until `filter.sh` existed.
  reported=$(PEER="$peer" ROUTER="$router" LAN="$lan" \
    EXPECT_FILTER="$EXPECT_FILTER" "$here/filter.sh")
  filtering=$(printf '%s\n' "$reported" | sed -n 's/^FILTER=//p')
  [ -n "$filtering" ] \
    || { echo "$peer: filter.sh reported no class" >&2; return 1; }

  measured="$measured $peer=$class($ports)/$filtering"
}

for mode in "$@"; do
  printf '\n== NAT_MODE=%s FILTER_MODE=%s ==\n' "$mode" "$FILTER_MODE"
  NAT_MODE="$mode" "$here/topology.sh" up
  # BOTH DOMAINS ARE MEASURED, and the second one is why. A hole punch
  # needs two peers each behind their OWN translation, so the topology
  # builds two -- and the first version built the second and probed only
  # the first, which is the same overclaim as building one domain and
  # describing two. An unmeasured NAT is not evidence of a NAT: the
  # rule-landed assertion in `topology.sh` says a rule is present, not
  # that anything was translated by it. Review finding on PR #78.
  probe_domain natm-peer   natm-router   "$NET_LAN"   "$mode"
  probe_domain natm-peer-b natm-router-b "$NET_LAN_B" "$mode"
done

# THE OBSERVED PORTS, which are the only part of this line that is not a
# restatement of the INPUT: the peer names are two literals written here,
# and NEITHER class can differ from what was asked for -- the mapping
# class is checked against `EXPECT` and the filtering class against
# `EXPECT_FILTER`, both of which exit non-zero on a mismatch -- the
# per-row assertion exits non-zero on any other value -- so a summary
# carrying classes alone prints back its own input, which is what the
# two tallies before it did in different words. The ports are
# NOT CONSTRAINED BY ANY ASSERTION HERE -- which is a statement about
# this harness's assertions, and is the strongest true one available.
# Two stronger ones have already failed: "they differ between runs and
# between domains" (an `eim` row reports the bound port twice, in both
# domains), and "nothing in this harness decides what they are" (it sets
# the source ports, and an `eim` NAT that can preserve them reports them
# back). What survives is that no check here requires any particular
# value, so an unexpected one reaches the reader instead of being
# normalised away. The full measurement is `probe.sh`'s block on stderr.
printf '\nmeasured and matched:%s\n' "$measured"
