#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_yamux_muxer.sh
#
# >>> help
# Does any first-party source call a yamux `Config` setter?
#
# It must not, and the reason is not style. `libp2p-yamux` 0.47 depends
# on BOTH yamux 0.12.1 and the patched 0.13.10. `Config::default()`
# returns `Either::Right(Config013)` — the patched one, which is what
# `crates/transport/libp2p` uses. But `Config::set` reads:
#
#     Either::Right(_) => {
#         self.0 = Either::Left(Config012::default());
#
# so EVERY tuning setter silently moves the muxer onto 0.12.1:
#
#     set_receive_window_size   set_max_buffer_size
#     set_max_num_streams       set_window_update_mode
#
# yamux 0.12.1 has a remote-panic denial of service — a malformed Data
# frame with SYN set and len 262145 (GHSA-vxx9-2994-q338), patched in
# 0.13.10. There is no deprecation warning on the downgrade and nothing
# in the type says it happened.
#
# WHY A BESPOKE CHECK RATHER THAN THE DEPENDENCY ONE. `cargo-deny`
# resolves advisories against RustSec, and yamux has NO RustSec advisory
# — the vulnerability exists only as a GHSA. `check_dependencies.sh`
# therefore reports clean, truthfully, and cannot ever catch this.
# Banning the version outright is no use either: 0.12.1 is in the graph
# unconditionally as `libp2p-yamux`'s alternate, so a ban would fail
# every run whether or not anything selected it.
#
# What is actually dangerous is OUR code choosing it, and that is what
# this greps for. CLAUDE.md §6 pushes toward bounded resources, so
# `set_max_num_streams` is exactly the call someone reaches for next.
#
# If a real need for one of these appears, the fix is not to delete this
# check: it is to confirm which implementation results, and to say so
# where the call is made.
#
# Exit codes:
#   0  no first-party source selects the vulnerable muxer
#   1  a yamux Config setter is called
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
cd "$ROOT" || exit 1

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi

# The four setters that route through `Config::set`. Listed rather than
# matched by prefix: a future `set_something_harmless` that does NOT
# downgrade should not fail this check, and a reviewer adding one here
# has to look at what it does first.
SETTERS=(
    set_receive_window_size
    set_max_buffer_size
    set_max_num_streams
    set_window_update_mode
)

# TRACKED FILES ONLY, and only Rust: the vendored registry sources under
# ~/.cargo contain these calls legitimately, and scanning them would fail
# on libp2p's own code.
mapfile -t sources < <(git ls-files '*.rs' 2>/dev/null)
if [[ ${#sources[@]} -eq 0 ]]; then
    echo "check_yamux_muxer: no tracked Rust sources; nothing to check."
    exit 0
fi

found=0
for setter in "${SETTERS[@]}"; do
    # `yamux` must appear on the same line or the preceding two, so an
    # unrelated `set_max_num_streams` on some other type does not trip
    # this. Yamux config is always built as `yamux::Config` here.
    while IFS= read -r hit; do
        [[ -z "$hit" ]] && continue
        file="${hit%%:*}"
        rest="${hit#*:}"
        line="${rest%%:*}"
        context="$(sed -n "$((line > 2 ? line - 2 : 1)),${line}p" "$file")"
        if [[ "$context" == *yamux* ]]; then
            echo "check_yamux_muxer: $file:$line calls $setter on a yamux Config." >&2
            found=$((found + 1))
        fi
    done < <(grep -Hn -- "\.$setter(" "${sources[@]}" 2>/dev/null)
done

if (( found > 0 )); then
    cat >&2 <<'EOF'

Every yamux Config setter routes through `Config::set`, which replaces
the config with `Config012::default()` — silently moving the muxer from
the patched yamux 0.13.10 onto 0.12.1 and its remote-panic denial of
service (GHSA-vxx9-2994-q338).

`cargo-deny` cannot catch this: yamux has no RustSec advisory at all, so
`check_dependencies.sh` reports clean and always will.

If the tuning is genuinely needed, confirm which implementation results
before proceeding, and record it at the call site.
EOF
    exit 1
fi

echo "check_yamux_muxer: OK — no first-party source selects the vulnerable muxer."
