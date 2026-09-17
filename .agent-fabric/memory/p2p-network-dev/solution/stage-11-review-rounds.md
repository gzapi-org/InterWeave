---
role: "p2p-network-dev"
class: solution
description: "Stage 11's per-PR record — what #80–#86 each landed, how many review rounds, and the defect each late round found (a knob in the schema is not a knob in the crate; a self-test baseline that asked only for a non-zero exit)"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - a32f5a96a6f3ca14
---

## Stage 11's per-PR record — what #80–#86 each landed, how many review rounds, and the defect each late round found (a knob in the schema is not a knob in the crate; a self-test baseline that asked only for a non-zero exit)

Stage 11's PRs, one line each, as of 2026-09-10 (the state and what is next: [[stage-11-progress]]):

- Steps 1 and 2 done (PRs #71, #74/#75, merged 2026-09-06). Features-on (#76) and `ClassGated<B>` merged 2026-09-07/08.
- **#80 MERGED 2026-09-09 10:23** — `profile-config` models `transport.connectivity`; first production `InfrastructureSet` constructor; constructor ships GATED OFF per owner ruling. Ten review rounds. Codex refused the final heads on usage limits; §9's subagent-only path was used and recorded on the PR.
- **#81 MERGED 2026-09-09 10:23** — phase-B filtering measurement (`filter.sh`), both RFC 4787 halves measured. Six rounds.
- **#82 MERGED 2026-09-09 10:28** — shellcheck CI guard. Eight rounds; CI red four times. The pinned shellcheck 0.11.0 tarball runs from the session scratchpad (`scratchpad/shellcheck/shellcheck`), which is how the last two rounds were verified before pushing. SC2015 exempts `A && B || C` when B is a TEST command (not when C exits); 158 SC2015 at info on this tree, clean at warning.
- **#83 MERGED 2026-09-09 10:45** — records the owner's 2026-09-09 ruling (containerised matrix satisfies the exit gate's NAT row; three deferrals: population claim, public VM, carrier CGNAT; settles one of phase B's six items). Docs only. Arm once reviewed.
- **Step 3 in progress as #84** — the POLICY half: `crates/transport/runtime/src/reachability.rs`, a pure `ReachabilityManager`, plus the route correction and the ADR-0035/AUTONAT.md §3 amendments. Ten review rounds; no production consumer yet, so it carries three `stage-11` entries in `domain_fn_exempt.txt`. The libp2p ADAPTER is the next PR.
- **Round 10 stripped the module's probe-planning half** (cfb8c25, 1205 lines out). Codex found `due_probes` planning probes the crate never issues and `expire_inflight` timing them out; the owner ruled to remove rather than patch a fifth time. The manager is now evidence + verdict + `dial_order`. Pacing is the crate's `with_probe_interval`/`with_max_candidates`; **backoff is the root dial gate's `retry_delay_ms`**, which is already AUTONAT.md §4's 30s/doubling/5min. 02d1ac8 removed `retry_interval`, `max_inflight_probes` and `timeout` from the schema, the example and `AutonatClientConfig` — the crate hard-codes 10 in flight and a 10 s timeout at `v2/client/handler/dial_request.rs:94` and exposes no setter.
- **A knob in the schema is not a knob in the crate.** Before adding config for a libp2p behaviour, check the pinned crate's public `Config` surface; four of seven AutoNAT client knobs had no mechanism.
- **#85 OPEN (2026-09-09)** — ADR-0051 + the vendored `libp2p-autonat` with `Behaviour::retest`. #84 cites ADR-0051 and MUST NOT be armed before #85 lands. See [[vendored-autonat-client]]. **Three review rounds by 2026-09-10**, each finding real defects in the previous round's fix: the advisory guard's first-party test was membership (cargo promotes path deps), then location (`exclude` escapes it), now BOTH; ADR-0051 asserted a config removal that had not happened. Codex refused every request on 2026-09-10 for usage limits, so the opus subagent carried every gate.
- Step 3 reaches routes **2 and 3**, not 1 and 3 — see [[autonat-client-crate-facts]].
- Steps 4–10 not started.
- Follow-ups recorded on PRs: `check_connectivity_config_keys.sh` drift guard (after #80); four sibling guards read `git ls-files` newline-unsafely (after #82). Address canonicalisation at `attempt_dial` is DONE in #86, so step 5 is no longer waiting on it.

The #85 and #86 rounds in detail: [[stage-11-pr85-pr86-rounds]].

*References: autonat-client-crate-facts, stage-11-pr85-pr86-rounds, stage-11-progress, vendored-autonat-client*
