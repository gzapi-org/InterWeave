#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_docs_integrity.py
#
# >>> help
# Prove the documentation tree still holds together: every relative
# Markdown link resolves, and every YAML document parses.
#
#   tools/checks/check_docs_integrity.py
#   tools/checks/check_docs_integrity.py --root <dir>
#
# This repository IS its documentation — the architecture under
# architecture/ is the normative source, and a link that stopped
# resolving is a reader sent to nothing. CLAUDE.md §9 has required both
# of these for repository-wide changes since before there was any code;
# they were run as an ad-hoc snippet, by hand, which is the same
# silently-unwired state check_guards_are_wired.sh exists to prevent for
# guards. A check nobody is obliged to run is a check that stops running.
#
# Three questions, all of them about drift a rename produces:
#
#   TARGET  — a relative link whose file or directory is not there. Moves
#             and renames are how this happens, and the moved document is
#             never the one that reports it.
#   ANCHOR  — a `#fragment` naming no heading in the target document.
#             Renaming a section leaves every deep link pointing at the
#             top of the page, which looks like a working link. A
#             repeated heading is disambiguated the way GitHub does it,
#             `exit-gate`, `exit-gate-1`, `exit-gate-2`, so a link to a
#             later occurrence resolves.
#   YAML    — a tracked .yaml/.yml file, or a ```yaml block inside
#             Markdown, that no longer parses. The configuration examples
#             under architecture/config/examples/ are read as
#             specification by anyone writing a profile, and CI workflow
#             files that do not parse simply stop running.
#
# NOT checked: external http(s) links. Reachability of someone else's
# server is not a property of this tree, and a network-dependent guard
# fails for reasons that have nothing to do with the commit under test.
#
# Anchor slugs follow the GitHub rule — lower-case, drop everything
# outside word characters/hyphen/space, spaces to hyphens — which is what
# the rendered documents actually link against.
#
# Options:
#   --root <dir>   check this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  every link resolves and every YAML document parses
#   1  a link is broken or a document does not parse
#   2  invocation problem, or PyYAML is missing
# <<< help

import pathlib
import re
import sys
import urllib.parse

HELP_RE = re.compile(r"^# >>> help$(.*?)^# <<< help$", re.M | re.S)

# Build output, vendored dependencies, agent worktrees. `.claude` holds
# committed configuration but also `.claude/worktrees/`, which is a second
# checkout of this same tree — scanning it would double every report.
SKIP_DIRS = {".git", "target", "node_modules", ".claude"}

FENCE_RE = re.compile(r"^```.*?^```", re.M | re.S)
YAML_FENCE_RE = re.compile(r"^```ya?ml[^\n]*\n(.*?)^```", re.M | re.S)
HEADING_RE = re.compile(r"^(#{1,6})\s+(.+?)\s*$", re.M)
INLINE_LINK_RE = re.compile(r"\[[^\]]*\]\(\s*([^)\s]+?)(?:\s+\"[^\"]*\")?\s*\)")
REF_DEF_RE = re.compile(r"^ {0,3}\[[^\]]+\]:\s*(\S+)", re.M)
# A scheme, a protocol-relative URL, or a bare fragment handled separately.
ABSOLUTE_RE = re.compile(r"^[a-z][a-z0-9+.-]*:", re.I)

problems: list[str] = []


def report(msg: str) -> None:
    problems.append(msg)
    print(msg)


def show_help() -> None:
    src = pathlib.Path(__file__).read_text(encoding="utf-8")
    m = HELP_RE.search(src)
    for line in (m.group(1) if m else "").splitlines():
        print(line[2:] if line.startswith("# ") else line.lstrip("#"))


def slug(heading: str) -> str:
    """The fragment GitHub generates for a heading.

    Lower-case, drop every character outside word/hyphen/space, spaces to
    hyphens. Backticks and punctuation disappear, which is why
    `## The `xtask` runner` is reachable as `#the-xtask-runner`.
    """
    text = heading.strip().lower()
    text = re.sub(r"[^\w\- ]", "", text)
    return text.replace(" ", "-")


def unfragment(fragment: str) -> str:
    """The anchor a link is actually asking the browser to jump to.

    Percent-decoded and otherwise LEFT ALONE. Running the fragment
    through slug() as well would compare two normalised strings and
    accept links that do not work: `#TITLE` against `# Title` normalises
    to `title` on both sides and passes, while the browser looks for an
    element with `id="TITLE"` and finds nothing. Only the heading gets
    slugged, because only the heading is what the renderer transforms.
    """
    return urllib.parse.unquote(fragment)


def walk(root: pathlib.Path, suffixes: set[str]) -> list[pathlib.Path]:
    out = []
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        if any(part in SKIP_DIRS for part in path.relative_to(root).parts):
            continue
        if path.suffix.lower() in suffixes:
            out.append(path)
    return out


def read(path: pathlib.Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8")
    except (UnicodeDecodeError, OSError) as e:
        report(f"{path}: cannot be read as UTF-8 — {e}")
        return None


def anchor_ids(text: str) -> set[str]:
    """Every anchor a document offers, in the ids the renderer emits.

    Fenced code is stripped first: a `# comment` inside a shell block is
    not a heading, and counting it would make a broken anchor resolve.

    REPEATED HEADINGS GET SUFFIXES. GitHub disambiguates a second
    `## Exit gate` as `exit-gate-1`, a third as `exit-gate-2`. Collapsing
    them to one id would report every link to a later occurrence as
    broken — and the documents most likely to repeat a heading are the
    stage-structured ones here, so a false positive would land on exactly
    the files this is meant to protect.

    Each suffix is chosen against ALL ids already emitted, not merely a
    per-base counter. A document mixing `# Foo`, `# Foo-1`, `# Foo`
    renders as `foo`, `foo-1`, `foo-2`: the third heading's counter says
    `foo-1`, but that id is already taken by the second heading, so the
    renderer keeps counting. A per-base counter would emit `foo-1` twice
    and reject the valid `#foo-2` link.
    """
    ids: set[str] = set()
    seen: dict[str, int] = {}
    for m in HEADING_RE.finditer(FENCE_RE.sub("", text)):
        base = slug(m.group(2))
        count = seen.get(base, 0)
        candidate = base if count == 0 else f"{base}-{count}"
        while candidate in ids:
            count += 1
            candidate = f"{base}-{count}"
        seen[base] = count + 1
        ids.add(candidate)
    return ids


def check_links(root: pathlib.Path, markdown: list[pathlib.Path]) -> int:
    anchors = {}
    for path in markdown:
        text = read(path)
        if text is not None:
            anchors[path.resolve()] = anchor_ids(text)

    checked = 0
    for path in markdown:
        text = read(path)
        if text is None:
            continue
        rel = path.relative_to(root)
        # Links inside fenced code are illustrations, not navigation.
        body = FENCE_RE.sub("", text)

        targets = [m.group(1) for m in INLINE_LINK_RE.finditer(body)]
        targets += [m.group(1) for m in REF_DEF_RE.finditer(body)]

        for target in targets:
            if ABSOLUTE_RE.match(target) or target.startswith("//"):
                continue
            checked += 1
            location, _, fragment = target.partition("#")

            if not location:
                # A same-document fragment.
                if fragment and unfragment(fragment) not in anchors.get(path.resolve(), set()):
                    report(f"{rel}: '#{fragment}' names no heading in this document")
                continue

            resolved = (path.parent / urllib.parse.unquote(location)).resolve()
            if not resolved.exists():
                report(f"{rel}: link target does not exist: {target}")
                continue

            if fragment and resolved.suffix.lower() == ".md":
                known = anchors.get(resolved)
                if known is None:
                    # Outside the scanned set (a skipped directory); the
                    # path resolved, and that is all this can honestly say.
                    continue
                if unfragment(fragment) not in known:
                    report(f"{rel}: '{target}' names no heading in {resolved.relative_to(root)}")
    return checked


def check_yaml(root: pathlib.Path, markdown: list[pathlib.Path]) -> int:
    try:
        import yaml
    except ImportError as e:
        print(
            f"check_docs_integrity: python3 PyYAML is required: {e}",
            file=sys.stderr,
        )
        raise SystemExit(2) from e

    checked = 0
    for path in walk(root, {".yaml", ".yml"}):
        text = read(path)
        if text is None:
            continue
        checked += 1
        try:
            list(yaml.safe_load_all(text))
        except yaml.YAMLError as e:
            report(f"{path.relative_to(root)}: does not parse as YAML — {e}")

    for path in markdown:
        text = read(path)
        if text is None:
            continue
        for i, m in enumerate(YAML_FENCE_RE.finditer(text), start=1):
            checked += 1
            try:
                list(yaml.safe_load_all(m.group(1)))
            except yaml.YAMLError as e:
                report(
                    f"{path.relative_to(root)}: yaml block {i} does not parse — {e}"
                )
    return checked


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
            print(f"check_docs_integrity: unexpected argument: {a}", file=sys.stderr)
            return 2

    if not root.is_dir():
        print(f"check_docs_integrity: not a directory: {root}", file=sys.stderr)
        return 2

    markdown = walk(root, {".md"})
    links = check_links(root, markdown)
    documents = check_yaml(root, markdown)

    if problems:
        print(f"\ncheck_docs_integrity: {len(problems)} problem(s).", file=sys.stderr)
        return 1

    print(
        f"check_docs_integrity: OK — {links} relative link(s) across "
        f"{len(markdown)} document(s) resolve, {documents} YAML document(s) parse."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
