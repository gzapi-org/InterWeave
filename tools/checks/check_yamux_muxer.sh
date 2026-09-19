#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_yamux_muxer.sh
#
# >>> help
# Is the vulnerable yamux line out of the build graph?
#
#   tools/checks/check_yamux_muxer.sh
#
# yamux 0.12.1 carries a remote-panic denial of service
# (GHSA-vxx9-2994-q338) and NO RustSec advisory, so `cargo-deny` reports
# clean on it and always will. This guard is the only mechanism, which is
# why it exists at all rather than as a sentence in a document.
#
# WHAT IT ASKED BEFORE THE libp2p 0.57 BUMP, and why that stopped being
# the right question (measured 2026-09-19, and recorded because a guard
# whose premise has evaporated passes for the wrong reason forever).
# `libp2p-yamux` 0.47 depended on BOTH `yamux012 = 0.12.1` and
# `yamux013`, and its `Config::set` moved the config from the patched
# line onto 0.12.1 -- so every tuning setter silently selected the
# vulnerable muxer, and the guard scanned first-party sources for calls
# to the four setters that routed through it.
#
# `libp2p-yamux` 0.48 depends on `yamux = "0.14"` and nothing else. The
# dual-version scheme is gone, `Config012` with it, and three of the four
# setter names the old scan watched no longer exist (0.14 has
# `set_max_connection_receive_window`, `set_max_num_streams`,
# `set_read_after_close`, `set_split_send_size`, none of which downgrades
# anything). The old scan would therefore have passed forever while
# watching for calls that cannot occur -- green, and meaningless.
#
# SO THE QUESTION IS NOW THE PROPERTY ITSELF: no crate named `yamux` in
# the build graph may be in the 0.12 line. That is what the setter scan
# was ever a proxy for, it survives a future libp2p reintroducing a
# dual-version scheme (the new version would appear in the graph), and it
# needs no list of function names to stay current.
#
# NOT CHECKED HERE: whether some other crate in the graph carries its own
# vendored copy of yamux under a different package name. `cargo tree`
# answers for what cargo resolves, which is the same scope the old scan
# had.
#
# Exit codes:
#   0  no vulnerable yamux in the graph
#   1  the 0.12 line is present
#   2  cargo is unavailable, so the question could not be asked
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
cd "$ROOT" || exit 2

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi

if ! command -v cargo >/dev/null 2>&1; then
    # EXIT 2, NOT 0: a guard that cannot ask its question has not
    # answered it.
    echo "check_yamux_muxer: cargo is not available; the graph could not be read." >&2
    exit 2
fi

# The RESOLVED graph, not the lockfile: a lockfile records optional
# dependencies that no feature selects, and a crate cargo does not build
# cannot be the muxer this binary speaks.
#
# `--prefix none` so every line is `<name> v<version>` with no tree
# drawing, and the package name is matched WHOLE: `libp2p-yamux` ends in
# `yamux`, and that crate has had a 0.12 line of its own, so a substring
# match would report the wrapper as the muxer. Measured on this graph,
# where the naive pattern reported `yamux v0.48.0` -- which is
# `libp2p-yamux`.
if ! graph="$( cargo tree -e normal --prefix none 2>/dev/null )"; then
    echo "check_yamux_muxer: cargo tree failed; the graph could not be read." >&2
    exit 2
fi

vulnerable="$( printf '%s\n' "$graph" | awk '$1 == "yamux" && $2 ~ /^v0\.12\./ { print $1, $2 }' | sort -u )"
if [[ -n "$vulnerable" ]]; then
    echo "check_yamux_muxer: the vulnerable yamux line is in the build graph:" >&2
    printf '%s\n' "$vulnerable" | sed 's/^/    /' >&2
    cat >&2 <<'EOF'

yamux 0.12.1 carries a remote-panic denial of service
(GHSA-vxx9-2994-q338) and no RustSec advisory, so `cargo-deny` cannot
see it and `check_dependencies.sh` will report clean.

Find what pulls it in — `cargo tree -e normal -i yamux@0.12.1` — and
resolve that dependency onto a later line rather than suppressing this
check.
EOF
    exit 1
fi

present="$( printf '%s\n' "$graph" | awk '$1 == "yamux" { print $2 }' | sort -u | tr '\n' ' ' )"
echo "check_yamux_muxer: OK — no vulnerable yamux in the build graph (${present:-none present})."
