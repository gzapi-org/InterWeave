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

# BUILT EVERY RUN, not skipped when a tag exists. `interweave-natmatrix:1`
# is exactly the moving tag the Containerfile argues against: once
# anything with that name is present locally — an older build, another
# branch — the Containerfile would never be consulted again and the run
# would measure something this directory does not describe. The build is
# layer-cached, so the cost is a cache check.
podman build -q -t "$IMAGE" -f "$here/Containerfile" "$here" >/dev/null

# Leaked containers and networks otherwise outlive a failed row, and a
# Ctrl-C leaves six containers behind. `up` calls `down` first, so a
# later run self-heals -- but only a later run.
trap '"$here/topology.sh" down >/dev/null 2>&1 || true' EXIT

MODES="${MODES:-eim eds}"
measured=""
for mode in $MODES; do
  printf '\n== NAT_MODE=%s ==\n' "$mode"
  NAT_MODE="$mode" "$here/topology.sh" up
  class=$(EXPECT="$mode" "$here/probe.sh" | sed -n 's/^CLASS=//p')
  [ -n "$class" ] || { echo "probe reported no class for $mode" >&2; exit 2; }
  measured="$measured $class"
done

# ASSERT THE ROWS WERE DISTINCT, which is the only claim worth making
# here. An earlier version counted loop iterations against the literal
# `2` it had just looped over -- an assertion that could not fail, sat
# under a comment saying it was "NAMED, not counted from a variable",
# above a counted variable. It also cited `spike-003`'s guard, which
# protects against a USER-SUPPLIED filter selecting nothing; this script
# takes no such input, so the guard was imported without the thing it
# guards.
#
# Distinct classes is the real property: it fails if the topology built
# the same NAT twice, or if the classifier answers the same whatever it
# is shown.
distinct=$(printf '%s\n' $measured | sort -u | wc -l)
count=$(printf '%s\n' $measured | wc -l)
[ "$distinct" -eq "$count" ] \
  || { echo "rows were not distinct: measured$measured" >&2; exit 2; }
printf '\n%s matrix rows, each measured and matched, all distinct:%s\n' "$count" "$measured"
