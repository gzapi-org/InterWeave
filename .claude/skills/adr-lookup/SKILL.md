---
name: adr-lookup
description: Finding and reading the InterWeave ADRs that govern a piece of work — digest first, full ADR on demand. Use whenever you need to know what the architecture decides about a subsystem, whether a rule exists, or which ADR owns a concept, and before changing behaviour in an area you have not read. For WRITING or amending an ADR use adr-authoring instead. Rule of thumb: if you are about to grep the ADR directory for a keyword, read the digest's keyword table first.
---

# Reading the ADRs

The accepted decisions live in `architecture/adr/`, one file each. Filenames sort by decision date, not topic, so browsing them is the slow way to find anything.

## The procedure

1. **Open [`architecture/adr/ADR-DIGEST.md`](../../../architecture/adr/ADR-DIGEST.md) and use the keyword → ADR table.** It maps how people actually phrase the question ("what does AcceptedV2 prove", "can a bootstrap peer be trusted", "where does this file go") onto the ADRs that answer it, including the secondary ADRs worth reading alongside.
2. **Read the matching digest entries.** Each is the current state of that decision: the binding rules, compressed. For most tasks the digest plus the repository-root `CLAUDE.md` is enough to work correctly.
3. **Open the full ADR only when** your change touches that decision's substance, the entry flags nuance you need, or you are about to write something that contradicts it.
4. **Never take a normative constant from the digest.** Limits, wire formats, and hash vectors come from `architecture/contracts/`, `architecture/transport/`, and `fixtures/`. The digest is explicitly non-normative and is allowed to be stale; the ADR and the contracts are not.

## What the digest guarantees

Every ADR has an entry — `tools/checks/validate_adr_index.sh` fails otherwise — so "not in the digest" reliably means "no such decision", not "someone forgot". That is what makes a digest-first read safe rather than a gamble.

Entries are grouped into nine clusters (foundation, broadcast, directed messaging and endpoints, discovery and Kademlia, connection/trust/reachability, local clients and IPC, identity, human retention, limits and state). Scanning a cluster heading is the right move when you know the area but not the vocabulary.

## Reading an ADR body

Bodies read **current** (ADR-0048): every amendment is folded into the section it qualifies, so the file is the decision as it stands. You never reconstruct the present state from an original plus a stack of notes.

The eight sections are always the same. For "what am I bound by", read **Decision** and **Security implications** — most invariants that get lost in a partial reading live in one of those two. **Revisit conditions** tells you whether your situation is the one the decision anticipated.

The `**Status:**` line carries supersession in prose: `Superseded by ADR-0035`, `Accepted; supersedes ADR-0024`, `Superseded in part by ADR-0034; cache/mDNS/static provider roles remain accepted`. **Read it before trusting the body** — three ADRs (0008, 0016, 0024) are wholly or partly historical, and the digest's superseded-pointer table lists them.

## When history actually matters

`architecture/adr/history/NNNN-amendments.md` holds dated notes explaining *why* a decision changed. Open it for research — reconstructing intent, understanding a reversal, answering "was this considered" — never to find out what a decision currently says. The body already answers that.

## If the digest and an ADR disagree

The ADR wins, always. Fix the digest entry in the same change, and say so in your response — a drifted entry is a bug that will mislead the next reader, not a matter of interpretation.
