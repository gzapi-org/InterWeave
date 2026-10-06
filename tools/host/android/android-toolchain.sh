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
#   --install ARCHIVE     in the TEMPLATE (or a StandaloneVM), as root:
#                         verify the archive and its manifest against the
#                         pins, create the group, install to SDK_DIR with
#                         group-write and setgid, write /etc/profile.d
#   --check               any host, read-only: is SDK_DIR what the pins say
#
# WHY TWO STAGES. On a Qubes AppVM /opt is on the root volume, discarded at
# shutdown and re-derived from the template, so an install made there
# vanishes at the next reboot. The template persists but reaches only the
# updates proxy, so it cannot download. The AppVM downloads and verifies;
# the archive travels with a sha256 sidecar (qvm-copy); the template
# installs it offline. (The same split as gzapp's Flutter SDK refresh.)
#
# WHAT EACH ACCOUNT STILL DOES. Join the group (the template's /etc/group,
# by the owner; then restart the AppVM), and run rust-android.sh once for
# rustup, the Android Rust target and cargo-ndk, which live in the home.
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
read_pins() {
    local line n=0
    [[ -r "$1" ]] || die "cannot read the pins at $1"
    while IFS= read -r line || [[ -n "$line" ]]; do
        n=$((n + 1))
        [[ -z "$line" || "$line" == \#* ]] && continue
        [[ "$line" =~ ^([A-Z][A-Z0-9_]*)=(.+)$ ]] || die "$1:$n is not KEY=VALUE: $line"
        PIN["${BASH_REMATCH[1]}"]="${BASH_REMATCH[2]}"
    done < "$1"
    local k
    for k in SDK_DIR SDK_GROUP CMDLINE_TOOLS_URL CMDLINE_TOOLS_SHA1 CMDLINE_TOOLS_REVISION JDK_VERSION JDK_URL JDK_SHA256 PKG_1; do
        [[ -n "${PIN[$k]:-}" ]] || die "$1 has no $k"
    done
}
# The PKG_<n> entries in order: "<sdkmanager path>@<revision>".
pkgs() { local k; for k in $(printf '%s\n' "${!PIN[@]}" | grep -E '^PKG_[0-9]+$' | LC_ALL=C sort -t_ -k2 -n); do printf '%s\n' "${PIN[$k]}"; done; }
# The pins as the manifest records them: every KEY=VALUE line, sorted
# bytewise. Never the locale's order: the AppVM that stages and the
# template that installs need not share a locale, and a different order
# would refuse a good archive as "staged from other pins".
pins_canonical() { local k; for k in "${!PIN[@]}"; do printf '%s=%s\n' "$k" "${PIN[$k]}"; done | LC_ALL=C sort; }

read_pins "$PINS_FILE"
SDK_DIR="${ANDROID_TOOLCHAIN_SDK_DIR:-${PIN[SDK_DIR]}}"
GROUP="${PIN[SDK_GROUP]}"
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
    if [[ "$(stat -c %G "$SDK_DIR" 2>/dev/null)" == "$GROUP" && -g "$SDK_DIR" ]]; then ok "group $GROUP, setgid"
    else bad "$SDK_DIR is not group $GROUP with setgid (it is $(stat -c '%G %A' "$SDK_DIR" 2>/dev/null))"; fi
    if grep -qx "export ANDROID_HOME=$SDK_DIR" "$PROFILE" 2>/dev/null; then ok "$PROFILE sets ANDROID_HOME"
    else bad "$PROFILE does not export ANDROID_HOME=$SDK_DIR"; fi
    if id -nG 2>/dev/null | tr ' ' '\n' | grep -qx "$GROUP"; then ok "$(id -un) is in $GROUP"
    else say "  NOTE  $(id -un) is not in $GROUP: it can read the SDK, not write it (the owner adds it in the template)"; fi
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
    say "Next, from this AppVM:"
    say "  qvm-copy-to-vm <template> $archive $archive.sha256"
    say "then in the template, as root, from a checkout of this repository:"
    say "  sudo bash tools/host/android/android-toolchain.sh --install ~/QubesIncoming/$(hostname -s 2>/dev/null || echo '<appvm>')/$(basename "$archive")"
    exit 0
fi

# ── --install ────────────────────────────────────────────────────────
# Refusals first, every one before anything is written.
vm_type="${ANDROID_TOOLCHAIN_VM_TYPE:-$(qubesdb-read /qubes-vm-type 2>/dev/null || true)}"
case "$vm_type" in
    TemplateVM|StandaloneVM) ;;
    AppVM|DispVM) die "this is a Qubes $vm_type: /opt is discarded at its next shutdown. Stage here, install in the template." ;;
    "") die "cannot tell whether this is a Qubes template (qubesdb-read /qubes-vm-type gave nothing); refusing rather than guessing" ;;
    *) die "unknown Qubes VM type '$vm_type'; refusing" ;;
esac
[[ -r "$ARCHIVE" && -r "$ARCHIVE.sha256" ]] || die "need $ARCHIVE and $ARCHIVE.sha256 side by side"
( cd "$(dirname "$ARCHIVE")" && sha256sum --check --quiet "$(basename "$ARCHIVE").sha256" ) >/dev/null 2>&1 \
    || die "$ARCHIVE does not match its .sha256 — copy it again"
manifest="$(tar -xzOf "$ARCHIVE" "./$MANIFEST_NAME" 2>/dev/null)" || die "$ARCHIVE holds no $MANIFEST_NAME"
[[ "$manifest" == "$(pins_canonical)" ]] \
    || die "$ARCHIVE was staged from other pins than this checkout's: stage again, or check out the commit it was staged from"
[[ "$(id -u)" -eq 0 ]] || die "--install writes $SDK_DIR, $PROFILE and /etc/group: run it as root"
for tool in setfacl groupadd tar; do command -v "$tool" >/dev/null || die "$tool is required to install"; done

say "== $me --install into $SDK_DIR =="
getent group "$GROUP" >/dev/null || groupadd --system "$GROUP" || die "cannot create group $GROUP"
new="$SDK_DIR.new" old="$SDK_DIR.old"
rm -rf "$new" "$old"
# --no-same-owner: as root, tar would otherwise give every file the
# staging account's uid from the archive; ownership is set below, once.
mkdir -p "$new" && tar --no-same-owner -xzf "$ARCHIVE" -C "$new" || die "cannot unpack into $new"
# Group-writable with setgid and a default ACL, as /opt/flutter is: setgid
# keeps the group on new files, the ACL keeps their group write. Without
# both, the first account to let Gradle add a package leaves files the next
# account cannot touch.
chown -R root:"$GROUP" "$new" \
    && chmod -R g+rwX "$new" \
    && find "$new" -type d -exec chmod g+s {} + \
    && setfacl -R -m g:"$GROUP":rwX "$new" \
    && find "$new" -type d -exec setfacl -d -m g:"$GROUP":rwX {} + \
    || die "cannot set the group, mode or ACL on $new (nothing installed yet)"
PROBLEMS=0; verify_tree "$new" >/dev/null
[[ "$PROBLEMS" -eq 0 ]] || die "the unpacked tree does not verify ($PROBLEMS problem(s)); $SDK_DIR is untouched"
[[ -e "$SDK_DIR" ]] && { mv "$SDK_DIR" "$old" || die "cannot move the old $SDK_DIR aside"; }
mv "$new" "$SDK_DIR" || { [[ -e "$old" ]] && mv "$old" "$SDK_DIR"; die "cannot move $new into place; the old install is restored"; }
rm -rf "$old"
ndk_path="$(pkgs | sed -n 's/^ndk;\([^@]*\)@.*/\1/p' | head -1)"
cat > "$PROFILE" <<EOF
# Written by InterWeave tools/host/android/android-toolchain.sh --install.
export ANDROID_HOME=$SDK_DIR
export ANDROID_SDK_ROOT=$SDK_DIR
export ANDROID_NDK_HOME=$SDK_DIR/ndk/$ndk_path
export ANDROID_NDK_ROOT=$SDK_DIR/ndk/$ndk_path
export ANDROID_JDK_HOME=$SDK_DIR/jdk
EOF
chmod 0644 "$PROFILE"
say "== installed =="
say "Next: add each Android account to $GROUP in this template"
say "  usermod -aG $GROUP <account>"
say "then shut the template down and restart the AppVM; each account then runs"
say "  bash tools/host/android/android-toolchain.sh --check"
exit 0
