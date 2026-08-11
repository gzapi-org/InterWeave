# Required implementation spikes

Spikes are empirical evidence tasks, not production implementation.

## SPIKE-001 — Claude Channel packaging and MCP compatibility

**Objective:** prove the exact current Channel/package contract for the target Claude Code release.

**Experiment:** create the smallest throwaway Channel MCP server that declares `claude/channel`, emits one Channel notification, exposes one reply-like tool, and is packaged using the current documented `channels` manifest field and MCP declaration. Compare behavior to the current official Telegram plugin. Test clean stdio shutdown and the supported MCP SDK/spec revision.

**Expected evidence:** exact manifest fields; whether `channels` is required/optional for target packaging; SDK version; event meta constraints; process shutdown behavior; approved development-channel launch command.

**Decision unlocked:** implementation packaging and bridge SDK pin.

## SPIKE-002 — direct protocol codec and request-response semantics

**Objective:** validate rust-libp2p request-response behavior under timeout, disconnect, cancellation, and connection reuse.

**Experiment:** throwaway two-peer harness sends bounded 48 KiB requests, forces connection drops at several stages, measures substream/connection reuse, and exercises unsupported protocol versions.

**Expected evidence:** failure event mapping, default/required timeout controls, cancellation race, practical frame limits, whether a custom codec is sufficient without custom behaviour.

**Decision unlocked:** final direct codec API and error mapping without changing the selected higher-level direct-vs-GossipSub decision.

## SPIKE-003 — Kademlia value and poisoning/privacy profile

**Objective:** determine whether v1.x needs Kademlia at all and whether random-key `get_closest_peers` produces useful resilient candidate expansion.

**Experiment:** simulated/local multi-peer DHT with multiple bootstrap sets, churn, stale/poisoned routes, and Sybil-like candidate concentration. Measure convergence, unique candidate gain, query traffic, and bootstrap dependence. Do not use channel names/provider records.

**Expected evidence:** time-to-diverse-candidates, failure under malicious seeds, traffic/privacy footprint, operational complexity.

**Decision unlocked:** implement KademliaDiscovery, prefer another discovery provider, or continue to defer.

## SPIKE-004 — NAT and relay deployment matrix

**Objective:** determine the minimum Internet reachability mechanisms needed by target users.

**Experiment:** test direct public, home NAT, symmetric/CGNAT-like environments, configured Circuit Relay v2 paths, and relay outage. Compare without/with AutoNAT/DCUtR prototype support.

**Expected evidence:** success rates, setup requirements, dependency on relays, complexity/diagnostics burden.

**Decision unlocked:** whether relay client, AutoNAT, and DCUtR enter the supported deployment baseline.

## SPIKE-005 — IPC same-user hardening (conditional)

**Objective:** decide whether filesystem/pipe ownership is sufficient for intended deployments.

**Experiment:** assess threat environments where untrusted same-user processes exist; prototype OS peer-credential checks and optional short-lived local capability token.

**Expected evidence:** platforms covered, residual attack paths, UX/deployment cost.

**Decision unlocked:** keep ACL-only IPC or add application-level local authentication.
