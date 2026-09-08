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

# THE ROW SET IS VALIDATED BEFORE ANYTHING RUNS, and this is the only
# check here that can fail. `MODES` is a caller-supplied filter, so
# `MODES=" "` is set and non-null -- `:-` does not substitute, the loop
# runs zero times, and every after-the-fact tally then agrees with
# itself about nothing. The previous version printed `1 matrix rows,
# each measured and matched, all distinct:` and exited 0 in exactly that
# case, because `printf '%s\n'` with no arguments still prints the
# format once. Review finding on PR #78.
# shellcheck disable=SC2086
set -- $MODES
[ "$#" -ge 1 ] || { echo "MODES named no rows, so nothing would be measured" >&2; exit 2; }

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

# AND NOTHING IS ASSERTED ACROSS ROWS, deliberately. Two attempts at a
# cross-row claim have now been vacuous: a count of loop iterations
# against the literal the loop was written over, and a distinctness
# check that `probe.sh` had already made unfailable -- it asserts
# `class == EXPECT` per row and exits non-zero on a mismatch, so by the
# time control reaches here every class equals its own distinct mode by
# construction. The per-row assertion is what carries the claim; saying
# so is truer than a third tally.
measured=""
for mode in $MODES; do
  printf '\n== NAT_MODE=%s ==\n' "$mode"
  NAT_MODE="$mode" "$here/topology.sh" up
  # A FAILING PROBE ALREADY STOPS THE RUN: `set -e` plus `pipefail` make
  # this assignment inherit the pipeline's status, so a non-zero
  # `probe.sh` never reaches the next line. The guard that used to sit
  # here tested a variable that cannot be empty when it is read.
  class=$(EXPECT="$mode" "$here/probe.sh" | sed -n 's/^CLASS=//p')
  measured="$measured $class"
done

printf '\n%s matrix rows, each measured and matched:%s\n' "$#" "$measured"
