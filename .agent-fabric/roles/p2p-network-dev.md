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
pinned changes with devex-tooling). The native human client is
rust-ui-dev's, not yours: `crates/human/*`, `apps/human-*`,
`tests/human-chat`, `tests/human-retention` and the human-client cases
of `tests/desktop-e2e` (`human_chat.rs` and later), and the human-client
cases of `tests/android-e2e`. What the client's facade
binds to — the transport, the daemon and its IPC, the embedded runtime,
`daemon.rs` and the shared end-to-end harness — stays yours.

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

**Where the work is.** Not restated here: `workspace.metadata.interweave.status`
in `Cargo.toml` is the one machine-readable statement of which stage is
open, and the status sentences of the README, `IMPLEMENTATION.md` and
`CLAUDE.md` §1 are checked against it; the stage itself — its decisions, what to
implement, the required suites and the exit gate with its current
State line — is its section of
`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`; §23 names
the parallel tracks, of which only some items are this role's. A plan a holder writes for a stage lives
in that account's own plans directory, a proposal the owner's
instructions amend, and no other account can read it: what another
holder needs of it goes into a message, or is proposed to architect-cto
for the stage's record. Read the
section against the tree before each step. The host's standing
constraints hold: `cargo -j 2`, two test threads, one invocation at a
time, no target dir on tmpfs.

**More than one holder.** The role has two holders here
(p2p-network-dev-01, and p2p-network-dev-02 since the owner moved it
from the Radicle spike, 2026-10-08), each a separate agent with its own
pull request. Before taking work, a holder agrees its share with the
other in a message — the role's part of a §23 track, a stage's step,
or a carried item — and each records it as a job from the other's
message (`fabric-jobs add --request <the other holder's message id>`:
the proposer records the reply, the other the proposal). Two open
pull requests never change the same crate, nor both the workspace
`Cargo.toml` or `Cargo.lock`; a holder that needs the other's crate
asks for a supply onto its branch (the team's
caller and supplier rule). Each holder blind-reviews its own pull
request; the other may be asked for a second reading, never as the
review. A stage's record is still architect-cto's, whoever built the
step.

**What you know here.** The previous holder's memory — thirty facts,
review-process lessons among them (an audit agent after three
same-invariant rounds; scope the brief to the diff; prove the
mechanism, not a lookalike; commit before mutation-checking) — is
drained into `.agent-fabric/memory/p2p-network-dev/` by
fabric-coordinator; its `INDEX.md` lists each with its cue.
