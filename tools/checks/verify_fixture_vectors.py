#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/verify_fixture_vectors.py
#
# >>> help
# Recompute every frozen conformance vector under fixtures/ from its
# declared algorithm and compare it to the stored value.
#
#   tools/checks/verify_fixture_vectors.py
#   tools/checks/verify_fixture_vectors.py --root <dir>
#
# A frozen vector nobody recomputes is a number in a file. These vectors
# are what independent implementations agree on, so a silent drift — a
# domain prefix edited, a length field widened, a canonicalization
# "clarified" — is a protocol break that no test would otherwise catch.
# The check exists so that changing a vector requires deciding to change
# it (CLAUDE.md §9, ADR-0049).
#
# A fixture file declares `algorithm.id`; this script implements that id
# independently, from the specification rather than from the fixture. An
# unknown id is a FAILURE, not a skip: a vector file that nothing can
# verify is exactly the case this guards against.
#
# Vectors carrying `frozen_by` are goldens re-frozen by an ADR. Their
# hashes are additionally required to be distinct from every other vector
# in the file, because collisions between the edge cases are the bug the
# edge cases exist to catch.
#
# PROSE COPIES are checked too. A golden is quoted in documents as well as
# stored here — ADR-0047 lists the re-frozen values, ENDPOINTS.md shows
# one inline, testing.md cites it in a conformance list. Only the vector
# file is recomputed, so a legitimate re-freeze would leave those copies
# confidently wrong in the documents a reader trusts most. Any file
# quoting a 64-hex value within a few lines of a golden's own inputs must
# quote a value this fixture actually holds.
#
# Options:
#   --root <dir>   verify this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  every vector recomputes to its stored value
#   1  a vector drifted, or a file declares an algorithm this cannot verify
#   2  invocation problem, or fixtures/ is missing
# <<< help

import hashlib
import json
import pathlib
import re
import sys

HELP_RE = re.compile(r"^# >>> help$(.*?)^# <<< help$", re.M | re.S)

problems: list[str] = []


def report(msg: str) -> None:
    problems.append(msg)
    print(msg)


def show_help() -> None:
    src = pathlib.Path(__file__).read_text(encoding="utf-8")
    m = HELP_RE.search(src)
    for line in (m.group(1) if m else "").splitlines():
        print(line[2:] if line.startswith("# ") else line.lstrip("#"))


# --- algorithms -----------------------------------------------------------
# Implemented from architecture/contracts/ENDPOINTS.md, deliberately NOT
# from the fixture's own description: a verifier that reads its rule from
# the artifact it checks proves nothing.

DIRECT_CONTENT_FINGERPRINT_V1_DOMAIN = b"interweave/direct-content-fingerprint/v1\x00"


def direct_content_fingerprint_v1(vector: dict) -> str:
    media = vector.get("media_type")
    payload = bytes.fromhex(vector.get("payload_hex", ""))

    buf = DIRECT_CONTENT_FINGERPRINT_V1_DOMAIN
    if media is None:
        buf += b"\x00"
    else:
        m = media.encode("ascii")
        if not 1 <= len(m) <= 128:
            raise ValueError(
                f"media type is {len(m)} bytes; the contract allows 1..128, "
                "and empty is invalid rather than absent"
            )
        buf += b"\x01" + len(m).to_bytes(2, "big") + m
    buf += len(payload).to_bytes(4, "big") + payload
    return hashlib.sha256(buf).hexdigest()


ALGORITHMS = {
    "direct-content-fingerprint-v1": direct_content_fingerprint_v1,
}


# --- prose copies ---------------------------------------------------------
# A frozen value is quoted in prose as well as stored in a vector file:
# ADR-0047 lists the re-frozen goldens, ENDPOINTS.md shows one inline,
# testing.md cites it in a conformance list. Those copies are useful and
# should stay — but only the vector file is recomputed, so a legitimate
# re-freeze would leave the prose confidently wrong in exactly the
# documents a reader trusts most. ADR-0047 re-froze these once already.
#
# So: every input that produces a stored hash is recomputed above; here
# the stored hash is looked for in the tracked tree, and any file quoting
# a DIFFERENT 64-hex value on the same line as the vector's own inputs is
# reported. This deliberately does not try to parse prose — it asks the
# narrow question "does some file quote a stale hash for this vector",
# which is the drift that matters.

WINDOW = 4

HEX64_RE = re.compile(r"\b([0-9a-f]{64})\b")

SKIP_DIRS = {".git", "target", "node_modules", ".claude"}


def golden_marker(vector: dict) -> str | None:
    """The text a prose copy of this vector is recognised by.

    Derived from `payload_hex`, the actual hash input, rather than from
    `payload_utf8` — that field is a reader convenience and optional, so
    a check that depended on it would silently stop attributing anything
    the moment a fixture omitted it, and report success while scanning
    nothing.
    """
    raw = vector.get("payload_hex")
    if raw:
        try:
            decoded = bytes.fromhex(raw).decode("utf-8")
        except (ValueError, UnicodeDecodeError):
            decoded = ""
        if decoded.isprintable() and decoded.strip():
            return decoded
    return vector.get("payload_utf8") or None


def check_prose_copies(root: pathlib.Path, fixture_rel: pathlib.Path, doc: dict) -> int:
    """Report prose that quotes a stale hash for a vector. Returns files scanned."""
    known: set[str] = {v["sha256"] for v in doc.get("vectors", []) if v.get("sha256")}
    if not known:
        return 0

    # A vector is identified in prose by its distinctive inputs. Only the
    # goldens are quoted in prose, and only they carry `frozen_by`.
    goldens = [v for v in doc.get("vectors", []) if v.get("frozen_by")]
    if not goldens:
        return 0

    scanned = 0
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        if any(part in SKIP_DIRS for part in path.relative_to(root).parts):
            continue
        if path.suffix not in {".md", ".txt", ".json", ".yaml", ".yml"}:
            continue
        rel = path.relative_to(root)
        if rel == fixture_rel:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        if "SHA-256" not in text and "sha256" not in text:
            continue
        scanned += 1

        # PROXIMITY, not whole-file. ADR-0047 lists four different frozen
        # goldens in one document — the fingerprint, the topic key, the
        # GossipSub message ID, the Kademlia namespace — so "this file
        # mentions the inputs somewhere and also contains a hash" flags
        # every neighbour. A hash is attributed to a vector only when
        # that vector's own inputs appear in the few lines leading up to
        # it, which is how all three prose copies are actually written.
        lines = text.splitlines()
        for i, line in enumerate(lines):
            found = HEX64_RE.findall(line)
            if not found:
                continue
            window = "\n".join(lines[max(0, i - WINDOW):i + 1])
            for g in goldens:
                marker = golden_marker(g)
                media = g.get("media_type")
                if not marker or marker not in window:
                    continue
                if media and media not in window:
                    continue
                for h in found:
                    # Compared against THIS golden's hash, not merely
                    # membership in the file. Hashes are input-specific,
                    # so prose that quotes a neighbouring edge vector's
                    # value beside these inputs is wrong in the way that
                    # matters — and a membership test would pass it.
                    if h != g["sha256"]:
                        report(
                            f"{rel}:{i + 1}: quotes SHA-256 {h} for the "
                            f"{g['name']} inputs, which should be "
                            f"{g['sha256']} per {fixture_rel}"
                        )
    return scanned


def main(argv: list[str]) -> int:
    root = pathlib.Path(__file__).resolve().parent.parent.parent

    args = argv[1:]
    while args:
        a = args.pop(0)
        if a in ("-h", "--help"):
            show_help()
            return 0
        if a == "--root":
            if not args:
                print("--root needs a value", file=sys.stderr)
                return 2
            root = pathlib.Path(args.pop(0)).resolve()
        else:
            print(f"verify_fixture_vectors: unexpected argument: {a}", file=sys.stderr)
            return 2

    fixtures = root / "fixtures"
    if not fixtures.is_dir():
        print(f"verify_fixture_vectors: not a directory: {fixtures}", file=sys.stderr)
        return 2

    files = sorted(fixtures.rglob("*.json"))
    checked = 0
    prose_scanned = 0

    for path in files:
        rel = path.relative_to(root)
        try:
            doc = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as e:
            report(f"{rel}: invalid JSON — {e}")
            continue

        if not isinstance(doc, dict) or "vectors" not in doc:
            continue

        alg_id = doc.get("algorithm", {}).get("id")
        fn = ALGORITHMS.get(alg_id)
        if fn is None:
            report(
                f"{rel}: declares algorithm '{alg_id}', which this verifier cannot "
                "compute — an unverifiable vector file is the failure, not a skip"
            )
            continue

        seen: dict[str, str] = {}
        for v in doc.get("vectors", []):
            name = v.get("name", "(unnamed)")
            stored = v.get("sha256")
            try:
                computed = fn(v)
            except Exception as e:  # noqa: BLE001 — the message is the report
                report(f"{rel}[{name}]: cannot compute — {e}")
                continue
            checked += 1
            if computed != stored:
                report(
                    f"{rel}[{name}]: DRIFT\n"
                    f"      stored:   {stored}\n"
                    f"      computed: {computed}"
                )
            if computed in seen:
                report(
                    f"{rel}[{name}]: collides with '{seen[computed]}' — "
                    "distinct inputs must produce distinct fingerprints"
                )
            else:
                seen[computed] = name

        # ANCHORING. Every vector file must trace to a decision, but the
        # two ways that happens are different and both are legitimate:
        #
        #   per-vector `frozen_by` — this exact value was published by an
        #     ADR and re-frozen there, so the ADR is the authority for the
        #     number itself;
        #   file-level `adr`       — the ALGORITHM was decided by these
        #     ADRs and the vectors are derived from it, which is the
        #     normal case for a layout with no published golden.
        #
        # Requiring `frozen_by` alone would push a derived-vector file
        # into either inventing a golden or going unanchored, and the
        # second is what this check exists to prevent.
        anchors = [a for a in doc.get("adr", []) if a]
        if not anchors and not any(v.get("frozen_by") for v in doc.get("vectors", [])):
            report(
                f"{rel}: nothing anchors this file to a decision — give it a "
                "file-level `adr` list, or mark an ADR-published golden with `frozen_by`"
            )
        for a in anchors + [v["frozen_by"] for v in doc.get("vectors", []) if v.get("frozen_by")]:
            if not list((root / "architecture" / "adr").glob(f"{a}-*.md")):
                report(f"{rel}: cites ADR-{a}, which does not exist")

        prose_scanned += check_prose_copies(root, rel, doc)

    if problems:
        print(f"\nverify_fixture_vectors: {len(problems)} problem(s).", file=sys.stderr)
        return 1

    print(
        f"verify_fixture_vectors: OK — {checked} vectors recomputed and matched, "
        f"{prose_scanned} prose file(s) checked for stale copies."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
