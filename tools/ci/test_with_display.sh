#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/ci/test_with_display.sh
#
# Self-test for with_display.sh.
#
# Xvfb, dbus-run-session, dbus-daemon, gdbus and setsid are stubs on PATH, each told by a file
# in the sandbox how to misbehave. Every step the wrapper stands up has a
# case where that step fails, and each such case asserts the command did
# NOT run: a wrapper that ran the tests anyway would hand them a session
# missing the thing they read, and they would fail as the application's
# fault. The real session was measured in an ubuntu:24.04 container (a GTK
# window read back over AT-SPI, 30 runs of 30); this file pins the logic.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/with_display.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

# python3 restores INT's default for the signal cases and stands in for
# setsid's syscall; without it those cases would fail as a missing
# command rather than on the logic, so say so instead.
command -v python3 >/dev/null || { echo "test_with_display: python3 is needed (the signal cases and the setsid stub)" >&2; exit 1; }

failures=0
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT
BIN="$SANDBOX/bin"
mkdir -p "$BIN"

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

# Xvfb: reports display 7 on fd 3 and stays up until killed, recording
# its pid and the SIGTERM; `xvfb-dies` makes it exit first, `xvfb-mute`
# stay up silent. It ends itself after 10s, so a wrapper that only WAITS
# for it would see it gone too -- the terminated marker tells them apart.
cat > "$BIN/Xvfb" <<EOF
#!/usr/bin/env bash
echo \$\$ > "$SANDBOX/xvfb-pid"
[[ -e "$SANDBOX/xvfb-dies" ]] && { echo "Fatal server error: no screens" >&2; exit 1; }
[[ -e "$SANDBOX/xvfb-mute" ]] || echo 7 >&3
sleep 10 >/dev/null 2>&1 &
trap 'touch "$SANDBOX/xvfb-terminated"; kill \$!; exit 0' TERM
wait
EOF
# dbus-run-session: records the environment it was started with (every
# service it activates inherits it), then runs what follows `--`;
# `bus-fails` makes it fail as one that cannot start dbus-daemon does.
cat > "$BIN/dbus-run-session" <<EOF
#!/usr/bin/env bash
{ echo "DISPLAY=\${DISPLAY-unset}"; echo "WAYLAND_DISPLAY=\${WAYLAND_DISPLAY-unset}"
  echo "GSETTINGS_BACKEND=\${GSETTINGS_BACKEND-unset}"; echo "XDG_RUNTIME_DIR=\${XDG_RUNTIME_DIR-unset}"
  echo "XDG_MODE=\$(stat -c %a "\${XDG_RUNTIME_DIR:-/nonexistent}" 2>/dev/null || echo none)"; } > "$SANDBOX/bus-ran"
[[ -e "$SANDBOX/bus-fails" ]] && { echo "dbus-run-session: failed to exec 'dbus-daemon'" >&2; exit 127; }
[[ "\$1" == "--" ]] && shift
export DBUS_SESSION_BUS_ADDRESS=unix:path=$SANDBOX/bus
# A parent of the command, as the real one is (it forks the daemon and the
# command and outlives both), so a signal to it alone does not reach them.
"\$@"
exit \$?
EOF
# gdbus: Set, GetAddress and the registry probe, each breakable.
cat > "$BIN/gdbus" <<EOF
#!/usr/bin/env bash
args="\$*"
case "\$args" in
  *Properties.Set*IsEnabled*)
    [[ -e "$SANDBOX/set-fails" ]] && { echo "Error: ServiceUnknown org.a11y.Bus" >&2; exit 1; }
    touch "$SANDBOX/a11y-on"; echo "()" ;;
  *GetAddress*)
    [[ -e "$SANDBOX/no-address" ]] && { echo "()"; exit 0; }
    echo "('unix:path=$SANDBOX/a11y,guid=1',)" ;;
  *"--address unix:path=$SANDBOX/a11y,guid=1"*ChildCount*)
    [[ -e "$SANDBOX/registry-down" ]] && { echo "Error: ServiceUnknown org.a11y.atspi.Registry" >&2; exit 1; }
    echo "(<0>,)" ;;
  *) echo "gdbus stub: unexpected call: \$args" >&2; exit 64 ;;
esac
EOF
printf '#!/usr/bin/env bash\nexit 0\n' > "$BIN/dbus-daemon"
# setsid: a real new session and process group, so the signal case below
# exercises the group the wrapper signals, on any host.
cat > "$BIN/setsid" <<'EOF'
#!/usr/bin/env bash
exec python3 -c 'import os, sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])' "$@"
EOF
chmod +x "$BIN"/*

# The command under the wrapper: records its environment, exits $1.
CMD="$SANDBOX/cmd"
cat > "$CMD" <<EOF
#!/usr/bin/env bash
{ echo "DISPLAY=\${DISPLAY-unset}"; echo "WAYLAND_DISPLAY=\${WAYLAND_DISPLAY-unset}"
  echo "a11y-before-cmd=\$([[ -e $SANDBOX/a11y-on ]] && echo yes || echo no)"; } > "$SANDBOX/cmd-ran"
exit "\${1:-0}"
EOF
chmod +x "$CMD"

reset() { rm -f "$SANDBOX"/{xvfb-dies,xvfb-mute,bus-fails,set-fails,no-address,registry-down,cmd-ran,bus-ran,a11y-on,xvfb-pid,xvfb-terminated,sleeper-pid}; }

# run [<arg>…]: the wrapper under the stubs, from a Wayland desktop.
run() {
    out="$(PATH="$BIN:$PATH" WAYLAND_DISPLAY=wayland-0 DISPLAY=:99 XDG_RUNTIME_DIR="$SANDBOX/desktop-run" WITH_DISPLAY_READY_SECONDS=1 \
        bash "$UNDER_TEST" "$@" 2>&1)"
    got=$?
}

# refused <name> <says>: exit 125, the step named, the command not run.
refused() {
    if [[ "$got" -ne 125 ]]; then fail "$1 — wanted exit 125, got $got" "$out"; return; fi
    if [[ "$out" != *"$2"* ]]; then fail "$1 — output lacks: $2" "$out"; return; fi
    if [[ -e "$SANDBOX/cmd-ran" ]]; then fail "$1 — the command ran anyway" "$out"; return; fi
    pass "$1 (exit 125, command not run)"
}

echo "test_with_display"

reset; run "$CMD" 3
if [[ "$got" -eq 3 && -e "$SANDBOX/cmd-ran" ]]; then pass "the command runs and its exit status is the wrapper's (3)"
else fail "the command should run and exit 3, got $got" "$out"; fi
if [[ -e "$SANDBOX/bus-ran" ]]; then pass "  inside dbus-run-session"; else fail "dbus-run-session was not used" "$out"; fi
# The bus's own environment is what its activated services inherit: the
# AT-SPI launcher marks DISPLAY's root window and writes the
# toolkit-accessibility setting through GSettings.
for want in 'DISPLAY=:7' 'WAYLAND_DISPLAY=unset' 'GSETTINGS_BACKEND=memory'; do
    if grep -qx "$want" "$SANDBOX/bus-ran" 2>/dev/null; then pass "  the bus is started with $want"
    else fail "the bus should be started with $want" "$(cat "$SANDBOX/bus-ran" 2>/dev/null)"; fi
done
# The accessibility bus puts its socket under XDG_RUNTIME_DIR: a private
# directory, not the desktop's, and gone when the wrapper returns.
rundir="$(sed -n 's/^XDG_RUNTIME_DIR=//p' "$SANDBOX/bus-ran" 2>/dev/null)"
if [[ -n "$rundir" && "$rundir" != "$SANDBOX/desktop-run" && "$rundir" != unset && ! -e "$rundir" ]] \
    && grep -qx 'XDG_MODE=700' "$SANDBOX/bus-ran"; then
    pass "  the bus's XDG_RUNTIME_DIR is private (mode 700 during the run), and removed with it"
else fail "XDG_RUNTIME_DIR should be a private directory removed afterwards, was: ${rundir:-none}"; fi
grep -qx 'DISPLAY=:7' "$SANDBOX/cmd-ran" 2>/dev/null && pass "  DISPLAY is Xvfb's, not the caller's" \
    || fail "DISPLAY should be :7" "$(cat "$SANDBOX/cmd-ran" 2>/dev/null)"
grep -qx 'WAYLAND_DISPLAY=unset' "$SANDBOX/cmd-ran" 2>/dev/null && pass "  WAYLAND_DISPLAY is unset, so winit picks X11" \
    || fail "WAYLAND_DISPLAY should be unset" "$(cat "$SANDBOX/cmd-ran" 2>/dev/null)"
grep -qx 'a11y-before-cmd=yes' "$SANDBOX/cmd-ran" 2>/dev/null && pass "  accessibility was on before the command started" \
    || fail "accessibility was not switched on first" "$(cat "$SANDBOX/cmd-ran" 2>/dev/null)"
pid="$(cat "$SANDBOX/xvfb-pid" 2>/dev/null)"
if [[ -n "$pid" && -e "$SANDBOX/xvfb-terminated" ]] && ! kill -0 "$pid" 2>/dev/null; then pass "  Xvfb is terminated, and gone, when the wrapper returns"
else fail "Xvfb (pid ${pid:-?}) was not terminated by the wrapper"; kill "$pid" 2>/dev/null; fi

reset; run "$CMD" 0
[[ "$got" -eq 0 ]] && pass "a passing command passes (exit 0)" || fail "wanted exit 0, got $got" "$out"

reset; run
refused "no command is a usage refusal" "no command given"

reset; touch "$SANDBOX/xvfb-dies"; run "$CMD"
refused "Xvfb exiting before it reports a display" "Xvfb did not report a display"
[[ "$out" == *"Fatal server error"* ]] && pass "  and Xvfb's own error is shown" || fail "Xvfb's log was not shown" "$out"

reset; touch "$SANDBOX/xvfb-mute"; run "$CMD"
refused "Xvfb never reporting a display, within the bound" "Xvfb did not report a display within 1s"
pid="$(cat "$SANDBOX/xvfb-pid" 2>/dev/null)"
if [[ -n "$pid" && -e "$SANDBOX/xvfb-terminated" ]] && ! kill -0 "$pid" 2>/dev/null; then pass "  and the silent Xvfb is terminated"
else fail "the silent Xvfb (pid ${pid:-?}) was left running"; kill "$pid" 2>/dev/null; fi

reset; touch "$SANDBOX/bus-fails"; run "$CMD"
refused "a session bus that does not start" "the private session bus did not start (dbus-run-session exited 127)"
if [[ -e "$SANDBOX/xvfb-terminated" ]]; then pass "  and Xvfb is terminated"; else fail "Xvfb was not terminated after the bus failed"; fi

reset; touch "$SANDBOX/set-fails"; run "$CMD"
refused "accessibility that cannot be switched on" "cannot switch accessibility on"

reset; touch "$SANDBOX/no-address"; run "$CMD"
refused "an AT-SPI bus with no address" "the AT-SPI bus gave no address"

reset; touch "$SANDBOX/registry-down"; run "$CMD"
refused "an AT-SPI registry that never answers" "the AT-SPI registry did not answer within 1s"

# A signal sent to the wrapper ALONE, mid-command, as `kill <pid>` or a
# terminal's Ctrl-C sends it: the command is ended at once, not left to
# finish, and Xvfb with it. The wrapper is started through a launcher
# that restores INT's default, since a background job of this script
# starts with INT ignored and a shell cannot trap what it began ignoring.
# sig_case <signal> <exit> <command> <name> [<stop-seconds>]; SIG_REPEAT=N
# sends the signal N more times, 0.3s apart (Ctrl-C held down during the
# stop), and the caller's environment carries `stop_by` and `stopping`,
# which the wrapper must not take for its own state.
sig_case() {
    local sig="$1" want="$2" cmd="$3" name="$4" stop="${5:-5}" wrapper got took start sleeper
    reset
    stop_by=0 stopping=1 PATH="$BIN:$PATH" WITH_DISPLAY_READY_SECONDS=1 WITH_DISPLAY_STOP_SECONDS="$stop" \
        python3 -c 'import os, signal, sys; signal.signal(signal.SIGINT, signal.SIG_DFL); os.execvp(sys.argv[1], sys.argv[1:])' \
        bash "$UNDER_TEST" "$cmd" >/dev/null 2>&1 &
    wrapper=$!
    for ((i = 0; i < 50; i++)); do [[ -s "$SANDBOX/sleeper-pid" ]] && break; sleep 0.1; done
    start=$SECONDS
    kill "-$sig" "$wrapper"
    # Repeats: one more signal every 0.3s, polling every 0.1s for when the
    # command died, in milliseconds after the first signal.
    local t0 died_ms="" r
    t0=$(date +%s%3N)
    for ((r = 1; r <= ${SIG_REPEAT:-0} * 3; r++)); do
        sleep 0.1
        (( r % 3 == 0 )) && kill "-$sig" "$wrapper" 2>/dev/null
        if [[ -z "$died_ms" ]] && [[ -s "$SANDBOX/sleeper-pid" ]] && ! kill -0 "$(cat "$SANDBOX/sleeper-pid")" 2>/dev/null; then
            died_ms=$(( $(date +%s%3N) - t0 ))
        fi
    done
    if [[ -n "${SIG_REPEAT:-}" ]]; then
        # KILL at the first signal's deadline: not after it (a deadline each
        # signal restarts), and not before it (TERM's grace cut short).
        if [[ -n "$died_ms" && "$died_ms" -ge $(( stop * 1000 - 300 )) && "$died_ms" -le $(( stop * 1000 + 700 )) ]]; then
            pass "  the command died ${died_ms}ms after the first signal, at its deadline"
        else fail "  the command should die at the first signal's deadline (${stop}s), died at ${died_ms:-never}ms"; fi
    fi
    wait "$wrapper"; got=$?
    took=$((SECONDS - start))
    sleeper="$(cat "$SANDBOX/sleeper-pid" 2>/dev/null)"
    # At once means well inside the stop bound, which falls back to KILL;
    # only a command that ignores TERM may take the bound itself.
    local limit=2; [[ "$cmd" == "$STUBBORN" ]] && limit=$((stop + 2))
    if [[ "$got" -eq "$want" && "$took" -le "$limit" ]]; then pass "$name (exit $got, ${took}s)"
    else fail "$name — wanted exit $want within ${limit}s, got $got after ${took}s"; fi
    if [[ -n "$sleeper" ]] && ! kill -0 "$sleeper" 2>/dev/null; then pass "  and the command was ended with it"
    else fail "  the command (pid ${sleeper:-?}) outlived the wrapper"; kill -KILL "$sleeper" 2>/dev/null; fi
    if [[ -e "$SANDBOX/xvfb-terminated" ]]; then pass "  and Xvfb was terminated"; else fail "  Xvfb was not terminated after $sig"; fi
}
SLEEPER="$SANDBOX/sleeper"
printf '#!/usr/bin/env bash\necho $$ > "%s/sleeper-pid"\nexec sleep 30\n' "$SANDBOX" > "$SLEEPER"
STUBBORN="$SANDBOX/stubborn"
printf '#!/usr/bin/env bash\ntrap "" TERM\necho $$ > "%s/sleeper-pid"\nsleep 30 & wait\n' "$SANDBOX" > "$STUBBORN"
chmod +x "$SLEEPER" "$STUBBORN"
# A stop bound of 10s, so a regression that waits it out cannot pass as "at once".
sig_case TERM 143 "$SLEEPER" "TERM to the wrapper alone ends the command at once" 10
sig_case INT 130 "$SLEEPER" "INT to the wrapper alone (Ctrl-C) ends the command at once" 10
sig_case HUP 129 "$SLEEPER" "HUP to the wrapper alone ends the command at once" 10
sig_case TERM 143 "$STUBBORN" "a command that ignores TERM is killed once the stop bound passes" 1
# Eight more INTs 0.3s apart span 2.4s: KILL must still come at the first
# one's 1s deadline, inside the 3s limit, not after the last one.
SIG_REPEAT=8 sig_case INT 130 "$STUBBORN" "Ctrl-C held down keeps the first signal's deadline, and kills" 1

# A tool missing from PATH: a PATH holding only the other stubs. The
# wrapper reaches its tool check on builtins alone, so nothing else is
# needed, and a host that has the real tool installed cannot mask the case.
BASH_BIN="$(command -v bash)"
for tool in Xvfb dbus-run-session dbus-daemon gdbus setsid; do
    reset
    mkdir -p "$SANDBOX/partial"; rm -f "$SANDBOX/partial"/*
    for t in Xvfb dbus-run-session dbus-daemon gdbus setsid; do [[ "$t" == "$tool" ]] || ln -s "$BIN/$t" "$SANDBOX/partial/$t"; done
    out="$(PATH="$SANDBOX/partial" "$BASH_BIN" "$UNDER_TEST" "$CMD" 2>&1)"; got=$?
    refused "$tool missing is named" "$tool not found"
    [[ -e "$SANDBOX/bus-ran" ]] && fail "  $tool missing: the bus was started anyway"
done

help_out="$(bash "$UNDER_TEST" --help 2>/dev/null)"
[[ "$help_out" == *"Plan §18 (Stage 15)"* ]] && pass "--help prints the help block" || fail "--help should print the help block" "$help_out"

echo
if (( failures > 0 )); then
    echo "test_with_display: $failures failure(s)" >&2
    exit 1
fi
echo "test_with_display: OK — all assertions passed."
