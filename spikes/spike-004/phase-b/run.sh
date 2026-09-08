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

podman image exists "$IMAGE" \
  || podman build -q -t "$IMAGE" -f "$here/Containerfile" "$here" >/dev/null

rows=0
for mode in eim eds; do
  printf '\n== NAT_MODE=%s ==\n' "$mode"
  NAT_MODE="$mode" "$here/topology.sh" up
  EXPECT="$mode" "$here/probe.sh"
  rows=$((rows + 1))
done
"$here/topology.sh" down

# NAMED, not counted from a variable that could stay zero. A run that
# built no topology and printed "all rows passed" is the shape this
# spike's phase A record spends a paragraph warning about, and the same
# shape `spike-003`'s harness refuses by exiting 2 on an id that matches
# nothing.
[ "$rows" -eq 2 ] || { echo "expected 2 matrix rows, ran $rows" >&2; exit 2; }
printf '\nall %s matrix rows measured and matched\n' "$rows"
