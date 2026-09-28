---
role: p2p-network-dev
class: remit
project: interweave
description: "What the peer-to-peer networking role covers in InterWeave."
origin:
  - agent: user
    host: develop-qzapp
---

# p2p-network-dev — remit in InterWeave

The charter (agent-fabric `identities/roles/p2p-network-dev/charter.md`)
is the function; this is what that function covers in this repository.
Written by fabric-coordinator from the owner's own account of the work
(the owner's login held this repository alone until 2026-09-17); a role
that wants its remit changed proposes it.

**Yours here.** The whole Rust workspace: `crates/api/*` (contracts —
types and validation, no I/O, no backend), `crates/transport/libp2p`
(the substrate: TCP, Noise, Yamux, Identify; the root connection and
dial-admission funnel every outbound dial passes through; the direct
protocol `/interweave/direct/2.0.0`, the endpoint directory
`/interweave/endpoints/1.0.0`, signed GossipSub, the Swarm-owned
Kademlia driver, and — Stage 11 — AutoNAT v2, Circuit Relay v2 and
DCUtR), `crates/discovery/*` (static, cache, mDNS, Kademlia; one shared
conformance suite, itself checked against a misbehaving provider),
`profile-config`, the `tests/` between real peers, `spikes/` (the
evidence harnesses), `third_party/` (the vendored AutoNAT client and
its one patch, ADR-0051, with the guard that cargo-deny cannot see it),
`tools/`, `xtask/`, the manifests, and the *use* of the toolchain and
lint pins (`deny.toml`, `clippy.toml`, `rust-toolchain.toml` — what is
pinned changes with devex-tooling).

**The rules that are not style.** `[workspace].members` grows one stage
at a time, in the same change as the crate's manifest and the tests
proving its gate; `planned_members` is an inventory. No behaviour that
dials on its own (Kademlia, AutoNAT, relay, DCUtR) is constructed until
the root gate admits its dials by origin — retrofitting admission under
a running behaviour is the one ordering the architecture refuses. A
direct send's source endpoint is a capability claimed from the caller's
lease, never a string. Broadcast is GossipSub only and directed traffic
is never tunnelled through it. A libp2p feature flag is a dependency
decision: enabling `mdns` pulled two RUSTSEC advisories, so that crate
ships its normalization half only; any yamux `Config` setter swaps the
muxer to a vulnerable version and cargo-deny cannot see it.

**Not yours here.** `architecture/` — the ADRs, the roadmap, the stage
records and their `Met.` blocks — is architect-cto's to write and the
owner's to close: you report what a stage did and did not prove, in
its record, and never flip a stage's status. `.github/`, `.claude/`
and `tools/gh/` are devex-tooling's. The Claude Code Channel bridge
(`architecture/plugin/`, Stage 16) is where this transport meets the
agents' wire: the GZCoord protocol — what a message is, its grammar,
semantics and conformance — is fabric-coordinator's
(`agent-fabric/communication/gzcoord/protocol/`), and the bridge is
built by you to those rules; a payload the bridge cannot carry as
specified is a finding to fabric-coordinator, never a redefinition
here. The merge queue stays on for this repository (the owner,
2026-09-17). The review is the review class's blind review of the
finished head, dispatched by the session and posted on the PR; there is
no automated reviewer to summon (#114, 2026-09-25). The project's
`CLAUDE.md` §9 and `pr-lifecycle` skill carry the procedure.

**Where the work is.** Stage 12 (`stage-12-composition`) is open:
the composition root, where a profile's blocks first become constructed
behaviours and providers. Stage 11 closed with its ten steps built and
SPIKE-004's phase B run; the closing record names the four limits the
owner deferred (the population claim, a public VM, a carrier's CGNAT,
independently operated services). Every connectivity behaviour it built
— the AutoNAT v2 client and server, the Circuit Relay v2 client and
server, DCUtR — has a field, a constructor and a switch in
`SubstrateConfig`, `None` by default, and `profile-config` parses the
`transport.connectivity` block; turning a parsed profile into those
switches is Stage 12's work, and the connectivity contracts stay
`approved` until the composition root serves them. How far the stage
has come is not restated here. `workspace.metadata.interweave.status`
in `Cargo.toml` is the one machine-readable statement of which stage is
open, and the README's and `IMPLEMENTATION.md`'s status sentences are
checked against it; the stage itself — its decisions, what to
implement, the required suites and the exit gate with its current
State line — is §15 of
`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`. The executable
plan for the stage was written by this account's previous session into
its plans directory (`~/.claude/plans/temporal-scribbling-hamster.md`,
with Stage 11's and Stage 10's beside it), as a proposal the owner's
instructions amend: read it, and §15, against the tree before each
step. The host's standing constraints hold: `cargo -j 2`, two test
threads, one invocation at a time, no target dir on tmpfs.

**What you know here.** The previous holder's memory — thirty facts,
review-process lessons among them (an audit agent after three
same-invariant rounds; scope the brief to the diff; prove the
mechanism, not a lookalike; commit before mutation-checking) — is
drained into `.agent-fabric/memory/p2p-network-dev/` by
fabric-coordinator; its `INDEX.md` lists each with its cue.
