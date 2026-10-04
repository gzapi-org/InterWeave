#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/ci/with_display.sh
#
# >>> help
# Run a command inside a private X display and a live AT-SPI bus.
#
#   tools/ci/with_display.sh <command> [<arg>…]
#
# Plan §18 (Stage 15): the desktop client's end-to-end tests open a real
# window (winit on X11, the software renderer) and the accessibility
# bullet reads that window's tree through the platform adapter over
# AT-SPI. CI's runner has neither a display nor a session bus, so the
# `rust` job's Tests step runs under this; locally it gives a test the
# same session, apart from the desktop it was started on.
#
# WHAT IT STANDS UP, in order, and refuses to run the command without:
#   1. Xvfb on the first free display (-displayfd, so the display is
#      READY when its number is read, not merely started), no TCP;
#   2. a private session bus (dbus-run-session), started with a SEALED
#      environment, since every service it activates inherits it:
#      DISPLAY is Xvfb's and WAYLAND_DISPLAY is unset (winit chooses X11
#      here even from a Wayland desktop, and the AT-SPI launcher marks
#      Xvfb's root window, never the desktop's), GSETTINGS_BACKEND is
#      `memory`, because switching accessibility on writes the
#      toolkit-accessibility setting, which on a desktop would otherwise
#      land in the user's own dconf database and outlive the run, and
#      XDG_RUNTIME_DIR is a private directory removed with the run, where
#      the accessibility bus puts its socket;
#   3. accessibility switched on (org.a11y.Status IsEnabled = true on the
#      session's org.a11y.Bus, which activates at-spi-bus-launcher): an
#      adapter exports its tree only while that reads true;
#   4. the AT-SPI registry answering on the accessibility bus — the
#      registry is what a test asks for the applications, so a bus with
#      no registry would let a test fail as "the window exported
#      nothing" when the platform under it was what was missing.
# Each step that fails ends the run, named, before the command starts.
# Xvfb is terminated when the wrapper returns; the bus and what it
# activated end with dbus-run-session. The bus runs in a process group of
# its own (setsid), and INT, TERM or HUP sent to the wrapper — `kill`, or
# Ctrl-C in its terminal — ends that whole group, the command included,
# at once rather than once the command has finished: TERM to the group,
# then KILL to whatever is left after WITH_DISPLAY_STOP_SECONDS (5).
# TERM whatever arrived, since a background child starts with INT
# ignored. All three, the bound and the KILL are in test_with_display.sh.
#
# Needs: Xvfb (xvfb), dbus-run-session and dbus-daemon (dbus-daemon),
# gdbus (libglib2.0-bin), setsid (util-linux) and at-spi2-core. The command runs once; this
# adds no retry, so a flaky window is reported as one.
#
# Exit codes:
#   the command's own status, once it has run
#   125  the session could not be stood up, or no command was given; the
#        step that failed is printed and the command did not run
# <<< help

set -uo pipefail

me="with_display"
# How long Xvfb and the AT-SPI registry each get to come up. Measured in
# an ubuntu:24.04 container: under a second for each.
READY_SECONDS="${WITH_DISPLAY_READY_SECONDS:-20}"
# How long the command's group gets to end after TERM before KILL.
STOP_SECONDS="${WITH_DISPLAY_STOP_SECONDS:-5}"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi
die() { echo "$me: $*" >&2; exit 125; }
[[ $# -gt 0 ]] || die "no command given (--help)"

# INSIDE the bus (re-entered below): steps 3 and 4, then the command.
# The marker tells the outside that the bus came up and this ran.
if [[ -n "${WITH_DISPLAY_INNER:-}" && -d "$WITH_DISPLAY_INNER" ]]; then
    inner="$WITH_DISPLAY_INNER"
    unset WITH_DISPLAY_INNER
    touch "$inner/entered"

    # 3. Accessibility on. Setting the property is what activates the bus
    # launcher through the session bus's service file.
    gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
        --method org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled '<true>' \
        >/dev/null 2>"$inner/a11y.err" || {
        cat "$inner/a11y.err" >&2
        die "cannot switch accessibility on (org.a11y.Bus on the session bus) — is at-spi2-core installed?"
    }
    address="$(gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
        --method org.a11y.Bus.GetAddress 2>"$inner/a11y.err" | sed -n "s/^('\(.*\)',)\$/\1/p")"
    [[ -n "$address" ]] || { cat "$inner/a11y.err" >&2; die "the AT-SPI bus gave no address"; }

    # 4. The registry on that bus.
    for ((i = 0; i < READY_SECONDS * 10; i++)); do
        if gdbus call --address "$address" --dest org.a11y.atspi.Registry \
            --object-path /org/a11y/atspi/accessible/root \
            --method org.freedesktop.DBus.Properties.Get org.a11y.atspi.Accessible ChildCount \
            >/dev/null 2>"$inner/a11y.err"; then
            registry=up
            break
        fi
        sleep 0.1
    done
    [[ "${registry:-}" == up ]] || { cat "$inner/a11y.err" >&2; die "the AT-SPI registry did not answer within ${READY_SECONDS}s"; }

    echo "$me: DISPLAY=$DISPLAY, AT-SPI bus up, accessibility on" >&2
    exec "$@"
fi

# OUTSIDE: the tools, step 1, then step 2 re-entering this script.
for tool in Xvfb dbus-run-session dbus-daemon gdbus setsid; do
    command -v "$tool" >/dev/null || die "$tool not found — install xvfb, dbus-daemon, libglib2.0-bin, util-linux and at-spi2-core"
done

scratch="$(mktemp -d)" || die "cannot make a scratch directory"
xvfb_pid=""
bus_pid=""
cleanup() {
    if [[ -n "$xvfb_pid" ]]; then kill "$xvfb_pid" 2>/dev/null; wait "$xvfb_pid" 2>/dev/null; fi
    rm -rf "$scratch"
}
trap cleanup EXIT
# A signal ends the bus's whole process group, then the wrapper through
# exit, so the EXIT trap above runs. `wait` below is interrupted by a
# trapped signal, which a foreground child would defer. Before setsid has
# run in the child there is no group yet, and the child itself is ended.
forward() {
    # A second signal while the first is being handled (Ctrl-C twice)
    # changes nothing: the first is already ending the group, bounded.
    [[ -n "${stopping:-}" ]] && return
    stopping=1
    if [[ -n "$bus_pid" ]]; then
        kill -TERM -- "-$bus_pid" 2>/dev/null || kill -TERM "$bus_pid" 2>/dev/null
        for ((i = 0; i < STOP_SECONDS * 10; i++)); do
            kill -0 -- "-$bus_pid" 2>/dev/null || kill -0 "$bus_pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL -- "-$bus_pid" 2>/dev/null
        wait "$bus_pid" 2>/dev/null
    fi
    exit "$1"
}
trap 'forward 130' INT
trap 'forward 143' TERM
trap 'forward 129' HUP

# 1. Xvfb. -displayfd writes the display number once the server accepts
# connections, so reading it is the readiness check.
Xvfb -displayfd 3 -screen 0 1280x800x24 -nolisten tcp 3>"$scratch/display" 2>"$scratch/xvfb.log" &
xvfb_pid=$!
for ((i = 0; i < READY_SECONDS * 10; i++)); do
    [[ -s "$scratch/display" ]] && break
    kill -0 "$xvfb_pid" 2>/dev/null || break
    sleep 0.1
done
number="$(tr -dc '0-9' <"$scratch/display")"
if [[ -z "$number" ]]; then
    tail -5 "$scratch/xvfb.log" >&2
    die "Xvfb did not report a display within ${READY_SECONDS}s"
fi

# 2. The bus, with the sealed environment, running this script again, in
# a process group of its own (a background child is not a group leader,
# so setsid runs in it and $! is the group). `<&0` keeps the caller's
# stdin, which a background child would otherwise lose to /dev/null.
mkdir -m 700 "$scratch/run" || die "cannot make a private runtime directory"
env -u WAYLAND_DISPLAY DISPLAY=":$number" GSETTINGS_BACKEND=memory XDG_RUNTIME_DIR="$scratch/run" \
    WITH_DISPLAY_INNER="$scratch" setsid dbus-run-session -- bash "$0" "$@" <&0 &
bus_pid=$!
wait "$bus_pid"
status=$?
[[ -e "$scratch/entered" ]] || die "the private session bus did not start (dbus-run-session exited $status)"
exit "$status"
