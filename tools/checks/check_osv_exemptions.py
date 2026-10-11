#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_osv_exemptions.py
#
# >>> help
# Does every OSV.dev finding the Gradle graph accepts follow the acceptance
# policy, and does none of them sit on a classpath that ships?
#
#   tools/checks/check_osv_exemptions.py
#   tools/checks/check_osv_exemptions.py --root <dir>
#   tools/checks/check_osv_exemptions.py --today YYYY-MM-DD
#
# Plan §20 runs osv-scanner over the Gradle lockfiles locally, never in
# CI, with one osv-scanner.toml beside the build. That file's header
# states the acceptance policy. This check is the half of it that needs
# no network, so it runs with the tree checks. It reads
# apps/human-android/android/osv-scanner.toml and fails when:
#
#   1. the file holds anything but [[IgnoredVulns]] entries -- a
#      [[PackageOverrides]] entry ignores every finding of a package, and
#      plan §20 refuses it;
#   2. an entry has a key other than id, ignoreUntil and reason, lacks
#      one of them, or repeats another entry's id;
#   3. an entry's reason lacks one of its labelled parts, each on a line
#      of its own: `artefact: <group>:<name>:<version>`, `configuration:
#      <name>[, <name>...]`, `severity: LOW|MODERATE|HIGH|CRITICAL`,
#      `why: <text>`, `changes: <text>`, `checked: <the newest stable
#      release of the pinned plugin line> YYYY-MM-DD` (when that line was
#      last read for a release that fixes it -- agreed with rust-ui-dev,
#      01a12883), `reviewed: YYYY-MM-DD`;
#   4. an entry's ignoreUntil is not after its review date, or is more
#      than 90 days after it -- 30 for a CRITICAL;
#   5. a named configuration is one whose classes reach the APK: any
#      *RuntimeClasspath or *CompileClasspath. Those findings are never
#      accepted;
#   6. the artefact, at that version, is on no committed *.lockfile
#      under the build with every named configuration -- an entry must
#      describe the graph as it is -- or the lockfiles put it on a
#      configuration from rule 5. So an entry cannot say "lint only"
#      about an artefact that also ships.
#
# With --today, an entry whose ignoreUntil has passed fails too. That is
# for a local run: in CI a date passing would turn an unrelated PR red,
# and osv-scanner itself stops honouring an expired entry, so the
# local scan already fails on it.
#
# NOT CHECKED: whether the OSV id names that artefact, or whether the
# finding exists at all -- that is osv-scanner's, and needs the network.
# Nor whether the `why` is true: a reviewer reads it.
#
# A tree without osv-scanner.toml passes, saying so: nothing is accepted.
#
# Options:
#   --root <dir>        check this repository instead of the one
#                       containing this script
#   --today YYYY-MM-DD  also fail an entry whose ignoreUntil has passed
#   -h, --help          this text
#
# Exit codes:
#   0  every entry follows the policy, or there is no file
#   1  an entry breaks it
#   2  the check could not run: an unreadable or non-TOML file, Python
#      older than 3.11
# <<< help

import datetime
import re
import sys
from pathlib import Path

try:
    import tomllib
except ImportError:
    tomllib = None

NAME = "check_osv_exemptions"
BUILD = Path("apps/human-android/android")
CONFIG = BUILD / "osv-scanner.toml"
PARTS = ("artefact", "configuration", "severity", "why", "changes", "checked", "reviewed")
SEVERITIES = {"LOW", "MODERATE", "HIGH", "CRITICAL"}
HORIZON_DAYS = {"CRITICAL": 30}
DEFAULT_HORIZON_DAYS = 90
# A configuration whose classes reach the APK (plan §20's policy, rule 1).
SHIPS = re.compile(r"(RuntimeClasspath|CompileClasspath)$")
ARTEFACT = re.compile(r"^[A-Za-z0-9_.-]+:[A-Za-z0-9_.-]+:[A-Za-z0-9_.+-]+$")


def usage() -> None:
    text = Path(__file__).read_text(encoding="utf-8")
    body = text.split("# >>> help\n", 1)[1].split("# <<< help", 1)[0]
    print("\n".join(line[2:] if line.startswith("# ") else line.lstrip("#") for line in body.splitlines()))


def lockfile_configurations(build: Path) -> dict[str, set[str]]:
    """Each `group:name:version` on any committed lockfile under the
    build, with the union of the configurations the lockfiles give it."""
    found: dict[str, set[str]] = {}
    for lock in sorted(build.rglob("*.lockfile")):
        for line in lock.read_text(encoding="utf-8").splitlines():
            if line.startswith("#") or "=" not in line or line.startswith("empty="):
                continue
            coordinate, configurations = line.split("=", 1)
            found.setdefault(coordinate.strip(), set()).update(
                c.strip() for c in configurations.split(",") if c.strip()
            )
    return found


def parts_of(reason: str) -> dict[str, str]:
    """The labelled parts of a reason, one `label: text` per line."""
    parts = {}
    for line in reason.splitlines():
        m = re.match(r"^\s*([a-z]+):\s*(.*\S)\s*$", line)
        if m and m.group(1) in PARTS and m.group(1) not in parts:
            parts[m.group(1)] = m.group(2)
    return parts


def as_date(value) -> datetime.date | None:
    if isinstance(value, datetime.datetime):
        return value.date()
    if isinstance(value, datetime.date):
        return value
    return None


def check(root: Path, today: datetime.date | None) -> list[str]:
    config = root / CONFIG
    if not config.is_file():
        print(f"{NAME}: OK -- no {CONFIG}: nothing is accepted")
        return []
    data = tomllib.loads(config.read_text(encoding="utf-8"))
    problems = []
    for key in data:
        if key != "IgnoredVulns":
            problems.append(f"{key}: only [[IgnoredVulns]] entries are allowed (plan §20)")
    entries = data.get("IgnoredVulns", [])
    if not isinstance(entries, list):
        return problems + ["IgnoredVulns: must be an array of tables"]
    locks = lockfile_configurations(root / BUILD)
    seen = set()
    for n, entry in enumerate(entries, 1):
        where = f"entry {n} ({entry.get('id', 'no id')})"
        extra = set(entry) - {"id", "ignoreUntil", "reason"}
        if extra:
            problems.append(f"{where}: keys other than id, ignoreUntil, reason: {sorted(extra)}")
        missing = [k for k in ("id", "ignoreUntil", "reason") if k not in entry]
        if missing:
            problems.append(f"{where}: missing {missing}")
            continue
        if entry["id"] in seen:
            problems.append(f"{where}: the id is already accepted by an earlier entry")
        seen.add(entry["id"])
        parts = parts_of(str(entry["reason"]))
        lacking = [p for p in PARTS if p not in parts]
        if lacking:
            problems.append(f"{where}: the reason lacks {', '.join(f'`{p}:`' for p in lacking)}")
            continue
        severity = parts["severity"].upper()
        if severity not in SEVERITIES:
            problems.append(f"{where}: severity {parts['severity']!r} is none of {sorted(SEVERITIES)}")
        until = as_date(entry["ignoreUntil"])
        try:
            reviewed = datetime.date.fromisoformat(parts["reviewed"])
        except ValueError:
            reviewed = None
        if until is None:
            problems.append(f"{where}: ignoreUntil is not a date")
        if reviewed is None:
            problems.append(f"{where}: `reviewed:` is not YYYY-MM-DD")
        checked = re.search(r"\b(\d{4}-\d{2}-\d{2})$", parts["checked"])
        if not checked or len(parts["checked"].split()) < 2:
            problems.append(f"{where}: `checked:` names no release and date ({parts['checked']!r})")
        if until and reviewed:
            horizon = HORIZON_DAYS.get(severity, DEFAULT_HORIZON_DAYS)
            if until <= reviewed:
                problems.append(f"{where}: ignoreUntil {until} is not after the review date {reviewed}")
            elif (until - reviewed).days > horizon:
                problems.append(
                    f"{where}: ignoreUntil {until} is {(until - reviewed).days} days after the review; "
                    f"a {severity} entry gets at most {horizon}"
                )
            if today and until < today:
                problems.append(f"{where}: ignoreUntil {until} has passed; review it again")
        artefact = parts["artefact"]
        named = [c.strip() for c in parts["configuration"].split(",") if c.strip()]
        for c in named:
            if SHIPS.search(c):
                problems.append(f"{where}: {c} reaches the APK; a finding there is never accepted")
        if not ARTEFACT.match(artefact):
            problems.append(f"{where}: `artefact:` {artefact!r} is not group:name:version")
            continue
        on = locks.get(artefact)
        if on is None:
            problems.append(f"{where}: {artefact} is on no committed lockfile under {BUILD}")
            continue
        absent = [c for c in named if c not in on]
        if absent:
            problems.append(f"{where}: the lockfiles do not put {artefact} on {absent} (they say {sorted(on)})")
        shipping = sorted(c for c in on if SHIPS.search(c))
        if shipping:
            problems.append(f"{where}: the lockfiles put {artefact} on {shipping}, which reach the APK")
    if not problems:
        print(f"{NAME}: OK -- {len(entries)} accepted finding(s), each dated, explained and build-only")
    return problems


def main(argv: list[str]) -> int:
    root = Path(__file__).resolve().parents[2]
    today = None
    args = list(argv)
    while args:
        a = args.pop(0)
        if a in ("-h", "--help"):
            usage()
            return 0
        if a == "--root" and args:
            root = Path(args.pop(0))
        elif a == "--today" and args:
            try:
                today = datetime.date.fromisoformat(args.pop(0))
            except ValueError:
                print(f"{NAME}: --today takes YYYY-MM-DD", file=sys.stderr)
                return 2
        else:
            print(f"{NAME}: unknown argument {a!r} (see --help)", file=sys.stderr)
            return 2
    if tomllib is None:
        print(f"{NAME}: needs Python 3.11 or later (tomllib)", file=sys.stderr)
        return 2
    try:
        problems = check(root, today)
    except (OSError, tomllib.TOMLDecodeError, UnicodeDecodeError) as e:
        print(f"{NAME}: could not run: {e}", file=sys.stderr)
        return 2
    for p in problems:
        print(f"{NAME}: {p}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
