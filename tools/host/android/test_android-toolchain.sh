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
# refusal before anything is written, the install itself (as uid 0 in a
# user namespace, into a sandbox), and --stage end to end, including a
# package that moved upstream.
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
SANDBOX="$(realpath -- "$(mktemp -d)")"; trap 'chmod -R u+w "$SANDBOX" 2>/dev/null; rm -rf "$SANDBOX"' EXIT
PINS="$SANDBOX/pins"

# Pins naming a fixture JDK and cmdline-tools archive, built below.
write_pins() {
    cat > "$PINS" <<EOF
# test pins
SDK_DIR=$SANDBOX/opt/android-sdk
SDK_GROUP=${1:-root}
CMDLINE_TOOLS_URL=https://example.invalid/cmdline-tools.zip
CMDLINE_TOOLS_SHA1=$(sha1sum "$SANDBOX/fx/cmdline-tools.zip" 2>/dev/null | cut -d' ' -f1)
CMDLINE_TOOLS_REVISION=23.0
PKG_1=platform-tools@37.0.1
PKG_2=platforms;android-30@3
PKG_10=ndk;28.2.13676358@28.2.13676358
JDK_VERSION=17.0.20.1+1
JDK_URL=https://example.invalid/jdk.tar.gz
JDK_SHA256=$(sha256sum "$SANDBOX/fx/jdk.tar.gz" 2>/dev/null | cut -d' ' -f1)
EOF
}
run() { out="$(ANDROID_TOOLCHAIN_PINS="$PINS" PATH="$SANDBOX/bin:$PATH" "$@" 2>&1)"; got=$?; }
# A tree as an install of the pins looks: what --check reads.
make_tree() {  # make_tree <root> [<override "path@rev">]
    local root="$1" spec path rev
    mkdir -p "$root/cmdline-tools/latest" "$root/jdk"
    printf 'Pkg.Revision=23.0\n' > "$root/cmdline-tools/latest/source.properties"
    printf 'IMPLEMENTOR_VERSION="Temurin-17.0.20.1+1"\n' > "$root/jdk/release"
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
shift_to=0; for a in "$@"; do
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
# curl: copies the fixture its URL names to -o.
cat > "$SANDBOX/bin/curl" <<EOF
#!/usr/bin/env bash
dest=""; url=""
while [[ \$# -gt 0 ]]; do case "\$1" in -o) dest="\$2"; shift 2 ;; -*) shift ;; *) url="\$1"; shift ;; esac; done
cp "$SANDBOX/fx/\$(basename "\$url")" "\$dest"
EOF
chmod +x "$SANDBOX/bin/curl"
write_pins

echo "android-toolchain: the pins are read, never sourced"
cp "$PINS" "$SANDBOX/pins.good"
printf 'PKG_3=$(touch %s/owned)\n' "$SANDBOX" >> "$PINS"; printf 'not a pin line\n' >> "$PINS"
run bash "$UNDER_TEST" --check
expect "a line that is not KEY=VALUE is refused" 2 "is not KEY=VALUE"
[[ ! -e "$SANDBOX/owned" ]] && pass "  and nothing in the file was executed" || fail "a pins line was executed"
grep -v '^JDK_SHA256=' "$SANDBOX/pins.good" > "$PINS"
run bash "$UNDER_TEST" --check
expect "a missing required pin is refused" 2 "has no JDK_SHA256"
cp "$SANDBOX/pins.good" "$PINS"

echo "android-toolchain --check"
run bash "$UNDER_TEST" --check
expect "nothing installed: exit 1, said" 1 "is not installed on this host"
make_tree "$SANDBOX/opt/android-sdk"; chmod g+s "$SANDBOX/opt/android-sdk"
mkdir -p "$SANDBOX/etc"; printf 'export ANDROID_HOME=%s\n' "$SANDBOX/opt/android-sdk" > "$SANDBOX/etc/profile"
export ANDROID_TOOLCHAIN_PROFILE="$SANDBOX/etc/profile"
write_pins "$(id -gn)"; cp "$PINS" "$SANDBOX/pins.mine"
make_tree "$SANDBOX/opt/android-sdk"
run bash "$UNDER_TEST" --check
expect "an install matching the pins passes" 0 "the install matches the pins"
[[ "$out" == *"OK    ndk;28.2.13676358 28.2.13676358"* && "$out" == *"jdk Temurin-17.0.20.1+1"* ]] && pass "  each package and the JDK named" || fail "the passing check did not name each part" "$out"
rm -rf "$SANDBOX/opt/android-sdk"; make_tree "$SANDBOX/opt/android-sdk" "platforms;android-30@2"; chmod g+s "$SANDBOX/opt/android-sdk"
run bash "$UNDER_TEST" --check
expect "a package at another revision is drift" 1 "platforms;android-30: 2, pinned 3"
make_tree "$SANDBOX/opt/android-sdk"; sed -i 's/^PKG_1=.*/PKG_1=platform-tools@37.0.2/' "$PINS"
run bash "$UNDER_TEST" --check
expect "pins raised after the install: the manifest differs" 1 "differs from the pins"
[[ "$out" == *"platform-tools: 37.0.1, pinned 37.0.2"* ]] && pass "  and the package is named" || fail "the raised package was not named" "$out"
cp "$SANDBOX/pins.mine" "$PINS"
rm -f "$SANDBOX/opt/android-sdk/jdk/release"
run bash "$UNDER_TEST" --check
expect "a missing JDK is drift" 1 "jdk: not installed"
make_tree "$SANDBOX/opt/android-sdk"; chmod g-s "$SANDBOX/opt/android-sdk"
run bash "$UNDER_TEST" --check
expect "an install without setgid is named" 1 "with setgid"
chmod g+s "$SANDBOX/opt/android-sdk"; : > "$ANDROID_TOOLCHAIN_PROFILE"
run bash "$UNDER_TEST" --check
expect "a missing profile entry is named" 1 "does not export ANDROID_HOME"
rm -rf "$SANDBOX/opt"

make_tree "$SANDBOX/opt/android-sdk"; chmod g+s "$SANDBOX/opt/android-sdk"; printf 'export ANDROID_HOME=%s\n' "$SANDBOX/opt/android-sdk" > "$ANDROID_TOOLCHAIN_PROFILE"
LC_ALL=en_US.UTF-8 run bash "$UNDER_TEST" --check
expect "the manifest compares the same under another locale (PKG_1 beside PKG_10)" 0 "the install matches the pins"
rm -rf "$SANDBOX/opt"

echo "android-toolchain --install: every refusal comes before anything is written"
cp "$SANDBOX/pins.good" "$PINS"
make_tree "$SANDBOX/staged"
tar -C "$SANDBOX/staged" -czf "$SANDBOX/a.tar.gz" . && ( cd "$SANDBOX" && sha256sum a.tar.gz > a.tar.gz.sha256 )
ANDROID_TOOLCHAIN_VM_TYPE=AppVM run bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
expect "an AppVM is refused" 2 "Stage here, install in the template"
run env -u ANDROID_TOOLCHAIN_VM_TYPE PATH="$SANDBOX/bin:/usr/bin:/bin" bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
[[ "$got" -eq 2 ]] && pass "an unreadable VM type is refused (rc=2)" || fail "an unreadable VM type was not refused" "$out"
printf 'x' >> "$SANDBOX/a.tar.gz"
ANDROID_TOOLCHAIN_VM_TYPE=TemplateVM run bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
expect "an archive not matching its .sha256 is refused" 2 "does not match its .sha256"
make_tree "$SANDBOX/staged2"; sed -i 's/^JDK_VERSION=.*/JDK_VERSION=17.0.19+1/' "$SANDBOX/staged2/.android-toolchain.manifest"
tar -C "$SANDBOX/staged2" -czf "$SANDBOX/a.tar.gz" . && ( cd "$SANDBOX" && sha256sum a.tar.gz > a.tar.gz.sha256 )
ANDROID_TOOLCHAIN_VM_TYPE=TemplateVM run bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
expect "an archive staged from other pins is refused" 2 "staged from other pins"
tar -C "$SANDBOX/staged" -czf "$SANDBOX/a.tar.gz" . && ( cd "$SANDBOX" && sha256sum a.tar.gz > a.tar.gz.sha256 )
ANDROID_TOOLCHAIN_VM_TYPE=TemplateVM run bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
if [[ "$(id -u)" -ne 0 ]]; then expect "not root: refused" 2 "run it as root"; fi
[[ ! -e "$SANDBOX/opt" ]] && pass "  and nothing was written by any refusal" || fail "a refusal wrote $SANDBOX/opt"

echo "android-toolchain --install, as uid 0 in a user namespace"
if unshare -r true 2>/dev/null && command -v setfacl >/dev/null; then
    mkdir -p "$SANDBOX/opt/android-sdk"; : > "$SANDBOX/opt/android-sdk/stale-old-file"
    ANDROID_TOOLCHAIN_VM_TYPE=TemplateVM run unshare -r env ANDROID_TOOLCHAIN_PINS="$PINS" \
        ANDROID_TOOLCHAIN_PROFILE="$SANDBOX/etc/profile.d" bash "$UNDER_TEST" --install "$SANDBOX/a.tar.gz"
    expect "installs" 0 "== installed =="
    [[ ! -e "$SANDBOX/opt/android-sdk/stale-old-file" && ! -e "$SANDBOX/opt/android-sdk.old" && ! -e "$SANDBOX/opt/android-sdk.new" ]] \
        && pass "  the old install is replaced whole, with no .old or .new left" || fail "the swap left something behind" "$(ls -a "$SANDBOX/opt")"
    [[ -g "$SANDBOX/opt/android-sdk/ndk" ]] && getfacl -p "$SANDBOX/opt/android-sdk/ndk" 2>/dev/null | grep -q '^default:group:' \
        && pass "  directories are setgid with a default group ACL" || fail "setgid or the default ACL is missing" "$(getfacl -p "$SANDBOX/opt/android-sdk/ndk" 2>&1)"
    grep -qx "export ANDROID_NDK_HOME=$SANDBOX/opt/android-sdk/ndk/28.2.13676358" "$SANDBOX/etc/profile.d" \
        && grep -qx "export ANDROID_JDK_HOME=$SANDBOX/opt/android-sdk/jdk" "$SANDBOX/etc/profile.d" \
        && ! grep -qE 'JAVA_HOME=|PATH=' "$SANDBOX/etc/profile.d" \
        && pass "  the profile names the SDK, NDK and JDK, and touches neither JAVA_HOME nor PATH" || fail "the profile is wrong" "$(cat "$SANDBOX/etc/profile.d")"
    ANDROID_TOOLCHAIN_PROFILE="$SANDBOX/etc/profile.d" run unshare -r env ANDROID_TOOLCHAIN_PINS="$PINS" ANDROID_TOOLCHAIN_PROFILE="$SANDBOX/etc/profile.d" bash "$UNDER_TEST" --check
    expect "  and --check passes on it" 0 "the install matches the pins"
    rm -rf "$SANDBOX/opt"
else
    echo "  - (skipped: no unprivileged user namespaces or no setfacl here)"
fi

echo "android-toolchain --stage, with curl and sdkmanager stubbed"
export REVS="$SANDBOX/revs"; grep -E '^PKG_' "$PINS" | sed 's/^PKG_[0-9]*=//' > "$REVS"
run bash "$UNDER_TEST" --stage --dest "$SANDBOX/st"
expect "stages a verified tree and an archive" 0 "== staged:"
arc="$(ls "$SANDBOX"/st/android-toolchain-*.tar.gz 2>/dev/null | head -1)"
[[ -n "$arc" ]] && ( cd "$SANDBOX/st" && sha256sum --check --quiet "$(basename "$arc").sha256" ) \
    && pass "  the archive and its .sha256 agree" || fail "no archive, or its .sha256 disagrees" "$(ls "$SANDBOX/st")"
[[ "$out" == *"qvm-copy-to-vm <template> $arc $arc.sha256"* ]] && pass "  and the next step is printed" || fail "the hand-off line is missing" "$out"
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
