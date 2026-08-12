#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/validate_contracts.py
#
# >>> help
# Validate the machine-readable wire contracts under
# architecture/contracts/schemas/.
#
#   tools/checks/validate_contracts.py
#   tools/checks/validate_contracts.py --root <dir>
#
# Prose contracts cannot be validated, diffed for compatibility, or used
# to generate conformance vectors. These schemas can — but only while
# they stay consistent with the tree around them, which is what this
# checks (ADR-0049).
#
# Checks:
#   META       every schema validates against _meta/contract.meta.schema.json
#              and is itself a legal Draft 2020-12 schema.
#   IDENTITY   $id is urn:interweave:schemas:<family>:<concept>, and both
#              halves match the file's own path and name. x-contract.name
#              is the dotted form of the same pair.
#   MANIFEST   both directions: every schema file has a concept row in its
#              family manifest, every row names a file that exists, every
#              family directory has a root-manifest row, and every row
#              names a directory that exists.
#   STATUS     the manifest row's status equals the schema's own
#              x-contract.status. Two places to read a lifecycle is one
#              too many; they must agree.
#   PROVENANCE every referenced ADR number exists in architecture/adr/,
#              every referenced specification and fixture file exists.
#   REFS       every $ref to a urn:interweave: target resolves to a schema
#              that exists in the corpus.
#
# Options:
#   --root <dir>   validate this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  clean
#   1  one or more problems found
#   2  invocation problem, or a required path/dependency is missing
# <<< help

import json
import pathlib
import re
import sys

HELP_RE = re.compile(r"^# >>> help$(.*?)^# <<< help$", re.M | re.S)
ID_RE = re.compile(r"^urn:interweave:schemas:([a-z][a-z0-9-]*):([a-z][a-z0-9-]*)$")
REF_RE = re.compile(r'"\$ref"\s*:\s*"(urn:interweave:[^"]+)"')

problems: list[str] = []


def report(msg: str) -> None:
    problems.append(msg)
    print(msg)


def show_help() -> None:
    src = pathlib.Path(__file__).read_text(encoding="utf-8")
    m = HELP_RE.search(src)
    body = m.group(1) if m else ""
    for line in body.splitlines():
        print(line[2:] if line.startswith("# ") else line.lstrip("#"))


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
            print(f"validate_contracts: unexpected argument: {a}", file=sys.stderr)
            return 2

    try:
        import jsonschema
    except ImportError:
        print("validate_contracts: python3 jsonschema is required", file=sys.stderr)
        return 2

    schemas_dir = root / "architecture" / "contracts" / "schemas"
    meta_path = schemas_dir / "_meta" / "contract.meta.schema.json"
    root_manifest_path = schemas_dir / "manifest.json"
    for p in (schemas_dir, meta_path, root_manifest_path):
        if not p.exists():
            print(f"validate_contracts: expected path not found: {p}", file=sys.stderr)
            return 2

    def load(p: pathlib.Path):
        try:
            return json.loads(p.read_text(encoding="utf-8"))
        except json.JSONDecodeError as e:
            report(f"{p.relative_to(root)}: invalid JSON — {e}")
            return None

    meta = load(meta_path)
    if meta is None:
        return 1
    validator = jsonschema.Draft202012Validator(meta)

    adr_dir = root / "architecture" / "adr"
    known_adrs = {p.name[:4] for p in adr_dir.glob("[0-9][0-9][0-9][0-9]-*.md")}

    root_manifest = load(root_manifest_path)
    if root_manifest is None:
        return 1

    declared_families = {f["name"]: f for f in root_manifest.get("families", [])}
    actual_families = {
        d.name for d in schemas_dir.iterdir() if d.is_dir() and d.name != "_meta"
    }

    for name in sorted(actual_families - declared_families.keys()):
        report(f"schemas/{name}/: family directory has no row in the root manifest")
    for name in sorted(declared_families.keys() - actual_families):
        report(f"root manifest declares family '{name}', which has no directory")

    all_ids: set[str] = set()
    all_refs: list[tuple[str, str]] = []

    for family in sorted(actual_families & declared_families.keys()):
        fam_dir = schemas_dir / family
        fam_manifest_path = fam_dir / "manifest.json"
        if not fam_manifest_path.exists():
            report(f"schemas/{family}/: no manifest.json")
            continue
        fam_manifest = load(fam_manifest_path)
        if fam_manifest is None:
            continue
        if fam_manifest.get("family") != family:
            report(
                f"schemas/{family}/manifest.json: declares family "
                f"'{fam_manifest.get('family')}', but sits in '{family}/'"
            )

        rows = {c["file"]: c for c in fam_manifest.get("concepts", [])}
        files = {p.name for p in fam_dir.glob("*.schema.json")}

        for f in sorted(files - rows.keys()):
            report(f"schemas/{family}/{f}: no concept row in the family manifest")
        for f in sorted(rows.keys() - files):
            report(f"schemas/{family}/manifest.json: row names '{f}', which does not exist")

        for fname in sorted(files & rows.keys()):
            rel = f"schemas/{family}/{fname}"
            schema = load(fam_dir / fname)
            if schema is None:
                continue

            for err in validator.iter_errors(schema):
                where = "/".join(str(x) for x in err.absolute_path) or "(root)"
                report(f"{rel}: fails the meta-schema at {where} — {err.message}")

            try:
                jsonschema.Draft202012Validator.check_schema(schema)
            except jsonschema.exceptions.SchemaError as e:
                report(f"{rel}: not a legal Draft 2020-12 schema — {e.message}")

            sid = schema.get("$id", "")
            all_ids.add(sid)
            m = ID_RE.match(sid)
            if not m:
                report(f"{rel}: $id '{sid}' is not urn:interweave:schemas:<family>:<concept>")
            else:
                id_family, id_concept = m.groups()
                if id_family != family:
                    report(f"{rel}: $id family '{id_family}' does not match directory '{family}'")
                expected_file = f"{id_concept}.schema.json"
                if expected_file != fname:
                    report(f"{rel}: $id concept '{id_concept}' implies {expected_file}")
                xc = schema.get("x-contract", {})
                dotted = f"{id_family}.{id_concept}"
                if xc.get("name") != dotted:
                    report(f"{rel}: x-contract.name '{xc.get('name')}' should be '{dotted}'")

            xc = schema.get("x-contract", {})
            row_status = rows[fname].get("status")
            if row_status != xc.get("status"):
                report(
                    f"{rel}: manifest says status '{row_status}', schema says "
                    f"'{xc.get('status')}' — a lifecycle read from two places must agree"
                )

            for adr in xc.get("adr", []):
                if adr not in known_adrs:
                    report(f"{rel}: cites ADR-{adr}, which does not exist")
            spec = xc.get("specification")
            if spec and not (root / spec).exists():
                report(f"{rel}: specification '{spec}' does not exist")
            for fx in xc.get("fixtures", []):
                if not (root / fx).exists():
                    report(f"{rel}: fixture '{fx}' does not exist")

            for ref in REF_RE.findall(json.dumps(schema)):
                all_refs.append((rel, ref))

    for rel, ref in all_refs:
        if ref not in all_ids:
            report(f"{rel}: $ref '{ref}' resolves to no schema in the corpus")

    if problems:
        print(
            f"\nvalidate_contracts: {len(problems)} problem(s).",
            file=sys.stderr,
        )
        return 1

    print(
        f"validate_contracts: OK — {len(all_ids)} contracts across "
        f"{len(actual_families)} families, all meta-valid, manifested, and resolvable."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
