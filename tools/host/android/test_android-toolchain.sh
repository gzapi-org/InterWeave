#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/host/android/test_android-toolchain.sh
#
# Self-test for android-toolchain.sh, on a synthetic toolchain: no network,
# no real SDK. A fake tree carries what the script reads (package.xml
# revisions, cmdline-tools source.properties, the JDK's release file, the
# manifest); curl and sdkmanager are stubs. Covers the strict pins reader,
# --check on a matching, drifted and absent install, every --install
# refusal before anything is written, a hostile archive (a device node, a
# hard link, a symlink out of the tree, a setuid file), a failed install
# leaving the previous one, the recovery of an interrupted swap, the
# install itself in place and through Qubes bind-dirs (as uid 0 in user
# and mount namespaces), and --stage end to end.
#
# Under CI the namespaces are required: a skip there would leave the
# install untested while the suite passed.
#
# Exit codes: 0 all assertions passed; 1 otherwise.

set -uo pipefail
HERE="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$HERE/android-toolchain.sh"
command -v python3 >/dev/null || { echo "test_android-toolchain: python3 is needed" >&2; exit 1; }

failures=0
pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2; failures=$((failures + 1)); }
expect() {  # expect <label> <want-rc> <substring>
    if [[ "$got" -eq "$2" && "$out" == *"$3"* ]]; then pass "$1"
    else fail "$1 — wanted exit $2 and '$3', got $got" "$out"; fi
}
# This host may be a Qubes AppVM; the cases that want a persistence say so.
export ANDROID_TOOLCHAIN_PERSISTENCE=""
SANDBOX="$(realpath -- "$(mktemp -d)")"; trap 'chmod -R u+w "$SANDBOX" 2>/dev/null; rm -rf "$SANDBOX"' EXIT
PINS="$SANDBOX/pins"

write_pins() {
    cat > "$PINS" <<EOF
# test pins
SDK_DIR=$SANDBOX/opt/android-sdk
CMDLINE_TOOLS_URL=https://example.invalid/cmdline-tools.zip
CMDLINE_TOOLS_SHA1=$(sha1sum "$SANDBOX/fx/cmdline-tools.zip" | cut -d' ' -f1)
CMDLINE_TOOLS_REVISION=23.0
PKG_1=platform-tools@37.0.1
PKG_2=platforms;android-30@3
PKG_10=ndk;28.2.13676358@28.2.13676358
JDK_VERSION=17.0.20.1+1
JDK_URL=https://example.invalid/jdk.tar.gz
JDK_SHA256=$(sha256sum "$SANDBOX/fx/jdk.tar.gz" | cut -d' ' -f1)
EOF
}
run() { out="$(ANDROID_TOOLCHAIN_PINS="$PINS" PATH="$SANDBOX/bin:$PATH" "$@" 2>&1)"; got=$?; }
# A tree as an install of the pins looks: what --check reads.
make_tree() {  # make_tree <root> [<override "path@rev">]
    local root="$1" spec path rev
    mkdir -p "$root/cmdline-tools/latest" "$root/jdk"
    printf 'Pkg.Revision=23.0\n' > "$root/cmdline-tools/latest/source.properties"
    printf 'IMPLEMENTOR_VERSION="Temurin-17.0.20.1+1"\n' > "$root/jdk/release"
    ln -s release "$root/jdk/release-link"   # a symlink's mode is 0777: --check must not count it
    while IFS= read -r spec; do
        path="${spec%@*}" rev="${spec##*@}"
        [[ -n "${2:-}" && "${2%@*}" == "$path" ]] && rev="${2##*@}"
        mkdir -p "$root/${path//;//}"
        python3 - "$root/${path//;//}/package.xml" "$rev" <<'PY'
import sys
parts = sys.argv[2].split('.')
tags = ''.join(f'<{t}>{v}</{t}>' for t, v in zip(('major', 'minor', 'micro'), parts))
open(sys.argv[1], 'w').write(f'<?xml version="1.0"?><ns2:repository xmlns:ns2="http://schemas.android.com/repository/android/common/02"><localPackage><revision>{tags}</revision></localPackage></ns2:repository>')
PY
    done < <(grep -E '^PKG_[0-9]+=' "$PINS" | sed 's/^PKG_[0-9]*=//')
    grep -E '^[A-Z][A-Z0-9_]*=' "$PINS" | LC_ALL=C sort > "$root/.android-toolchain.manifest"
}
pack() {  # pack <tree> -> $SANDBOX/a.tar.gz + .sha256
    tar -C "$1" -czf "$SANDBOX/a.tar.gz" . && ( cd "$SANDBOX" && sha256sum a.tar.gz > a.tar.gz.sha256 )
}
# A hostile archive: a good tree plus one member of the named kind.
pack_hostile() {  # pack_hostile <tree> device|hardlink|innerlink|escape|setuid|dotdot|chain
    python3 - "$1" "$2" "$SANDBOX/a.tar.gz" <<'PY'
import io, sys, tarfile
tree, kind, out = sys.argv[1:]
with tarfile.open(out, 'w:gz') as t:
    t.add(tree, arcname='.')
    if kind == 'chain':  # out of the tree only through a link the archive itself makes; no `..` in a name
        for name, link in (('a', None), ('a/up', '..'), ('a/up/esc', '..'), ('esc/pwned', None)):
            m = tarfile.TarInfo(name)
            if name == 'a': m.type = tarfile.DIRTYPE
            elif link: m.type, m.linkname = tarfile.SYMTYPE, link
            t.addfile(m)
        sys.exit(0)
    m = tarfile.TarInfo('./platform-tools/../evil' if kind == 'dotdot' else './platform-tools/evil')
    if kind == 'device':   m.type, m.devmajor, m.devminor, m.mode = tarfile.CHRTYPE, 1, 1, 0o666
    if kind == 'hardlink': m.type, m.linkname = tarfile.LNKTYPE, '/etc/shadow'
    if kind == 'innerlink': m.type, m.linkname = tarfile.LNKTYPE, './jdk/release'
    if kind == 'escape':   m.type, m.linkname = tarfile.SYMTYPE, '../../../../etc'
    if kind == 'setuid':
        data = b'#!/bin/sh\n'; m.mode, m.size = 0o4755, len(data); t.addfile(m, io.BytesIO(data))
    else:
        t.addfile(m)
PY
    ( cd "$SANDBOX" && sha256sum a.tar.gz > a.tar.gz.sha256 )
}

# Fixtures: a JDK tarball and a cmdline-tools zip whose sdkmanager is a stub
# writing package.xml for each path it is asked for (revisions from REVS).
mkdir -p "$SANDBOX/fx/jdk-17/bin" "$SANDBOX/bin"
printf 'IMPLEMENTOR_VERSION="Temurin-17.0.20.1+1"\n' > "$SANDBOX/fx/jdk-17/release"
tar -C "$SANDBOX/fx" -czf "$SANDBOX/fx/jdk.tar.gz" jdk-17
python3 - "$SANDBOX/fx/cmdline-tools.zip" <<'PY'
import sys, zipfile
sdkm = r'''#!/usr/bin/env bash
root=""; for a in "$@"; do case "$a" in --sdk_root=*) root="${a#--sdk_root=}" ;; esac; done
[[ " $* " == *" --licenses "* ]] && exit 0
for a in "$@"; do
  case "$a" in --sdk_root=*|--install) ;; *)
    rev="$(grep -F "$a@" "$REVS" | head -1 | sed 's/.*@//')"
    dir="$root/${a//;//}"; mkdir -p "$dir"
    python3 -c "import sys; p=sys.argv[2].split('.'); t=''.join(f'<{k}>{v}</{k}>' for k,v in zip(('major','minor','micro'),p)); open(sys.argv[1],'w').write('<r><localPackage><revision>'+t+'</revision></localPackage></r>')" "$dir/package.xml" "$rev" ;;
  esac
done
'''
with zipfile.ZipFile(sys.argv[1], 'w') as z:
    i = zipfile.ZipInfo('cmdline-tools/bin/sdkmanager'); i.external_attr = 0o755 << 16
    z.writestr(i, sdkm)
    z.writestr('cmdline-tools/source.properties', 'Pkg.Revision=23.0\n')
PY
cat > "$SANDBOX/bin/curl" <<EOF
#!/usr/bin/env bash
dest=""; url=""
while [[ \$# -gt 0 ]]; do case "\$1" in -o) dest="\$2"; shift 2 ;; -*) shift ;; *) url="\$1"; shift ;; esac; done
cp "$SANDBOX/fx/\$(basename "\$url")" "\$dest"
EOF
chmod +x "$SANDBOX/bin/curl"
write_pins; cp "$PINS" "$SANDBOX/pins.good"

echo "android-toolchain: the pins are read strictly, never sourced"
printf 'PKG_3=$(touch %s/owned)@1\n' "$SANDBOX" >> "$PINS"
run bash "$UNDER_TEST" --check
expect "a command substitution in a value is refused" 2 "the value of PKG_3 is not of its form"
[[ ! -e "$SANDBOX/owned" ]] && pass "  and nothing in the file was executed" || fail "a pins value was executed"
sed "s|^SDK_DIR=.*|SDK_DIR=$SANDBOX/opt/a;touch $SANDBOX/owned|" "$SANDBOX/pins.good" > "$PINS"
run bash "$UNDER_TEST" --check
expect "a ';' in SDK_DIR is refused" 2 "the value of SDK_DIR is not of its form"
cp "$SANDBOX/pins.good" "$PINS"; printf 'not a pin line\n' >> "$PINS"
run bash "$UNDER_TEST" --check
expect "a line that is not KEY=VALUE is refused" 2 "is not KEY=VALUE"
grep -v '^JDK_SHA256=' "$SANDBOX/pins.good" > "$PINS"
run bash "$UNDER_TEST" --check
expect "a missing required pin is refused" 2 "has no JDK_SHA256"
cp "$SANDBOX/pins.good" "$PINS"

echo "android-toolchain --check"
run bash "$UNDER_TEST" --check
expect "nothing installed: exit 1, said" 1 "is not installed on this host"
mkdir -p "$SANDBOX/etc"; printf 'export ANDROID_HOME=%s\n' "$SANDBOX/opt/android-sdk" > "$SANDBOX/etc/profile"
ANDROID_TOOLCHAIN_OWNER="$(id -un)"
export ANDROID_TOOLCHAIN_PROFILE="$SANDBOX/etc/profile" ANDROID_TOOLCHAIN_OWNER
make_tree "$SANDBOX/opt/android-sdk"
run bash "$UNDER_TEST" --check
expect "an install matching the pins passes" 0 "the install matches the pins"
[[ "$out" == *"OK    ndk;28.2.13676358 28.2.13676358"* && "$out" == *"jdk Temurin-17.0.20.1+1"* ]] && pass "  each package and the JDK named" || fail "the passing check did not name each part" "$out"
LC_ALL=en_US.UTF-8 run bash "$UNDER_TEST" --check
expect "the manifest compares the same under another locale (PKG_1 beside PKG_10)" 0 "the install matches the pins"
rm -rf "$SANDBOX/opt/android-sdk"; make_tree "$SANDBOX/opt/android-sdk" "platforms;android-30@2"
run bash "$UNDER_TEST" --check
expect "a package at another revision is drift" 1 "platforms;android-30: 2, pinned 3"
make_tree "$SANDBOX/opt/android-sdk"; sed -i 's/^PKG_1=.*/PKG_1=platform-tools@37.0.2/' "$PINS"
run bash "$UNDER_TEST" --check
expect "pins raised after the install: the manifest differs" 1 "differs from the pins"
[[ "$out" == *"platform-tools: 37.0.1, pinned 37.0.2"* ]] && pass "  and the package is named" || fail "the raised package was not named" "$out"
cp "$SANDBOX/pins.good" "$PINS"
rm -f "$SANDBOX/opt/android-sdk/jdk/release"
run bash "$UNDER_TEST" --check
expect "a missing JDK is drift" 1 "jdk: not installed"
make_tree "$SANDBOX/opt/android-sdk"; chmod g+w "$SANDBOX/opt/android-sdk/ndk"
run bash "$UNDER_TEST" --check
expect "a group-writable path in the SDK is named" 1 "writable by no group or other"
chmod g-w "$SANDBOX/opt/android-sdk/ndk"; chmod u+s "$SANDBOX/opt/android-sdk/jdk/release"
run bash "$UNDER_TEST" --check
expect "a setuid file in the SDK is named" 1 "no setuid/setgid bit"
chmod u-s "$SANDBOX/opt/android-sdk/jdk/release"
ANDROID_TOOLCHAIN_OWNER=nobody run bash "$UNDER_TEST" --check
expect "an SDK not owned by root is named" 1 "must be root's"
# qubesdb-read, stubbed: answers each key from $SANDBOX/qdb/<key>, or nothing.
mkdir -p "$SANDBOX/qdb"
printf '#!/bin/sh\ncat "%s/qdb/$(basename "$1")" 2>/dev/null\n' "$SANDBOX" > "$SANDBOX/bin/qubesdb-read"; chmod +x "$SANDBOX/bin/qubesdb-read"
qubes() { out="$(env -u ANDROID_TOOLCHAIN_PERSISTENCE ANDROID_TOOLCHAIN_PINS="$PINS" PATH="$SANDBOX/bin:$PATH" bash "$UNDER_TEST" "$@" 2>&1)"; got=$?; }
printf AppVM > "$SANDBOX/qdb/type"
printf full > "$SANDBOX/qdb/qubes-vm-persistence"
qubes --check; [[ "$got" -eq 0 && "$out" != *"bind-dirs"* ]] && pass "a fully persistent VM (a StandaloneVM) is checked as a direct install" || fail "full persistence was checked for bind-dirs" "$out"
printf rw-only > "$SANDBOX/qdb/qubes-vm-persistence"
qubes --check; expect "an rw-only VM (template-based AppVM) is checked for bind-dirs" 1 "bind-dirs"
ANDROID_TOOLCHAIN_PERSISTENCE=rw-only ANDROID_TOOLCHAIN_BIND_CONF="$SANDBOX/none.conf" run bash "$UNDER_TEST" --check
expect "on an AppVM, an SDK no bind-dirs entry keeps is named" 1 "goes at the next shutdown"
: > "$ANDROID_TOOLCHAIN_PROFILE"
run bash "$UNDER_TEST" --check
expect "a missing profile entry is named" 1 "does not export ANDROID_HOME"
rm -rf "$SANDBOX/opt"; unset ANDROID_TOOLCHAIN_OWNER

echo "android-toolchain --install: every refusal comes before anything is written"
make_tree "$SANDBOX/staged"; pack "$SANDBOX/staged"
printf DispVM > "$SANDBOX/qdb/type"; printf rw-only > "$SANDBOX/qdb/qubes-vm-persistence"
qubes --install "$SANDBOX/a.tar.gz"; expect "a DispVM (/type) is refused" 2 "nothing it installs outlives it"
printf AppVM > "$SANDBOX/qdb/type"; rm "$SANDBOX/qdb/qubes-vm-persistence"
qubes --install "$SANDBOX/a.tar.gz"; expect "on Qubes, an unreadable persistence is refused" 2 "/qubes-vm-persistence cannot be read"
printf none > "$SANDBOX/qdb/qubes-vm-persistence"
qubes --install "$SANDBOX/a.tar.gz"; expect "a VM that persists nothing is refused" 2 "persistence is 'none'"
rm -f "$SANDBOX/bin/qubesdb-read"
cp "$SANDBOX/a.tar.gz" "$SANDBOX/other.tar.gz"; printf 'x' >> "$SANDBOX/a.tar.gz"
ANDROID_TOOLCHAIN_PERSISTENCE=rw-only run bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
expect "an archive not matching its .sha256 is refused" 2 "does not match its .sha256"
( cd "$SANDBOX" && sha256sum other.tar.gz > a.tar.gz.sha256 )
ANDROID_TOOLCHAIN_PERSISTENCE=rw-only run bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
expect "a .sha256 naming another file is refused" 2 "names other.tar.gz, not a.tar.gz"
make_tree "$SANDBOX/staged2"; sed -i 's/^JDK_VERSION=.*/JDK_VERSION=17.0.19+1/' "$SANDBOX/staged2/.android-toolchain.manifest"
pack "$SANDBOX/staged2"
ANDROID_TOOLCHAIN_PERSISTENCE=rw-only run bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
expect "an archive staged from other pins is refused" 2 "staged from other pins"
for kind in escape dotdot chain; do
    pack_hostile "$SANDBOX/staged" "$kind"
    ANDROID_TOOLCHAIN_PERSISTENCE=rw-only run bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
    expect "a $kind member is refused by the dry pass, before the root check" 2 "nothing was written"
done
pack "$SANDBOX/staged"
for v in 3.12.10 3.13.3 3.11.13; do
    ANDROID_TOOLCHAIN_PYTHON_VERSION=$v ANDROID_TOOLCHAIN_PERSISTENCE=rw-only run bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
    expect "python $v (a data filter with known bypasses) is refused" 2 "no trustworthy tarfile data filter"
done
for v in 3.12.11 3.13.4 3.14.0; do
    ANDROID_TOOLCHAIN_PYTHON_VERSION=$v ANDROID_TOOLCHAIN_PERSISTENCE=rw-only run bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
    [[ "$out" != *"no trustworthy"* ]] && pass "python $v is accepted" || fail "python $v was refused" "$out"
done
pack "$SANDBOX/staged"
if [[ "$(id -u)" -ne 0 ]]; then
    ANDROID_TOOLCHAIN_PERSISTENCE=rw-only run bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
    expect "not root: refused" 2 "run it as root"
fi
[[ ! -e "$SANDBOX/opt" && ! -e "$SANDBOX/rw" ]] && pass "  and nothing was written by any refusal" || fail "a refusal wrote something" "$(ls "$SANDBOX")"

echo "android-toolchain --install, as uid 0 in user and mount namespaces"
if unshare -rm true 2>/dev/null; then
    inst() {  # inst <persistence>: --install (and, rw-only, --check inside the same mount namespace)
        local extra=""
        [[ "$1" == rw-only ]] && extra=" && bash '$UNDER_TEST' --check"
        run unshare -rm env ANDROID_TOOLCHAIN_PINS="$PINS" ANDROID_TOOLCHAIN_PERSISTENCE="$1" \
            ANDROID_TOOLCHAIN_PROFILE="$SANDBOX/etc/android-sdk.sh" ANDROID_TOOLCHAIN_BIND_ROOT="$SANDBOX/rw/bind-dirs" \
            ANDROID_TOOLCHAIN_BIND_CONF="$SANDBOX/rw/config/50_android-sdk.conf" ANDROID_TOOLCHAIN_OWNER=root \
            bash -c "bash '$UNDER_TEST' --install '$SANDBOX/a.tar.gz'$extra"
    }
    # In place (a template, a StandaloneVM, any other host): the previous
    # install is there, and must survive every failed attempt below.
    mkdir -p "$SANDBOX/opt/android-sdk"; : > "$SANDBOX/opt/android-sdk/previous-install"
    for kind in device hardlink innerlink escape; do
        pack_hostile "$SANDBOX/staged" "$kind"
        inst full
        if [[ "$got" -eq 2 && -e "$SANDBOX/opt/android-sdk/previous-install" && "$(ls -A "$SANDBOX/opt")" == android-sdk ]]; then
            pass "an archive with a $kind member is refused; the previous install stays, nothing beside it"
        else fail "a $kind member was not refused cleanly (exit $got)" "$out"$'\n'"$(ls -a "$SANDBOX/opt")"; fi
    done
    make_tree "$SANDBOX/bad" "platforms;android-30@9"; pack "$SANDBOX/bad"
    inst full
    [[ "$got" -eq 2 && "$out" == *"platforms;android-30: 9, pinned 3"* && -e "$SANDBOX/opt/android-sdk/previous-install" ]] \
        && pass "a tree that fails verification is refused, named, and the previous install stays" || fail "a failing tree was not refused cleanly" "$out"
    mv "$SANDBOX/opt/android-sdk" "$SANDBOX/opt/android-sdk.old"
    inst full
    [[ "$out" == *"restored the previous install"* && -e "$SANDBOX/opt/android-sdk/previous-install" ]] \
        && pass "an interrupted swap (.old, nothing in place) is restored before anything else" || fail "the interrupted swap was not restored" "$out"
    mkdir -p "$SANDBOX/opt/.android-sdk.new.killed/x"
    pack_hostile "$SANDBOX/staged" setuid
    inst full
    expect "a good archive installs in place" 0 "== installed =="
    [[ ! -e "$SANDBOX/opt/android-sdk/previous-install" && "$(ls -A "$SANDBOX/opt")" == android-sdk ]] \
        && pass "  the previous install is replaced whole, nothing left beside it" || fail "the swap left something" "$(ls -a "$SANDBOX/opt")"
    [[ -z "$(find "$SANDBOX/opt/android-sdk" ! -type l \( -perm /6000 -o -perm /022 \) | head -1)" && -e "$SANDBOX/opt/android-sdk/platform-tools/evil" ]] \
        && pass "  read-only to group and other, and the archive's setuid bit dropped" || fail "a writable or setuid path survived" "$(find "$SANDBOX/opt/android-sdk" ! -type l \( -perm /6000 -o -perm /022 \) | head -3)"
    grep -qx "export ANDROID_NDK_HOME=$SANDBOX/opt/android-sdk/ndk/28.2.13676358" "$SANDBOX/etc/android-sdk.sh" \
        && grep -qx "export ANDROID_JDK_HOME=$SANDBOX/opt/android-sdk/jdk" "$SANDBOX/etc/android-sdk.sh" \
        && ! grep -qE 'JAVA_HOME=|PATH=' "$SANDBOX/etc/android-sdk.sh" \
        && pass "  the profile names the SDK, NDK and JDK, and touches neither JAVA_HOME nor PATH" || fail "the profile is wrong" "$(cat "$SANDBOX/etc/android-sdk.sh")"
    rm -rf "$SANDBOX/opt" "$SANDBOX/etc/android-sdk.sh"
    # A Qubes AppVM: the tree goes to the bind-dirs store under /rw, an
    # entry keeps SDK_DIR and the profile, both are mounted now, and
    # --check passes while they are.
    pack "$SANDBOX/staged"; inst rw-only
    expect "on an AppVM: installs into the bind-dirs store, and --check passes while mounted" 0 "kept across reboots by bind-dirs"
    [[ -r "$SANDBOX/rw/bind-dirs$SANDBOX/opt/android-sdk/.android-toolchain.manifest" && -r "$SANDBOX/rw/bind-dirs$SANDBOX/etc/android-sdk.sh" ]] \
        && pass "  the tree and the profile live under /rw (the persistent volume)" || fail "the bind-dirs store is not where Qubes reads it" "$(find "$SANDBOX/rw" -maxdepth 8 | head)"
    grep -qx "binds+=( '$SANDBOX/opt/android-sdk' )" "$SANDBOX/rw/config/50_android-sdk.conf" \
        && grep -qx "binds+=( '$SANDBOX/etc/android-sdk.sh' )" "$SANDBOX/rw/config/50_android-sdk.conf" \
        && pass "  and the bind-dirs entry names the SDK and the profile" || fail "the bind-dirs entry is wrong" "$(cat "$SANDBOX/rw/config/50_android-sdk.conf" 2>&1)"
    rm -rf "$SANDBOX/rw" "$SANDBOX/opt"
elif [[ -n "${CI:-}" ]]; then
    fail "unprivileged user and mount namespaces are unavailable under CI: the install would go untested"
else
    echo "  - (skipped: no unprivileged user and mount namespaces here)"
fi

echo "android-toolchain --stage, with curl and sdkmanager stubbed"
export REVS="$SANDBOX/revs"; grep -E '^PKG_' "$PINS" | sed 's/^PKG_[0-9]*=//' > "$REVS"
run bash "$UNDER_TEST" --stage --dest "$SANDBOX/st"
expect "stages a verified tree and an archive" 0 "== staged:"
arc="$(ls "$SANDBOX"/st/android-toolchain-*.tar.gz 2>/dev/null | head -1)"
[[ -n "$arc" ]] && ( cd "$SANDBOX/st" && sha256sum --check --quiet "$(basename "$arc").sha256" ) \
    && pass "  the archive and its .sha256 agree" || fail "no archive, or its .sha256 disagrees" "$(ls "$SANDBOX/st")"
[[ "$out" == *"--install $arc"* ]] && pass "  and the install line is printed" || fail "the next step is missing" "$out"
sed -i 's/^platforms;android-30@3$/platforms;android-30@4/' "$REVS"
run bash "$UNDER_TEST" --stage --dest "$SANDBOX/st"
expect "a package that moved upstream fails the stage" 2 "a package moved upstream"
printf 'x' >> "$SANDBOX/fx/jdk.tar.gz"; rm -f "$SANDBOX/st/downloads/jdk.tar.gz"
run bash "$UNDER_TEST" --stage --dest "$SANDBOX/st"
expect "a download not matching its pin is refused, and removed" 2 "pinned"
[[ ! -e "$SANDBOX/st/downloads/jdk.tar.gz" ]] && pass "  the bad download is gone" || fail "the bad download was kept"

echo
if [[ $failures -eq 0 ]]; then echo "test_android-toolchain: OK — all assertions passed."; exit 0; fi
echo "test_android-toolchain: FAILED — $failures assertion(s)." >&2; exit 1
