#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_vendored_provenance.py
#
# >>> help
# Is every vendored tree under third_party/ its registry tarball plus its
# recorded patch, and nothing else?
#
#   tools/checks/check_vendored_provenance.py
#   tools/checks/check_vendored_provenance.py --root <dir>
#   tools/checks/check_vendored_provenance.py --crate-dir <dir>
#
# ADR-0051's route vendors a crate as the crates.io tarball minus its
# packaging files, plus the upstream licence when the tarball lacks one
# and this repository's `INTERWEAVE.patch`, and records the tarball's
# sha256 in tools/checks/license_exempt.txt. Until this check, nothing
# compared that checksum, and `git apply --check --reverse` on the patch
# proves only that its hunks are present: an unrecorded edit to another
# file, or outside a hunk's context, passed silently (third_party/
# README.md said so). For each tree this check:
#
#   1. reads the crate's name and version from the tree's Cargo.toml and
#      the sha256 from the tree's block in license_exempt.txt -- a tree
#      with no block, or a block with no 64-hex checksum, FAILS;
#   2. obtains `<name>-<version>.crate`: from --crate-dir when given,
#      else from cargo's registry cache, else from static.crates.io;
#   3. requires the tarball's sha256 to equal the recorded one;
#   4. reverse-applies INTERWEAVE.patch (when the tree has one) to a copy
#      of the tree, from a workspace-shaped root with -p0, as the
#      third_party README documents;
#   5. requires the result, less INTERWEAVE.patch and a LICENSE the
#      tarball does not carry, to be byte-identical to the tarball less
#      its packaging files (.cargo_vcs_info.json, Cargo.toml.orig,
#      Cargo.lock, .cargo-ok): the same file set, the same bytes.
#
# A patch that no longer reverse-applies is a failure too: the tree has
# moved under its record.
#
# NOT CHECKED: that the recorded checksum is the registry's. A tarball and
# a checksum replaced together pass; the lockfile's own checksum for the
# crate is gone once it is path-patched, so the record here is the only
# one left, and a reviewer of the commit that changes it is the check.
#
# Options:
#   --root <dir>        check this repository instead of the one
#                       containing this script
#   --crate-dir <dir>   take every tarball from <dir> and nothing else:
#                       no cache, no network (the self-test's fixtures)
#   -h, --help          this text
#
# Exit codes:
#   0  every vendored tree is its tarball plus its patch
#   1  a tree differs, a checksum differs or is missing, or a patch no
#      longer reverse-applies
#   2  the check could not run: no tarball obtainable, git missing, an
#      unreadable manifest
# <<< help

import hashlib
import io
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request
from pathlib import Path

PACKAGING = {".cargo_vcs_info.json", "Cargo.toml.orig", "Cargo.lock", ".cargo-ok"}
PATCH = "INTERWEAVE.patch"
NAME = "check_vendored_provenance"


class Unrunnable(Exception):
    """The check could not run: exit 2."""


def usage() -> None:
    text = Path(__file__).read_text(encoding="utf-8")
    body = text.split("# >>> help\n", 1)[1].split("# <<< help", 1)[0]
    print("\n".join(line[2:] if line.startswith("# ") else line.lstrip("#") for line in body.splitlines()))


def recorded_checksums(exempt: Path) -> dict[str, str]:
    """Each third_party tree's recorded sha256, from its comment block."""
    found: dict[str, str] = {}
    current = None
    block = ""
    for line in exempt.read_text(encoding="utf-8").splitlines():
        header = re.match(r"^# --- (third_party/[^ ]+) ", line)
        if header:
            if current:
                found[current] = block
            current, block = header.group(1), ""
        elif current and line.startswith("#"):
            block += line + "\n"
        elif current and not line.startswith("#"):
            found[current] = block
            current = None
    if current:
        found[current] = block
    sums = {}
    for tree, text in found.items():
        m = re.search(r"sha256\s*\n?#?\s*([0-9a-f]{64})", text)
        if m:
            sums[tree] = m.group(1)
    return sums | {tree: "" for tree in found if tree not in sums}


def tarball(name: str, version: str, crate_dir: Path | None) -> bytes:
    file = f"{name}-{version}.crate"
    if crate_dir is not None:
        path = crate_dir / file
        if not path.is_file():
            raise Unrunnable(f"{file} is not in {crate_dir}")
        return path.read_bytes()
    for cached in sorted(Path.home().glob(f".cargo/registry/cache/*/{file}")):
        return cached.read_bytes()
    url = f"https://static.crates.io/crates/{name}/{file}"
    try:
        with urllib.request.urlopen(url, timeout=60) as response:
            return response.read()
    except OSError as e:
        raise Unrunnable(f"{file}: not cached and not fetched from {url}: {e}") from e


def files_under(root: Path) -> dict[str, bytes]:
    return {
        str(p.relative_to(root)): p.read_bytes()
        for p in sorted(root.rglob("*"))
        if p.is_file()
    }


def check_tree(root: Path, tree: Path, recorded: str, crate_dir: Path | None) -> list[str]:
    """The tree's failures, empty when it is its tarball plus its patch."""
    rel = str(tree.relative_to(root))
    try:
        package = tomllib.loads((tree / "Cargo.toml").read_text(encoding="utf-8"))["package"]
        name, version = package["name"], package["version"]
    except (OSError, KeyError, tomllib.TOMLDecodeError) as e:
        raise Unrunnable(f"{rel}/Cargo.toml: no readable [package] name and version: {e}") from e
    if not recorded:
        return [f"{rel}: license_exempt.txt records no sha256 for it"]
    data = tarball(name, version, crate_dir)
    actual = hashlib.sha256(data).hexdigest()
    if actual != recorded:
        return [f"{rel}: {name}-{version}.crate is sha256 {actual}, the record says {recorded}"]

    with tempfile.TemporaryDirectory(prefix=f"{NAME}-") as scratch:
        scratch = Path(scratch)
        upstream = scratch / "upstream"
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            archive.extractall(upstream, filter="data")
        prefix = upstream / f"{name}-{version}"
        expected = {
            path: content
            for path, content in files_under(prefix).items()
            if Path(path).name not in PACKAGING or "/" in path
        }
        copy = scratch / "work" / rel
        shutil.copytree(tree, copy)
        if (copy / PATCH).is_file():
            applied = subprocess.run(
                ["git", "apply", "--reverse", "-p0", str(copy / PATCH)],
                cwd=scratch / "work",
                capture_output=True,
                text=True,
            )
            if applied.returncode != 0:
                return [f"{rel}: {PATCH} no longer reverse-applies: {applied.stderr.strip()}"]
        actual_files = files_under(copy)
        actual_files.pop(PATCH, None)
        if "LICENSE" not in expected:
            actual_files.pop("LICENSE", None)
        failures = []
        for path in sorted(set(expected) | set(actual_files)):
            if path not in actual_files:
                failures.append(f"{rel}: {path} is in the tarball and not in the tree")
            elif path not in expected:
                failures.append(f"{rel}: {path} is in the tree and not in the tarball or its patch")
            elif expected[path] != actual_files[path]:
                failures.append(f"{rel}: {path} differs from the tarball beyond {PATCH}")
        return failures


def main(argv: list[str]) -> int:
    root = Path(__file__).resolve().parents[2]
    crate_dir = None
    args = list(argv)
    while args:
        arg = args.pop(0)
        if arg in ("-h", "--help"):
            usage()
            return 0
        if arg == "--root" and args:
            root = Path(args.pop(0)).resolve()
        elif arg == "--crate-dir" and args:
            crate_dir = Path(args.pop(0)).resolve()
        else:
            print(f"{NAME}: unknown argument {arg!r}", file=sys.stderr)
            return 2
    if shutil.which("git") is None:
        print(f"{NAME}: git is required to reverse-apply a patch", file=sys.stderr)
        return 2
    third_party = root / "third_party"
    trees = sorted(p.parent for p in third_party.glob("*/Cargo.toml")) if third_party.is_dir() else []
    if not trees:
        print(f"{NAME}: OK — nothing is vendored.")
        return 0
    sums = recorded_checksums(root / "tools/checks/license_exempt.txt")
    failures = []
    checked = []
    try:
        for tree in trees:
            rel = str(tree.relative_to(root))
            tree_failures = check_tree(root, tree, sums.get(rel, ""), crate_dir)
            if tree_failures:
                failures += tree_failures
            else:
                checked.append(rel)
    except Unrunnable as e:
        print(f"{NAME}: cannot run: {e}", file=sys.stderr)
        return 2
    if failures:
        for failure in failures:
            print(f"{NAME}: {failure}", file=sys.stderr)
        print(
            f"\n{NAME}: {len(failures)} difference(s). A vendored tree is its registry "
            f"tarball plus {PATCH}; record any change in the patch (ADR-0051).",
            file=sys.stderr,
        )
        return 1
    print(f"{NAME}: OK — {len(checked)} vendored tree(s) are their tarball plus their patch:")
    for rel in checked:
        print(f"  {rel}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
