#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Build the flood row, then run it inside a private network namespace
# with one dummy interface that carries link-local multicast -- a domain
# chosen for the run rather than inherited from the host (SPIKE-010).
#
# Needs unprivileged user namespaces (`unshare -rn`) and iproute2. No
# root, and nothing outside the namespace is touched.
#
#   ./run.sh                     # targets 256,1024,4096,16384
#   ./run.sh 256,1024            # other targets
set -euo pipefail
cd "$(dirname "$0")"
cargo build --release --locked
bin="$PWD/target/release/flood"
targets="${1:-256,1024,4096,16384}"
exec unshare -rn sh -c '
  set -e
  ip link set lo up
  ip link add dummy0 type dummy
  ip link set dummy0 multicast on up
  ip addr add 10.99.0.1/16 dev dummy0
  ip route add 224.0.0.0/4 dev dummy0
  echo "kernel $(uname -r); namespace: dummy0 10.99.0.1/16, multicast on"
  FLOOD_IFACE=10.99.0.1 exec "$0" "$1"
' "$bin" "$targets"
