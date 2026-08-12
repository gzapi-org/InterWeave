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

        if not any(v.get("frozen_by") for v in doc.get("vectors", [])):
            report(f"{rel}: no vector carries `frozen_by` — nothing anchors this file to an ADR")

    if problems:
        print(f"\nverify_fixture_vectors: {len(problems)} problem(s).", file=sys.stderr)
        return 1

    print(f"verify_fixture_vectors: OK — {checked} vectors recomputed and matched.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
