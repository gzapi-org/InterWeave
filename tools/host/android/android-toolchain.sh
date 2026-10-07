#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/host/android/android-toolchain.sh
#
# The shared Android toolchain at /opt/android-sdk: the SDK, NDK and the
# JDK the Android Gradle Plugin runs on, exactly as android-toolchain.pins
# names them. One install per host, so every host can be checked against
# the pins.
#
#   --stage [--dest DIR]  in an AppVM (has network): download and verify,
#                         install the SDK packages into a staging tree with
#                         sdkmanager, check every revision, write the
#                         manifest, pack one archive + its .sha256
#   --install ARCHIVE     as root: verify the archive and its manifest
#                         against the pins, install SDK_DIR read-only for
#                         every account, write /etc/profile.d
#   --check               any host, read-only: is SDK_DIR what the pins say
#
# ON A QUBES AppVM, /opt and /etc are on the root volume, which is
# discarded at shutdown. --install keeps them with Qubes bind-dirs: the
# tree lives in /rw/bind-dirs/opt/android-sdk, a file in
# /rw/config/qubes-bind-dirs.d/ has Qubes bind-mount it onto
# /opt/android-sdk (and the profile file onto /etc/profile.d) at every
# boot, and --install mounts both at once so no restart is needed. In a
# TemplateVM, a StandaloneVM or any other Linux host, SDK_DIR persists and
# is written directly.
#
# READ-ONLY, NO GROUP. Every package is installed and every licence
# accepted at staging, so a build only reads the SDK; owned by root, it
# needs no shared group (a group made in an AppVM would not survive its
# reboot either).
#
# WHAT EACH ACCOUNT STILL DOES. Run rust-android.sh once for rustup, the
# Android Rust target and cargo-ndk, which live in the home.
#
# WHAT IS NOT CHANGED. Nobody's JAVA_HOME or PATH: the host JDK stays the
# default (a Gradle build names $ANDROID_JDK_HOME), and the SDK's adb is
# not put ahead of the system's, since two adb servers of different
# versions fight over the device.
#
# Exit codes: 0 done / the install matches the pins; 1 a check failed or
# the install drifted (each named); 2 usage, environment or refusal.

set -uo pipefail

HERE="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
PINS_FILE="${ANDROID_TOOLCHAIN_PINS:-$HERE/android-toolchain.pins}"
me="android-toolchain"
STAGE_DIR="${ANDROID_TOOLCHAIN_STAGE_DIR:-$HOME/android-staging}"   # not /tmp: a RAM tmpfs here
PROFILE="${ANDROID_TOOLCHAIN_PROFILE:-/etc/profile.d/android-sdk.sh}"

die()  { echo "$me: $*" >&2; exit 2; }
say()  { printf '%s\n' "$*"; }
ok()   { printf '  OK    %s\n' "$*"; }
PROBLEMS=0
bad()  { printf '  FAIL  %s\n' "$*"; PROBLEMS=$((PROBLEMS + 1)); }

MODE="" ARCHIVE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --stage)   MODE=stage; shift ;;
        --install) [[ $# -ge 2 ]] || die "--install needs an archive path"; MODE=install; ARCHIVE="$2"; shift 2 ;;
        --check)   MODE=check; shift ;;
        --dest)    [[ $# -ge 2 ]] || die "--dest needs a directory"; STAGE_DIR="$2"; shift 2 ;;
        -h|--help) awk 'NR > 3 && !/^#/ { exit } NR > 3 { sub(/^# ?/, ""); print }' "$0"; exit 0 ;;
        *) die "unknown argument '$1' (--help)" ;;
    esac
done
[[ -n "$MODE" ]] || die "one of --stage, --install ARCHIVE, --check (--help)"

# ── the pins: KEY=VALUE lines, read and never sourced ────────────────
declare -A PIN
# Each value has a form, checked before anything uses it: --install runs
# as root and writes SDK_DIR and the NDK's path into a profile every login
# sources, so a value is never trusted to be only what it looks like.
valid_pin() {
    local k="$1" v="$2"
    case "$k" in
        SDK_DIR)  [[ "$v" =~ ^/[A-Za-z0-9._/-]+$ && "/$v/" != */../* ]] ;;
        *_URL)    [[ "$v" =~ ^https://[A-Za-z0-9._~:/?\&=+%@-]+$ ]] ;;
        *_SHA1)   [[ "$v" =~ ^[0-9a-f]{40}$ ]] ;;
        *_SHA256) [[ "$v" =~ ^[0-9a-f]{64}$ ]] ;;
        PKG_*)    [[ "$v" =~ ^[A-Za-z0-9._-]+(\;[A-Za-z0-9._-]+)*@[0-9]+(\.[0-9]+)*$ ]] ;;
        *)        [[ "$v" =~ ^[A-Za-z0-9._+-]+$ ]] ;;
    esac
}
read_pins() {
    local line key value n=0
    [[ -r "$1" ]] || die "cannot read the pins at $1"
    while IFS= read -r line || [[ -n "$line" ]]; do
        n=$((n + 1))
        [[ -z "$line" || "$line" == \#* ]] && continue
        [[ "$line" =~ ^([A-Z][A-Z0-9_]*)=(.+)$ ]] || die "$1:$n is not KEY=VALUE: $line"
        # Taken before valid_pin, whose own matches overwrite BASH_REMATCH.
        key="${BASH_REMATCH[1]}" value="${BASH_REMATCH[2]}"
        valid_pin "$key" "$value" || die "$1:$n: the value of $key is not of its form"
        PIN["$key"]="$value"
    done < "$1"
    local k
    for k in SDK_DIR CMDLINE_TOOLS_URL CMDLINE_TOOLS_SHA1 CMDLINE_TOOLS_REVISION JDK_VERSION JDK_URL JDK_SHA256 PKG_1; do
        [[ -n "${PIN[$k]:-}" ]] || die "$1 has no $k"
    done
}
# The PKG_<n> entries in order: "<sdkmanager path>@<revision>".
pkgs() { local k; for k in $(printf '%s\n' "${!PIN[@]}" | grep -E '^PKG_[0-9]+$' | LC_ALL=C sort -t_ -k2 -n); do printf '%s\n' "${PIN[$k]}"; done; }
# The pins as the manifest records them: every KEY=VALUE line, sorted
# bytewise. Never the locale's order: the AppVM that stages and the
# host that installs need not share a locale, and a different order
# would refuse a good archive as "staged from other pins".
pins_canonical() { local k; for k in "${!PIN[@]}"; do printf '%s=%s\n' "$k" "${PIN[$k]}"; done | LC_ALL=C sort; }

read_pins "$PINS_FILE"
SDK_DIR="${ANDROID_TOOLCHAIN_SDK_DIR:-${PIN[SDK_DIR]}}"
BIND_ROOT="${ANDROID_TOOLCHAIN_BIND_ROOT:-/rw/bind-dirs}"
BIND_CONF="${ANDROID_TOOLCHAIN_BIND_CONF:-/rw/config/qubes-bind-dirs.d/50_android-sdk.conf}"
# How this machine keeps what is written to its root volume, the key
# Qubes' own bind-dirs decides on (/usr/lib/qubes/init/functions):
#   ""        not Qubes: everything persists
#   full      a TemplateVM or StandaloneVM: everything persists
#   rw-only   a template-based AppVM: only /rw (and /home) persist
#   dispvm    a DispVM (/type): nothing persists
#   ?         Qubes, but unreadable — never guessed
# Set (even empty) in the environment, that value is used (tests).
persistence() {
    if [[ -n "${ANDROID_TOOLCHAIN_PERSISTENCE+set}" ]]; then printf '%s' "$ANDROID_TOOLCHAIN_PERSISTENCE"; return; fi
    command -v qubesdb-read >/dev/null || return 0
    [[ "$(qubesdb-read /type 2>/dev/null)" == DispVM ]] && { printf dispvm; return; }
    local p; p="$(qubesdb-read /qubes-vm-persistence 2>/dev/null || true)"; printf '%s' "${p:-?}"
}
MANIFEST_NAME=".android-toolchain.manifest"

# A package's revision as sdkmanager records it in <dir>/package.xml:
# major[.minor[.micro]] joined with dots.
pkg_revision() {
    local xml="$1/package.xml"
    [[ -r "$xml" ]] || return 1
    python3 - "$xml" <<'PY'
import sys, xml.etree.ElementTree as ET
root = ET.parse(sys.argv[1]).getroot()
rev = next((e for e in root.iter() if e.tag.split('}')[-1] == 'revision'), None)
if rev is None: sys.exit(1)
parts = [c.text for c in rev if c.tag.split('}')[-1] in ('major', 'minor', 'micro') and c.text]
print('.'.join(parts))
PY
}
# The JDK's own version, from its release file.
jdk_version() { sed -n 's/^IMPLEMENTOR_VERSION="Temurin-\(.*\)"$/\1/p' "$1/release" 2>/dev/null; }

# Everything --check and --install hold a tree to: the manifest equals the
# pins, every package and the cmdline-tools at their pinned revision, the
# JDK at its version. Returns the number of problems in PROBLEMS.
verify_tree() {
    local root="$1" spec path want dir got
    if [[ ! -r "$root/$MANIFEST_NAME" ]]; then
        bad "$root has no $MANIFEST_NAME — not installed by this script"
    elif ! diff -q <(pins_canonical) "$root/$MANIFEST_NAME" >/dev/null; then
        bad "$root/$MANIFEST_NAME differs from the pins: $(diff <(pins_canonical) "$root/$MANIFEST_NAME" | grep -E '^[<>]' | head -3 | tr '\n' ' ')"
    else
        ok "manifest matches the pins"
    fi
    while IFS= read -r spec; do
        path="${spec%@*}" want="${spec##*@}"
        dir="$root/${path//;//}"
        got="$(pkg_revision "$dir" || true)"
        if [[ "$got" == "$want" ]]; then ok "$path $got"
        else bad "$path: ${got:-not installed}, pinned $want"; fi
    done < <(pkgs)
    got="$(sed -n 's/^Pkg.Revision=//p' "$root/cmdline-tools/latest/source.properties" 2>/dev/null | tr -d '[:space:]')"
    if [[ "$got" == "${PIN[CMDLINE_TOOLS_REVISION]}" ]]; then ok "cmdline-tools $got"
    else bad "cmdline-tools: ${got:-not installed}, pinned ${PIN[CMDLINE_TOOLS_REVISION]}"; fi
    got="$(jdk_version "$root/jdk")"
    if [[ "$got" == "${PIN[JDK_VERSION]}" ]]; then ok "jdk Temurin-$got"
    else bad "jdk: ${got:-not installed}, pinned Temurin-${PIN[JDK_VERSION]}"; fi
}

# ── --check ──────────────────────────────────────────────────────────
if [[ "$MODE" == check ]]; then
    say "== $me --check: $SDK_DIR against $(basename "$PINS_FILE") =="
    [[ -d "$SDK_DIR" ]] || { bad "$SDK_DIR is not installed on this host"; say "== 1 problem =="; exit 1; }
    verify_tree "$SDK_DIR"
    # A symlink's own mode is always rwxrwxrwx and means nothing: not counted.
    writable="$(find "$SDK_DIR" ! -type l -perm /022 -print -quit 2>/dev/null)"
    special="$(find "$SDK_DIR" -perm /6000 -print -quit 2>/dev/null)"
    # Every entry, links included: an account owning any of them can change it.
    foreign="$(find "$SDK_DIR" ! -user "${ANDROID_TOOLCHAIN_OWNER:-root}" -print -quit 2>/dev/null)"
    if [[ -z "$foreign" && -z "$writable" && -z "$special" ]]; then ok "owned by root throughout, read-only to accounts, no setuid or setgid"
    else bad "$SDK_DIR must be root's throughout, writable by no group or other, with no setuid/setgid bit (first offender: ${foreign:-${writable:-$special}})"; fi
    if [[ "$(persistence)" == rw-only ]]; then
        for kept in "$SDK_DIR" "$PROFILE"; do
            if grep -qxF "binds+=( '$kept' )" "$BIND_CONF" 2>/dev/null && mountpoint -q "$kept"; then ok "$kept kept across reboots by bind-dirs ($BIND_CONF)"
            else bad "this AppVM does not keep $kept: no bind-dirs entry in $BIND_CONF, or it is not mounted — it goes at the next shutdown"; fi
        done
    fi
    if grep -qx "export ANDROID_HOME=$SDK_DIR" "$PROFILE" 2>/dev/null; then ok "$PROFILE sets ANDROID_HOME"
    else bad "$PROFILE does not export ANDROID_HOME=$SDK_DIR"; fi
    if [[ "$PROBLEMS" -eq 0 ]]; then say "== the install matches the pins =="; exit 0; fi
    say "== $PROBLEMS problem(s) =="; exit 1
fi

# ── --stage ──────────────────────────────────────────────────────────
fetch() {  # fetch <url> <dest> <sha1|sha256> <sum>
    local url="$1" dest="$2" algo="$3" want="$4" got
    if [[ ! -s "$dest" ]]; then
        curl -fL --retry 3 --silent --show-error -o "$dest.part" "$url" || die "download failed: $url"
        mv "$dest.part" "$dest"
    fi
    got="$("${algo}sum" "$dest" | cut -d' ' -f1)"
    [[ "$got" == "$want" ]] || { rm -f "$dest"; die "$(basename "$dest"): $algo $got, pinned $want — removed; re-run to download again"; }
    ok "$(basename "$dest") $algo verified"
}
if [[ "$MODE" == stage ]]; then
    for tool in curl unzip tar sha1sum sha256sum python3 gzip; do
        command -v "$tool" >/dev/null || die "$tool is required to stage"
    done
    say "== $me --stage into $STAGE_DIR =="
    mkdir -p "$STAGE_DIR/downloads" || die "cannot create $STAGE_DIR"
    root="$STAGE_DIR/root"
    rm -rf "$root"; mkdir -p "$root" || die "cannot create $root"

    fetch "${PIN[JDK_URL]}" "$STAGE_DIR/downloads/jdk.tar.gz" sha256 "${PIN[JDK_SHA256]}"
    mkdir -p "$root/jdk" && tar -xzf "$STAGE_DIR/downloads/jdk.tar.gz" -C "$root/jdk" --strip-components=1 \
        || die "cannot unpack the JDK"
    fetch "${PIN[CMDLINE_TOOLS_URL]}" "$STAGE_DIR/downloads/cmdline-tools.zip" sha1 "${PIN[CMDLINE_TOOLS_SHA1]}"
    rm -rf "$STAGE_DIR/unzip" && mkdir -p "$STAGE_DIR/unzip" "$root/cmdline-tools" \
        && unzip -q "$STAGE_DIR/downloads/cmdline-tools.zip" -d "$STAGE_DIR/unzip" \
        && mv "$STAGE_DIR/unzip/cmdline-tools" "$root/cmdline-tools/latest" \
        || die "cannot unpack the command-line tools"

    # sdkmanager verifies each archive's sha1 against Google's repository.
    # The licences are accepted here, by the person staging, once per SDK.
    sdkm=( env JAVA_HOME="$root/jdk" "$root/cmdline-tools/latest/bin/sdkmanager" --sdk_root="$root" )
    yes | "${sdkm[@]}" --licenses >/dev/null 2>&1 || true
    mapfile -t paths < <(pkgs | sed 's/@[^@]*$//')
    "${sdkm[@]}" --install "${paths[@]}" > "$STAGE_DIR/sdkmanager.log" 2>&1 \
        || { tail -5 "$STAGE_DIR/sdkmanager.log" >&2; die "sdkmanager failed (log: $STAGE_DIR/sdkmanager.log)"; }

    pins_canonical > "$root/$MANIFEST_NAME"
    verify_tree "$root"
    [[ "$PROBLEMS" -eq 0 ]] || die "the staged tree does not match the pins ($PROBLEMS problem(s)): a package moved upstream — update the pins, then stage again"

    archive="$STAGE_DIR/android-toolchain-$(sha256sum "$root/$MANIFEST_NAME" | cut -c1-12).tar.gz"
    tar -C "$root" -czf "$archive" . || die "cannot write $archive"
    ( cd "$STAGE_DIR" && sha256sum "$(basename "$archive")" > "$(basename "$archive").sha256" )
    say "== staged: $archive ($(du -h "$archive" | cut -f1)) =="
    say "Next, as root on the host that gets it (here, or a copy of the archive and its .sha256):"
    say "  sudo bash tools/host/android/android-toolchain.sh --install $archive"
    exit 0
fi

# ── --install ────────────────────────────────────────────────────────
# Refusals first, every one before anything is written.
case "$(persistence)" in
    ""|full) persist=direct ;;
    rw-only) persist=bind ;;
    dispvm)  die "this is a Qubes DispVM: nothing it installs outlives it" ;;
    "?")     die "this is Qubes, but /qubes-vm-persistence cannot be read; refusing rather than guessing where a write persists" ;;
    *)       die "this Qubes VM's persistence is '$(persistence)': nothing it installs would survive" ;;
esac
[[ -r "$ARCHIVE" && -r "$ARCHIVE.sha256" ]] || die "need $ARCHIVE and $ARCHIVE.sha256 side by side"
# The sidecar's hash and name are both held to THIS archive: `sha256sum
# --check` would verify whatever file the sidecar names.
read -r want_sum want_name < "$ARCHIVE.sha256"
[[ "${want_name#\*}" == "$(basename "$ARCHIVE")" ]] || die "$ARCHIVE.sha256 names ${want_name:-nothing}, not $(basename "$ARCHIVE")"
[[ "$(sha256sum "$ARCHIVE" | cut -d' ' -f1)" == "$want_sum" ]] || die "$ARCHIVE does not match its .sha256 — copy it again"
manifest="$(tar -xzOf "$ARCHIVE" "./$MANIFEST_NAME" 2>/dev/null)" || die "$ARCHIVE holds no $MANIFEST_NAME"
[[ "$manifest" == "$(pins_canonical)" ]] \
    || die "$ARCHIVE was staged from other pins than this checkout's: stage again, or check out the commit it was staged from"
command -v python3 >/dev/null || die "python3 is required to install"
# THE ARCHIVE IS NOT TRUSTED: staged by an unprivileged account, unpacked
# here as root. Every member is judged before anything is written: only
# files, directories and symlinks (a hard link is refused even inside the
# tree; staged archives hold none); and each must pass tarfile's `data`
# filter — no absolute path, no `..` out of the tree, no link pointing
# outside it. The filter's known bypasses were fixed in 3.12.11 and
# 3.13.4, so an older Python is refused rather than trusted. The filter
# judges a path through the links already on disk, and here nothing is on
# disk yet, so two rules make the dry pass exact: no member name holds a
# `..` component, and no member is reached through an earlier symlink
# member (an SDK archive holds neither; links' TARGETS may use `..`).
python3 - "$ARCHIVE" <<'PY' || die "the archive is refused (above); nothing was written"
import os, sys, tarfile
# ANDROID_TOOLCHAIN_PYTHON_VERSION is a test hook, and it can only tighten:
# the version judged is the lower of it and the real one.
v = tuple(sys.version_info[:3])
if os.environ.get('ANDROID_TOOLCHAIN_PYTHON_VERSION'):
    v = min(v, (tuple(int(x) for x in os.environ['ANDROID_TOOLCHAIN_PYTHON_VERSION'].split('.')) + (0, 0))[:3])
if not hasattr(tarfile, 'data_filter') or v < (3, 12, 11) or (3, 13) <= v[:2] < (3, 14) and v < (3, 13, 4):
    sys.exit(f"python {v[0]}.{v[1]}.{v[2]} has no trustworthy tarfile data filter (needs 3.12.11+, 3.13.4+ or 3.14+)")
links = set()
with tarfile.open(sys.argv[1], 'r:gz') as t:
    for m in t.getmembers():
        if not (m.isfile() or m.isdir() or m.issym()):
            sys.exit(f"refused: {m.name} is a {'hard link' if m.islnk() else 'device or special file'}")
        parts = [p for p in m.name.split('/') if p not in ('', '.')]
        if '..' in parts:
            sys.exit(f"refused: {m.name} has a '..' component")
        through = next(('/'.join(parts[:i]) for i in range(1, len(parts)) if '/'.join(parts[:i]) in links), None)
        if through:
            sys.exit(f"refused: {m.name} is reached through the symlink {through}")
        if m.issym():
            links.add('/'.join(parts))
        try:
            tarfile.data_filter(m, '/nonexistent-android-sdk-dest')
        except tarfile.FilterError as e:
            sys.exit(f"refused: {e}")
PY
[[ "$(id -u)" -eq 0 ]] || die "--install writes $SDK_DIR and $PROFILE: run it as root"
[[ "$persist" == direct ]] || command -v mountpoint >/dev/null || die "mountpoint is required on a Qubes AppVM"
command -v flock >/dev/null || die "flock (util-linux) is required to install"

# Where the tree is really written: SDK_DIR itself, or, on an AppVM, its
# bind-dirs store under /rw, which Qubes mounts onto SDK_DIR at boot.
if [[ "$persist" == bind ]]; then store="$BIND_ROOT$SDK_DIR" pstore="$BIND_ROOT$PROFILE"; else store="$SDK_DIR" pstore="$PROFILE"; fi
old="$store.old"
if [[ "$persist" == bind ]]; then say "== $me --install into $SDK_DIR (kept by bind-dirs in $store) =="
else say "== $me --install into $SDK_DIR =="; fi
mkdir -p "$(dirname "$store")" || die "cannot create $(dirname "$store")"
# ONE INSTALL AT A TIME: everything below treats .old, .dead, .new.* and
# the profile record (.old or .absent) as a killed run's leftovers, and a second run would take
# the first's live state for them. Held until this process exits; the
# file lives in /run/lock (a tmpfs, never beside the SDK).
lock="${ANDROID_TOOLCHAIN_LOCK:-/run/lock/android-toolchain.lock}"
exec 9>"$lock" || die "cannot open the install lock $lock"
flock -n 9 || die "another --install is running on this host (it holds $lock)"
# On an AppVM the store is mounted on SDK_DIR: unmounted only for a swap
# or a rollback, and mounted again whatever happens, so a failure (or a
# killed run's next run) leaves a visible install.
mount_store() { [[ "$persist" == bind ]] || return 0; mkdir -p "$SDK_DIR" && { mountpoint -q "$SDK_DIR" || mount --bind "$store" "$SDK_DIR"; }; }
# What a run keeps of the profile before rewriting it: a copy at
# $pstore.old, or, where there was none, the marker $pstore.absent. Both
# are files, so a killed run's next run reads them as it would have. Kept
# only when there is a previous tree to roll back to: with none, a record
# would read at the next start as a killed rollback and take the profile
# from the tree the killed run left whole.
# The profile is restored IN PLACE: on an AppVM $PROFILE is bind-mounted
# from $pstore's inode, and a rename would leave the mount on the new text.
restore_profile() {
    if [[ -e "$pstore.old" ]]; then
        cat "$pstore.old" > "$pstore" && rm -f "$pstore.old"
    elif [[ -e "$pstore.absent" ]]; then
        { [[ "$persist" != bind ]] || ! mountpoint -q "$PROFILE" || umount "$PROFILE"; } \
            && rm -f "$pstore" && rm -f "$pstore.absent"
    else
        return 0
    fi
}
# A run killed between moving the old tree aside and moving the new one in
# left the old one at .old and nothing in place: put it back first, so
# this run's failure cannot leave the host with no install at all.
if [[ -e "$old" && ! -e "$store" ]]; then
    mv "$old" "$store" || die "cannot put the previous install back from $old; it is still there"
    say "  restored the previous install from $old"
fi
# A killed run's record of the profile is made only after its swap. With
# its .old tree still beside the store, it got past the swap and the new
# tree stays, so the record is dropped; without one, it was killed rolling
# back (rollback moves the store aside to .dead whole, and the final
# cleanup drops the record before the .old tree), so the previous tree is
# in place and the record is its profile: restored, as rollback would have.
if [[ -e "$pstore.old" || -e "$pstore.absent" ]]; then
    if [[ -e "$old" ]]; then rm -f "$pstore.old" "$pstore.absent"
    else restore_profile && say "  restored the previous profile (a killed rollback)"; fi
fi
rm -rf "$old" "$store.dead"
# A killed swap or rollback left SDK_DIR unmounted on an AppVM.
[[ ! -e "$store" ]] || mount_store || die "cannot bind-mount $store onto $SDK_DIR"
# Staging trees a killed run left (random names, so nothing else finds
# them).
rm -rf "$(dirname "$store")"/.android-sdk.new.* 2>/dev/null
new="$(mktemp -d "$(dirname "$store")/.android-sdk.new.XXXXXX")" || die "cannot make a staging directory beside $store"
trap 'rm -rf "$new"' EXIT
# Unpacked with the same `data` filter the scan judged it by, which also
# drops setuid, setgid and sticky bits and takes no owner from the archive.
python3 - "$ARCHIVE" "$new" <<'PY' || die "cannot unpack the archive (above); nothing installed"
import sys, tarfile
with tarfile.open(sys.argv[1], 'r:gz') as t:
    t.extractall(sys.argv[2], filter='data')
PY
chown -R root:root "$new" \
    && chmod -R u+rwX,go+rX,go-w "$new" \
    && chmod 0755 "$new" \
    || die "cannot set the owner or modes on $new (nothing installed yet)"
PROBLEMS=0; verify_tree "$new" >&2
[[ "$PROBLEMS" -eq 0 ]] || die "the unpacked tree does not verify ($PROBLEMS problem(s), above); $SDK_DIR is untouched"
# Unmounted for the swap only; mount_store (above) puts it back.
if [[ "$persist" == bind ]] && mountpoint -q "$SDK_DIR"; then umount "$SDK_DIR" || die "cannot unmount the old $SDK_DIR to replace it"; fi
[[ -e "$store" ]] && { mv "$store" "$old" || { mount_store; die "cannot move the old $store aside"; }; }
mv "$new" "$store" || { [[ -e "$old" ]] && mv "$old" "$store"; mount_store; die "cannot move the new tree into place; the old install is restored"; }
trap - EXIT
# The previous tree (and profile) stay until every step below has
# succeeded; a failure puts them back.
rollback() {  # rollback <message>
    [[ -e "$old" ]] || die "$1"
    # The failed tree is moved aside whole, not deleted in place: a run
    # killed half-way through deleting it left a half tree as the store
    # beside the intact .old, which the next run took for "past the swap"
    # and deleted the previous tree. .dead goes last, and is swept at start.
    if { [[ "$persist" != bind ]] || ! mountpoint -q "$SDK_DIR" || umount "$SDK_DIR"; } \
        && rm -rf "$store.dead" && mv "$store" "$store.dead" && mv "$old" "$store" && mount_store \
        && restore_profile; then
        rm -rf "$store.dead"
        die "$1; the previous install is restored"
    fi
    die "$1; restoring the previous install FAILED as well: the previous tree is at ${old}, if not at ${store}$( [[ -e "$pstore.old" ]] && printf ' (and its profile at %s)' "$pstore.old" )$( [[ -e "$pstore.absent" ]] && printf ' (it had no profile: %s says so)' "$pstore.absent" )"
}
mount_store || rollback "the new install is in $store but cannot be bind-mounted onto $SDK_DIR"

ndk_path="$(pkgs | sed -n 's/^ndk;\([^@]*\)@.*/\1/p' | head -1)"
mkdir -p "$(dirname "$pstore")" || rollback "cannot create $(dirname "$pstore")"
if [[ ! -e "$old" ]]; then :   # a first install: nothing to roll back to (above)
elif [[ -e "$pstore" ]]; then cp -p "$pstore" "$pstore.old" || rollback "cannot keep a copy of $pstore"
else : > "$pstore.absent" || rollback "cannot record that $pstore was absent"; fi
{
    echo "# Written by InterWeave tools/host/android/android-toolchain.sh --install."
    printf 'export ANDROID_HOME=%q\n' "$SDK_DIR"
    printf 'export ANDROID_SDK_ROOT=%q\n' "$SDK_DIR"
    printf 'export ANDROID_NDK_HOME=%q\n' "$SDK_DIR/ndk/$ndk_path"
    printf 'export ANDROID_NDK_ROOT=%q\n' "$SDK_DIR/ndk/$ndk_path"
    printf 'export ANDROID_JDK_HOME=%q\n' "$SDK_DIR/jdk"
} > "$pstore" && chmod 0644 "$pstore" || rollback "cannot write $pstore"

if [[ "$persist" == bind ]]; then
    mkdir -p "$(dirname "$BIND_CONF")" \
        && printf "# Written by InterWeave tools/host/android/android-toolchain.sh --install.\nbinds+=( '%s' )\nbinds+=( '%s' )\n" \
            "$SDK_DIR" "$PROFILE" > "$BIND_CONF.new" \
        && mv "$BIND_CONF.new" "$BIND_CONF" \
        || rollback "cannot write $BIND_CONF"
    # Now, without a restart: what Qubes does at the next boot (the SDK is
    # mounted already, just after the swap).
    if ! mountpoint -q "$PROFILE"; then
        mkdir -p "$(dirname "$PROFILE")" && touch "$PROFILE" && mount --bind "$pstore" "$PROFILE" \
            || rollback "cannot bind-mount $pstore onto $PROFILE"
    fi
fi
rm -f "$pstore.old" "$pstore.absent"; rm -rf "$old"
say "== installed =="
say "Each Android account runs, once: bash tools/host/android/rust-android.sh"
say "Any account, any time:            bash tools/host/android/android-toolchain.sh --check"
exit 0
