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
#      adapter exports its tree only while that reads true. Where the
#      session bus may not execute the launcher (SELinux enforcing, as on
#      Fedora), the wrapper starts it and the registry itself;
#   4. the AT-SPI registry answering on the accessibility bus — the
#      registry is what a test asks for the applications, so a bus with
#      no registry would let a test fail as "the window exported
#      nothing" when the platform under it was what was missing.
# Each step that fails ends the run, named, before the command starts.
# Xvfb is terminated when the wrapper returns; the bus and what it
# activated end with dbus-run-session, and the scratch is removed only
# once that group has ended (bounded by WITH_DISPLAY_STOP_SECONDS, then
# KILL), with any mount left under it unmounted first; what cannot be
# removed is reported. The bus runs in a process group of
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
# Where at-spi2-core installs its programs: /usr/libexec on Fedora and
# Ubuntu 24.04, /usr/lib/at-spi2-core on older Debian. Only the direct
# start (step 3) needs them.
ATSPI_DIRS="${WITH_DISPLAY_ATSPI_DIRS:-/usr/libexec:/usr/lib/at-spi2-core:/usr/libexec/at-spi2-core}"
atspi_program() {
    local dir
    local -a dirs
    IFS=: read -ra dirs <<<"$ATSPI_DIRS"
    for dir in "${dirs[@]}"; do
        [[ -x "$dir/$1" ]] && { echo "$dir/$1"; return 0; }
    done
    return 1
}
[[ $# -gt 0 ]] || die "no command given (--help)"

# INSIDE the bus (re-entered below): steps 3 and 4, then the command.
# The marker tells the outside that the bus came up and this ran.
if [[ -n "${WITH_DISPLAY_INNER:-}" && -d "$WITH_DISPLAY_INNER" ]]; then
    inner="$WITH_DISPLAY_INNER"
    unset WITH_DISPLAY_INNER
    touch "$inner/entered"

    # 3. Accessibility on. Setting the property is what activates the bus
    # launcher through the session bus's service file.
    a11y_on() {
        gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
            --method org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled '<true>' \
            >/dev/null 2>"$inner/a11y.err"
    }
    # THE DIRECT START. On an SELinux-enforcing host (Fedora) the session
    # bus may not execute the launcher (gnome_atspi_exec_t): activation
    # fails with Spawn.ExecFailed, "Permission denied", and the
    # accessibility bus refuses to activate the registry the same way.
    # Started from this shell, both run. Only a Spawn error takes this
    # path — the program could not be started, or died starting
    # (ChildExited, ChildSignaled), and then the direct start fails loudly
    # in its turn; an unknown service (at-spi2-core missing) still ends
    # the run.
    direct=""
    if ! a11y_on; then
        grep -q 'Error.Spawn' "$inner/a11y.err" || {
            cat "$inner/a11y.err" >&2
            die "cannot switch accessibility on (org.a11y.Bus on the session bus) — is at-spi2-core installed?"
        }
        launcher="$(atspi_program at-spi-bus-launcher)" \
            || { cat "$inner/a11y.err" >&2; die "the session bus cannot start the AT-SPI launcher, and at-spi-bus-launcher is not in ${ATSPI_DIRS}"; }
        echo "$me: the session bus cannot start the AT-SPI launcher ($(grep -o 'Error.Spawn[A-Za-z.]*' "$inner/a11y.err" | head -1)); starting it directly" >&2
        "$launcher" >"$inner/launcher.log" 2>&1 &
        for ((i = 0; i < READY_SECONDS * 10; i++)); do
            gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
                --method org.freedesktop.DBus.NameHasOwner org.a11y.Bus 2>/dev/null | grep -q true && break
            sleep 0.1
        done
        a11y_on || { cat "$inner/a11y.err" "$inner/launcher.log" >&2; die "cannot switch accessibility on with the AT-SPI launcher started directly"; }
        direct=yes
    fi
    address="$(gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
        --method org.a11y.Bus.GetAddress 2>"$inner/a11y.err" | sed -n "s/^('\(.*\)',)\$/\1/p")"
    [[ -n "$address" ]] || { cat "$inner/a11y.err" >&2; die "the AT-SPI bus gave no address"; }

    # 4. The registry on that bus — started directly when the launcher
    # had to be, since the accessibility bus refuses to activate it there.
    if [[ -n "$direct" ]]; then
        registryd="$(atspi_program at-spi2-registryd)" \
            || die "at-spi2-registryd is not in ${ATSPI_DIRS}"
        AT_SPI_BUS_ADDRESS="$address" "$registryd" >"$inner/registryd.log" 2>&1 &
    fi
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
# Canonical, as mountinfo lists mount points: a TMPDIR reached through a
# symlink (/home -> /var/home) would otherwise match no mount under it.
scratch="$(realpath -- "$scratch")" || die "cannot resolve the scratch directory"
xvfb_pid=""
bus_pid=""
# When the stop ends in KILL, in milliseconds: set by the first signal,
# here not inherited. EPOCHREALTIME, since SECONDS is whole seconds and
# would cut TERM's grace to nothing for a signal at a second's end.
stop_by=""
now_ms() { local t="${EPOCHREALTIME//[.,]/}"; echo $((t / 1000)); }
# Mount points under $1, deepest first, from mountinfo's fifth field
# (octal-escaped: a space is \040).
mounts_under() {
    local mp
    while read -r _ _ _ _ mp _; do
        mp="$(printf '%b' "$mp")"
        [[ "$mp" == "$1"/* ]] && printf '%s\n' "$mp"
    done <"${WITH_DISPLAY_MOUNTINFO:-/proc/self/mountinfo}" 2>/dev/null | sort -r
}
cleanup() {
    # Further signals are ignored here: a signal's trap ends in exit, and
    # an exit inside the EXIT trap skips the rest of it, the scratch with
    # it. The wait below is bounded, so ignoring them cannot hang.
    trap '' INT TERM HUP
    # The bus's process group, on every exit and not only on a signal: a
    # launcher or registry the wrapper started directly (step 3) that hung
    # before it connected would not end with the bus, and would outlive a
    # refusal. Then WAITED FOR, bounded like a signal's stop, before the
    # scratch goes: what the bus activated may write or unmount under the
    # runtime directory on its way out, and the document portal's FUSE
    # mount at $XDG_RUNTIME_DIR/doc was still up when rm reached it.
    if [[ -n "$bus_pid" ]]; then
        kill -TERM -- "-$bus_pid" 2>/dev/null
        local until=$(($(now_ms) + STOP_SECONDS * 1000))
        while kill -0 -- "-$bus_pid" 2>/dev/null && (($(now_ms) < until)); do sleep 0.1; done
        kill -KILL -- "-$bus_pid" 2>/dev/null
    fi
    if [[ -n "$xvfb_pid" ]]; then kill "$xvfb_pid" 2>/dev/null; wait "$xvfb_pid" 2>/dev/null; fi
    # A mount a KILLed service never took down: rm cannot remove a mount
    # point, and --one-file-system keeps it from deleting through one.
    local mp fuse
    fuse="${WITH_DISPLAY_FUSERMOUNT-$(command -v fusermount3 || command -v fusermount)}"
    while read -r mp; do
        [[ -n "$mp" ]] || continue
        if [[ -n "$fuse" ]]; then "$fuse" -u -z "$mp" 2>/dev/null; else umount -l "$mp" 2>/dev/null; fi
    done < <(mounts_under "$scratch")
    rm -rf --one-file-system "$scratch" 2>/dev/null
    [[ -e "$scratch" ]] && echo "with_display: could not remove its scratch $scratch — left behind" >&2
    return 0
}
trap cleanup EXIT
# A signal ends the bus's whole process group, then the wrapper through
# exit, so the EXIT trap above runs. `wait` below is interrupted by a
# trapped signal, which a foreground child would defer. Before setsid has
# run in the child there is no group yet, and the child itself is ended.
forward() {
    # More signals while this runs (Ctrl-C held down) run it again inside
    # the first, toward the same deadline: however many arrive, KILL comes
    # STOP_SECONDS after the first (the repeated-signal case in
    # test_with_display.sh).
    [[ -n "$stop_by" ]] || stop_by=$(($(now_ms) + STOP_SECONDS * 1000))
    if [[ -n "$bus_pid" ]]; then
        kill -TERM -- "-$bus_pid" 2>/dev/null || kill -TERM "$bus_pid" 2>/dev/null
        while (($(now_ms) < stop_by)); do
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
