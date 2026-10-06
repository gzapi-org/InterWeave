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
SANDBOX_TMPDIR="${TMPDIR-}"
# Canonical, as the wrapper's scratch is: a TMPDIR behind a symlink
# would otherwise fail every comparison of the two paths.
SANDBOX="$(realpath -- "$(mktemp -d)")"
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
# xvfb-stubborn: ignores TERM, as the process itself (exec keeps the
# pid and the ignored TERM), so the wrapper's KILL leaves no child.
[[ -e "$SANDBOX/xvfb-stubborn" ]] && { trap '' TERM; exec sleep 10 >/dev/null 2>&1; }
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
rc=\$?
touch "$SANDBOX/bus-exited"
exit \$rc
EOF
# gdbus: Set, GetAddress and the registry probe, each breakable.
cat > "$BIN/gdbus" <<EOF
#!/usr/bin/env bash
args="\$*"
case "\$args" in
  *Properties.Set*IsEnabled*)
    [[ -e "$SANDBOX/set-fails" ]] && { echo "Error: ServiceUnknown org.a11y.Bus" >&2; exit 1; }
    # An SELinux-enforcing host: the session bus may not execute the
    # launcher, until one is running that the wrapper started itself.
    [[ -e "$SANDBOX/spawn-denied" && ! -e "$SANDBOX/launcher-started" ]] && {
      echo "Error: GDBus.Error:org.freedesktop.DBus.Error.Spawn.ExecFailed: Failed to execute program org.a11y.Bus: Permission denied" >&2; exit 1; }
    touch "$SANDBOX/a11y-on"; echo "()" ;;
  *GetAddress*)
    [[ -e "$SANDBOX/no-address" ]] && { echo "()"; exit 0; }
    echo "('unix:path=$SANDBOX/a11y,guid=1',)" ;;
  *NameHasOwner*org.a11y.Bus*)
    [[ -e "$SANDBOX/launcher-started" ]] && echo "(true,)" || echo "(false,)" ;;
  *"--address unix:path=$SANDBOX/a11y,guid=1"*ChildCount*)
    [[ -e "$SANDBOX/registry-down" ]] && { echo "Error: ServiceUnknown org.a11y.atspi.Registry" >&2; exit 1; }
    [[ -e "$SANDBOX/spawn-denied" && ! -e "$SANDBOX/registryd-started" ]] && {
      echo "Error: GDBus.Error:org.freedesktop.DBus.Error.Spawn.ExecFailed: Failed to execute program org.a11y.atspi.Registry: Permission denied" >&2; exit 1; }
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

# at-spi2-core's programs, for the direct start: the launcher takes
# org.a11y.Bus (`launcher-mute`: it never does), the registry records the
# accessibility bus it was pointed at. Neither runs on the activation path.
ATSPI="$SANDBOX/libexec"
mkdir -p "$ATSPI"
cat > "$ATSPI/at-spi-bus-launcher" <<EOF
#!/usr/bin/env bash
touch "$SANDBOX/launcher-ran"
# launcher-hangs: never takes the name and never exits, as one stuck
# before it connects would.
[[ -e "$SANDBOX/launcher-hangs" ]] && { echo \$\$ > "$SANDBOX/launcher-pid"; exec sleep 300; }
[[ -e "$SANDBOX/launcher-mute" ]] || touch "$SANDBOX/launcher-started"
EOF
cat > "$ATSPI/at-spi2-registryd" <<EOF
#!/usr/bin/env bash
echo "\${AT_SPI_BUS_ADDRESS-unset}" > "$SANDBOX/registryd-started"
EOF
chmod +x "$ATSPI"/*

# The command under the wrapper: records its environment, exits $1.
CMD="$SANDBOX/cmd"
cat > "$CMD" <<EOF
#!/usr/bin/env bash
{ echo "DISPLAY=\${DISPLAY-unset}"; echo "WAYLAND_DISPLAY=\${WAYLAND_DISPLAY-unset}"
  echo "a11y-before-cmd=\$([[ -e $SANDBOX/a11y-on ]] && echo yes || echo no)"; } > "$SANDBOX/cmd-ran"
exit "\${1:-0}"
EOF
chmod +x "$CMD"

# What the bus activated, outliving the command: `late` starts a service
# in the bus's group that, on TERM, takes half a second and then writes
# into the runtime directory (the document portal unmounting its FUSE
# mount there); `mounted` lists a mount under the runtime directory (and
# one elsewhere) in the mountinfo the wrapper reads; `mixed` lists a
# FUSE mount and a tmpfs one. (A scratch that cannot be removed is an rm
# stub below, not a mode.)
# Each records the runtime directory.
CMD2="$SANDBOX/cmd2"
cat > "$CMD2" <<EOF
#!/usr/bin/env bash
echo "\$XDG_RUNTIME_DIR" > "$SANDBOX/rundir"
case "\${1:-}" in
  late)
    ( trap 'sleep 0.5; mkdir -p "\$XDG_RUNTIME_DIR/doc"; touch "\$XDG_RUNTIME_DIR/doc/late"; exit 0' TERM
      sleep 30 & wait ) >/dev/null 2>&1 &
    sleep 0.2 ;;
  mounted)
    mkdir -p "\$XDG_RUNTIME_DIR/doc/by-app"
    # The kernel lists mount points canonically.
    rd="\$(realpath "\$XDG_RUNTIME_DIR")"
    { echo "36 25 0:32 / \$rd/doc rw - fuse.portal portal rw"
      echo "37 36 0:33 / \$rd/doc/by-app rw - fuse.portal portal rw"
      echo "38 25 0:34 / /run/user/1000/doc rw - fuse.portal portal rw"; } > "$SANDBOX/mountinfo" ;;
  stubborn)
    ( trap '' TERM; sleep 30 ) >/dev/null 2>&1 &
    echo \$! > "$SANDBOX/stubborn-pid" ;;
  mixed)
    rd="\$(realpath "\$XDG_RUNTIME_DIR")"; mkdir -p "\$XDG_RUNTIME_DIR/doc" "\$XDG_RUNTIME_DIR/t"
    { echo "36 25 0:32 / \$rd/doc rw - fuse.portal portal rw"
      echo "40 25 0:35 / \$rd/t rw shared:1 - tmpfs tmpfs rw"; } > "$SANDBOX/mountinfo" ;;
esac
exit 0
EOF
chmod +x "$CMD2"
# umount: the fallback where no fusermount is installed.
cat > "$BIN/umount" <<EOF
#!/usr/bin/env bash
echo "umount \$*" >> "$SANDBOX/unmounted"
EOF
# fusermount3: records each unmount it was asked for.
cat > "$BIN/fusermount3" <<EOF
#!/usr/bin/env bash
echo "\$*" >> "$SANDBOX/unmounted"
[[ -e "$SANDBOX/fusermount-fails" ]] && exit 1
exit 0
EOF
chmod +x "$BIN/fusermount3" "$BIN/umount"

reset() { rm -f "$SANDBOX"/{xvfb-dies,xvfb-mute,bus-fails,set-fails,no-address,registry-down,cmd-ran,bus-ran,a11y-on,xvfb-pid,xvfb-terminated,sleeper-pid,spawn-denied,launcher-mute,launcher-hangs,launcher-pid,launcher-ran,launcher-started,registryd-started,rundir,mountinfo,unmounted,stubborn-pid,rm-args,bus-exited,xvfb-stubborn,fusermount-fails,rm-refuses}; ATSPI_DIRS="$ATSPI"; }

# run [<arg>…]: the wrapper under the stubs, from a Wayland desktop.
run() {
    out="$(PATH="$BIN:$PATH" WAYLAND_DISPLAY=wayland-0 DISPLAY=:99 XDG_RUNTIME_DIR="$SANDBOX/desktop-run" WITH_DISPLAY_READY_SECONDS=1 \
        WITH_DISPLAY_ATSPI_DIRS="$ATSPI_DIRS" \
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

if [[ ! -e "$SANDBOX/launcher-ran" && ! -e "$SANDBOX/registryd-started" ]]; then pass "  where activation works, nothing is started directly"
else fail "the launcher or the registry was started directly although activation worked" "$out"; fi

reset; run "$CMD" 0
[[ "$got" -eq 0 ]] && pass "a passing command passes (exit 0)" || fail "wanted exit 0, got $got" "$out"

# The direct start: the session bus may not execute the launcher, nor the
# accessibility bus the registry (SELinux enforcing, as on Fedora).
reset; touch "$SANDBOX/spawn-denied"; run "$CMD" 0
if [[ "$got" -eq 0 && -e "$SANDBOX/cmd-ran" ]]; then pass "activation denied: the launcher and registry are started directly, and the command runs"
else fail "with activation denied the command should run, got $got" "$out"; fi
[[ "$out" == *"cannot start the AT-SPI launcher (Error.Spawn.ExecFailed); starting it directly"* ]] \
    && pass "  and it says so, naming the error" || fail "the direct start was not announced" "$out"
grep -qx "unix:path=$SANDBOX/a11y,guid=1" "$SANDBOX/registryd-started" 2>/dev/null \
    && pass "  the registry is pointed at the accessibility bus's address" \
    || fail "the registry was not started on the accessibility bus" "$(cat "$SANDBOX/registryd-started" 2>/dev/null)"
grep -qx 'a11y-before-cmd=yes' "$SANDBOX/cmd-ran" 2>/dev/null && pass "  accessibility was on before the command started" \
    || fail "accessibility was not switched on first" "$(cat "$SANDBOX/cmd-ran" 2>/dev/null)"

reset; touch "$SANDBOX/spawn-denied" "$SANDBOX/launcher-mute"; run "$CMD"
refused "a launcher started directly that never takes the bus name" "cannot switch accessibility on with the AT-SPI launcher started directly"

# A launcher started directly that hangs is ended with the run, whichever
# way the run ends — here a refusal, which sends the wrapper no signal.
reset; touch "$SANDBOX/spawn-denied" "$SANDBOX/launcher-hangs"; run "$CMD"
refused "a launcher started directly that hangs" "cannot switch accessibility on with the AT-SPI launcher started directly"
pid="$(cat "$SANDBOX/launcher-pid" 2>/dev/null)"
sleep 0.3
if [[ -n "$pid" ]] && ! kill -0 "$pid" 2>/dev/null; then pass "  and the hung launcher does not outlive the run"
else fail "the hung launcher (pid ${pid:-?}) outlived the run"; [[ -n "$pid" ]] && kill "$pid" 2>/dev/null; fi

reset; touch "$SANDBOX/spawn-denied"; ATSPI_DIRS="$SANDBOX/nowhere"; run "$CMD"
refused "activation denied and no launcher to start" "at-spi-bus-launcher is not in $SANDBOX/nowhere"

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
[[ ! -e "$SANDBOX/launcher-ran" ]] && pass "  an unknown service is not a reason to start the launcher directly" \
    || fail "the launcher was started for an unknown service" "$out"

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


# The scratch goes only once the bus's group has ended: a service it
# activated may still write or unmount under the runtime directory as it
# exits (a run left <scratch>/run/doc behind, 2026-10-06).
reset; run "$CMD2" late
# Look after the writer's half second: a wrapper that removed the scratch
# without waiting would see it come back only then.
sleep 1
rundir="$(cat "$SANDBOX/rundir" 2>/dev/null)"
if [[ "$got" -eq 0 && -n "$rundir" && ! -e "$rundir" && ! -e "${rundir%/run}" ]]; then
    pass "a service writing into the runtime directory after TERM leaves nothing behind"
else fail "the scratch outlived a late writer (exit $got): ${rundir:-none}" "$(ls -R "${rundir%/run}" 2>&1 | head -5)"; rm -rf "${rundir%/run}"; fi

# A mount a service left up is unmounted, lazily and deepest first, before
# rm; a mount outside the scratch is not touched.
reset; export WITH_DISPLAY_MOUNTINFO="$SANDBOX/mountinfo"; run "$CMD2" mounted; unset WITH_DISPLAY_MOUNTINFO
rundir="$(cat "$SANDBOX/rundir" 2>/dev/null)"
if [[ "$(cat "$SANDBOX/unmounted" 2>/dev/null)" == "-u -z $rundir/doc/by-app"$'\n'"-u -z $rundir/doc" ]]; then
    pass "mounts under the scratch are unmounted (-u -z), deepest first, and no other"
else fail "the unmounts were wrong" "$(cat "$SANDBOX/unmounted" 2>/dev/null || echo none)"; fi
[[ ! -e "${rundir%/run}" ]] && pass "  and the scratch is removed" || { fail "the scratch was left after the unmounts"; rm -rf "${rundir%/run}"; }

# What cannot be removed is said, never silent. An rm that refuses the
# scratch stands for whatever kept it (a permission, a mount): for every
# uid, root included, which a read-only directory would not stop.
reset; touch "$SANDBOX/rm-refuses"
printf '#!/usr/bin/env bash\n[[ -e "%s/rm-refuses" && " $* " == *" --one-file-system "* ]] && exit 1\nexec "%s" "$@"\n' "$SANDBOX" "$(PATH=/usr/bin:/bin command -v rm)" > "$BIN/rm"; chmod +x "$BIN/rm"
run "$CMD2"; rm -f "$BIN/rm" "$SANDBOX/rm-refuses"
rundir="$(cat "$SANDBOX/rundir" 2>/dev/null)"
if [[ "$got" -eq 0 && "$out" == *"could not remove its scratch ${rundir%/run} — left behind"* ]]; then
    pass "a scratch that cannot be removed is reported, and the command's status kept"
else fail "a leftover scratch was not reported (exit $got)" "$out"; fi
rm -rf "${rundir%/run}"

# fusermount only for FUSE; any other type, or a fusermount that fails,
# takes umount -l.
reset; export WITH_DISPLAY_MOUNTINFO="$SANDBOX/mountinfo"; run "$CMD2" mixed; unset WITH_DISPLAY_MOUNTINFO
rundir="$(cat "$SANDBOX/rundir" 2>/dev/null)"
if [[ "$(cat "$SANDBOX/unmounted" 2>/dev/null)" == "umount -l $rundir/t"$'\n'"-u -z $rundir/doc" ]]; then
    pass "a tmpfs mount takes umount -l, a FUSE one fusermount"
else fail "the unmount commands by type were wrong" "$(cat "$SANDBOX/unmounted" 2>/dev/null || echo none)"; fi
reset; touch "$SANDBOX/fusermount-fails"; export WITH_DISPLAY_MOUNTINFO="$SANDBOX/mountinfo"; run "$CMD2" mixed; unset WITH_DISPLAY_MOUNTINFO
rundir="$(cat "$SANDBOX/rundir" 2>/dev/null)"
grep -qx "umount -l $rundir/doc" "$SANDBOX/unmounted" 2>/dev/null && pass "  and a fusermount that fails falls back to umount -l" \
    || fail "a failing fusermount did not fall back" "$(cat "$SANDBOX/unmounted" 2>/dev/null || echo none)"


# A service that ignores TERM: the wait is bounded by
# WITH_DISPLAY_STOP_SECONDS, then KILL, and the scratch still goes.
reset; start=$SECONDS; export WITH_DISPLAY_STOP_SECONDS=1; run "$CMD2" stubborn; unset WITH_DISPLAY_STOP_SECONDS
took=$((SECONDS - start)); spid="$(cat "$SANDBOX/stubborn-pid" 2>/dev/null)"; rundir="$(cat "$SANDBOX/rundir" 2>/dev/null)"
if [[ "$got" -eq 0 && "$took" -le 4 && -n "$spid" ]] && ! kill -0 "$spid" 2>/dev/null && [[ ! -e "${rundir%/run}" ]]; then
    pass "a service ignoring TERM is KILLed after the bound (${took}s), and the scratch goes"
else fail "a TERM-ignoring service: exit $got, ${took}s, pid ${spid:-?} alive=$(kill -0 "$spid" 2>/dev/null && echo yes || echo no)" "$out"
     pkill -KILL -P "$spid" 2>/dev/null; kill -KILL "$spid" 2>/dev/null; rm -rf "${rundir%/run}"; fi

# A signal while cleanup waits does not abandon it: the scratch goes and
# the command's status stands.
reset
PATH="$BIN:$PATH" WITH_DISPLAY_READY_SECONDS=1 WITH_DISPLAY_STOP_SECONDS=2 WITH_DISPLAY_ATSPI_DIRS="$ATSPI_DIRS" \
    bash "$UNDER_TEST" "$CMD2" stubborn >/dev/null 2>&1 &
wp=$!
# The bus has exited, so the wrapper is past its own wait and in cleanup,
# held there by the stubborn service.
for ((i = 0; i < 50; i++)); do [[ -e "$SANDBOX/bus-exited" ]] && break; sleep 0.1; done
sleep 0.2; kill -TERM "$wp" 2>/dev/null; wait "$wp"; got=$?
spid="$(cat "$SANDBOX/stubborn-pid" 2>/dev/null)"; rundir="$(cat "$SANDBOX/rundir" 2>/dev/null)"
if [[ "$got" -eq 0 && -n "$rundir" && ! -e "${rundir%/run}" ]]; then
    pass "TERM during cleanup's wait: the scratch still goes, and the status is the command's"
else fail "TERM during cleanup: exit $got, scratch ${rundir%/run} left=$([[ -e "${rundir%/run}" ]] && echo yes || echo no)"
     pkill -KILL -P "$spid" 2>/dev/null; kill -KILL "$spid" 2>/dev/null; rm -rf "${rundir%/run}"; fi
pkill -KILL -P "$spid" 2>/dev/null; kill -KILL "$spid" 2>/dev/null

# The removal never deletes through a mount point.
reset
printf '#!/usr/bin/env bash\necho "$*" >> "%s/rm-args"\nexec "%s" "$@"\n' "$SANDBOX" "$(PATH=/usr/bin:/bin command -v rm)" > "$BIN/rm"; chmod +x "$BIN/rm"
run "$CMD2"; rm -f "$BIN/rm"
rundir="$(cat "$SANDBOX/rundir" 2>/dev/null)"
grep -qxF -- "-rf --one-file-system ${rundir%/run}" "$SANDBOX/rm-args" 2>/dev/null \
    && pass "the scratch is removed with --one-file-system" \
    || fail "the scratch's rm lacked --one-file-system" "$(cat "$SANDBOX/rm-args" 2>/dev/null)"

# No fusermount installed: umount -l, deepest first.
reset; export WITH_DISPLAY_MOUNTINFO="$SANDBOX/mountinfo" WITH_DISPLAY_FUSERMOUNT=""
run "$CMD2" mounted; unset WITH_DISPLAY_MOUNTINFO WITH_DISPLAY_FUSERMOUNT
rundir="$(cat "$SANDBOX/rundir" 2>/dev/null)"
if [[ "$(cat "$SANDBOX/unmounted" 2>/dev/null)" == "umount -l $rundir/doc/by-app"$'\n'"umount -l $rundir/doc" ]]; then
    pass "with no fusermount, umount -l takes the mounts down, deepest first"
else fail "the umount fallback was wrong" "$(cat "$SANDBOX/unmounted" 2>/dev/null || echo none)"; fi

# A TMPDIR reached through a symlink: mountinfo is canonical, so the
# scratch must be too, or no mount under it matches.
reset; mkdir -p "$SANDBOX/realtmp"; ln -sfn "$SANDBOX/realtmp" "$SANDBOX/tmplink"
export TMPDIR="$SANDBOX/tmplink" WITH_DISPLAY_MOUNTINFO="$SANDBOX/mountinfo"
run "$CMD2" mounted; unset WITH_DISPLAY_MOUNTINFO
if [[ -n "$SANDBOX_TMPDIR" ]]; then export TMPDIR="$SANDBOX_TMPDIR"; else unset TMPDIR; fi
rundir="$(cat "$SANDBOX/rundir" 2>/dev/null)"
if [[ "$rundir" == "$SANDBOX/realtmp/"* && "$(wc -l < "$SANDBOX/unmounted" 2>/dev/null)" -eq 2 ]]; then
    pass "a symlinked TMPDIR: the scratch is canonical and its mounts are found"
else fail "a symlinked TMPDIR hid the mounts (runtime dir ${rundir:-none})" "$(cat "$SANDBOX/unmounted" 2>/dev/null || echo none)"; fi


# An Xvfb that ignores TERM: cleanup's wait on it is bounded too, then KILL.
reset; touch "$SANDBOX/xvfb-stubborn"; start=$SECONDS
export WITH_DISPLAY_STOP_SECONDS=1; run "$CMD" 0; unset WITH_DISPLAY_STOP_SECONDS
took=$((SECONDS - start)); pid="$(cat "$SANDBOX/xvfb-pid" 2>/dev/null)"
if [[ "$got" -eq 0 && "$took" -le 4 && -n "$pid" ]] && ! kill -0 "$pid" 2>/dev/null; then
    pass "an Xvfb ignoring TERM is KILLed after the bound (${took}s)"
else fail "a TERM-ignoring Xvfb: exit $got, ${took}s, pid ${pid:-?}" "$out"; kill -KILL "$pid" 2>/dev/null; fi

echo
if (( failures > 0 )); then
    echo "test_with_display: $failures failure(s)" >&2
    exit 1
fi
echo "test_with_display: OK — all assertions passed."
