# ADR digest — current-state summaries for fast navigation

**Status:** Informational (non-normative). This is the **cheapest correct entry point** into the ADR set for a contributor or an automated session: one compact, currently-true entry per ADR. It decides nothing — on any discrepancy the ADR wins, and the fix is to correct this digest.

## How to use this (read before opening full ADRs)

1. Look your topic up in the **keyword table** below, or scan the cluster headings.
2. Read the matching digest entries. For most tasks, those plus the repository-root `CLAUDE.md` are enough context to work correctly.
3. Open the **full ADR** only when your change touches its area's substance, or when the entry flags nuance you need.
4. Never take a normative constant from this file. Wire formats, limits, and hash vectors come from `architecture/contracts/`, `architecture/transport/`, and `fixtures/`.

Authority order is unchanged (`CLAUDE.md` §2): accepted ADRs → normative contracts and protocol specs → the bottom-up plan and test gates → explanatory documents → this digest and the index (navigation aids).

**Maintenance is binding.** Every new ADR gets an entry here in the same commit; every amendment updates its ADR's entry. `tools/checks/validate_adr_index.sh` fails when an ADR file has no digest entry, no index row, when either references an ADR that does not exist, when an ADR breaks the template, or when its amendment record is inconsistent.

---

## Keyword → ADR lookup

| You are working on… | Read |
|---|---|
| layering, what belongs in transport vs application, opaque payload, coordination semantics | **0001** (+ 0021, 0045 for where code lives) |
| Claude Code Channel, MCP bridge, stdio server, push notifications, tool surface | **0002** + **0023** |
| libp2p backend choice, TCP/Noise/Yamux/Identify wiring | **0003** (+ 0013 security, 0035 reachability wiring) |
| broadcast, GossipSub, topics, mesh message ID, duplicate key | **0004** + **0025** (+ **0029** validation mapping) |
| GossipSub validation result, Reject vs Ignore vs Accept, unauthorized publisher | **0029** (+ 0012 trust) |
| directed messaging, direct v2, request-response, AcceptedV2/RejectedV2, no_route | **0005** + **0030** (+ 0018 what acceptance means, 0019 dedup) |
| EndpointId, endpoint lease, default endpoint, source endpoint derivation, Model B | **0030** (+ 0017 lease handshake, 0026 endpoint limits, 0031 directory) |
| endpoint directory, advertised endpoints, remote endpoint discovery | **0031** (+ 0023 why it is not a Claude tool) |
| delivery guarantees, at-most-once, ordering, what AcceptedV2 proves | **0018** (+ 0020 no store, 0044 human retention) |
| duplicate suppression, dedup key, in-flight reservation, duplicate-ID conflict | **0019** (+ 0026 reservation bounds) |
| offline store, mailbox, buffering for an offline endpoint | **0020** (+ 0044 the one narrow application exception) |
| discovery provider interface, candidate events, provenance, expiry | **0006** + **0007** (+ 0022 registration, 0027 cache) |
| mDNS, static bootstrap, peer cache, provider roles | **0008** + **0010** + **0027** |
| Kademlia, DHT, peer routing, routing table admission, network namespace | **0009** (integration/security) + **0034** (default-on rollout) |
| bootstrap peer authority, "is a bootstrap peer trusted" | **0010** (no — reachability hint only) |
| ConnectionManager, dial admission, DialAdmissionGate, behaviour-originated dials, backoff | **0011** (+ 0036 infrastructure class) |
| address failure vs peer failure, poisoned address, address identity mismatch, quarantine | **0011** §Address-scoped failure |
| trust policy, allowlist, deny-by-default, revocation, UnauthorizedPeer | **0012** (+ 0029 broadcast mapping, 0036 the non-widening infrastructure set) |
| Noise, encryption in transit, PeerId authentication | **0013** (+ 0014 what is NOT end-to-end encrypted) |
| group/end-to-end encryption, payload confidentiality from forwarding peers | **0014** (deferred — bounded by the trusted overlay) |
| daemon vs embedded, process model, surviving Claude Code restarts | **0015** (+ 0041 the Android exception) |
| profile identity, one PeerId per profile, multiple local applications | **0016** (superseded for routing by **0030**) |
| local IPC, framing, 128 KiB body, hello handshake, keepalive, capabilities | **0017** (+ 0037 admin split) |
| admin socket, admin authority, client.kind, privilege separation | **0037** (+ 0017, 0032) |
| Claude tool surface, send/broadcast/reply/join/leave/identity/status | **0023** (+ 0002) |
| payload ceilings, queue bounds, rate limits, token buckets, resource exhaustion | **0026** (+ 0019 reservations, 0031 directory bounds) |
| peer cache, persisted reachability, protocol observations | **0027** (+ 0006, 0010) |
| config vs state vs cache, profile directories, what is persisted | **0028** (+ 0020, 0030 leases are runtime-only) |
| AutoNAT v2, Circuit Relay v2, DCUtR, NAT traversal, reachability state | **0035** (mandatory in standard v1; supersedes **0024**) |
| relay reservation targets, path selection, address advertisement | **0035** §Relay reservation policy / §Path selection |
| infrastructure peers, relay/probe authorization that is not data-plane trust | **0036** (+ 0012, 0011) |
| human client boundary, what the UI may own, contacts, display names | **0032** (+ 0039 stack, 0040 desktop, 0041 Android) |
| Slint, human client UI stack, Rust core, JNI shim scope | **0039** |
| desktop human client, daemon binding, endpoint lease, settings | **0040** (+ 0017, 0037) |
| Android runtime, foreground service, background reachability, LocalDataSession | **0041** (+ 0032, 0042) |
| Android Keystore, key wrapping, unlock policy, user-presence | **0042** (+ 0033 the portable seed it wraps) |
| identity recovery, 24-word mnemonic, BIP-39, expected_peer_id, verify drill | **0033** (+ 0038 at-rest encryption, 0043 multi-device) |
| encrypted key at rest, passphrase, SPIKE-007 | **0038** |
| multiple devices, same person on phone and desktop, cloning identity | **0043** (distinct PeerIds — not account sync) |
| human message retention, unread, keep-after-read, pending outbound, backup | **0044** (+ 0020, 0018) |
| human chat envelope, markdown messages, compression, brotli, decompressed ceiling, prompt injection, auto-fetch | **0050** (+ 0044 retention, 0032 boundary) |
| repository layout, where does this file go, apps vs crates vs tests | **0045** (+ 0021 dependency boundaries) |
| implementation order, what may be built next, stage gates, workspace members | **0046** (canonical order; phases are scope labels) |
| naming, wire namespace, `interweave` vs InterWeave, frozen hash vectors | **0047** |
| writing an ADR, amending one, the template, propagation, what this digest is for | **0048** |
| supersede vs amend, amendment history, why a rule changed | **0048** (+ `history/`) |
| wire schemas, JSON Schema, contract families, x-contract status, is this contract implemented | **0049** |
| frozen vectors, conformance fixtures, golden hashes, fixture drift | **0049** (+ 0047 the re-frozen values, 0019 the fingerprint's purpose) |

---

## Cluster 1 — Foundation and boundaries

### 0001 — Generic transport boundary (Accepted)
Four explicit layers: Claude Code, Channel MCP bridge, generic transport runtime, network backend.
- Rules: Claude-specific concepts stop at the bridge; libp2p-specific concepts stop at the backend; the generic transport carries opaque payloads plus transport metadata and defines **no** application coordination semantics.
- Keywords: layering, boundary, opaque payload, transport metadata, no coordination semantics

### 0002 — Reuse official Claude Channel and Telegram patterns (Accepted)
Adopt the current Claude Code Channel contract rather than inventing one.
- Rules: stdio MCP server, `claude/channel` capability, push `notifications/claude/channel`, ordinary tools for outbound actions, explicit Channel instructions, sender/trust gating **before** notification delivery; content/meta separation and terminal-only trust mutation; transport ownership moves into a daemon; no remote permission relay in v1.
- Keywords: claude channel, mcp, stdio, push notification, trust gating, permission relay

### 0003 — rust-libp2p as initial network backend (Accepted)
rust-libp2p is the first backend, behind the neutral transport contract.
- Rules: TCP, Noise, Yamux, GossipSub, request-response, Identify, pluggable discovery; the backend stays an adapter and never leaks upward.
- Keywords: rust-libp2p, backend adapter, tcp, yamux, identify

### 0015 — Separate profile-scoped transport daemon (Accepted)
Architecture B: the Claude bridge connects over local IPC to a separate Rust daemon.
- Rules: the daemon owns identity and network lifecycle and survives Claude Code restarts. Carries an Android amendment — ADR-0041 embeds the same runtime in a foreground service instead.
- Keywords: daemon, embedded, process model, lifecycle, architecture b

### 0021 — Rust workspace dependency boundaries (Accepted)
Neutral contracts stay separate from concrete runtime, libp2p, platform, and application crates.
- Rules: neutral transport/EndpointId, discovery, trust, local-client, IPC, and Kademlia-control contracts are their own crates; first-party human clients build from shared human-core/store/UI crates; IPC stays language-neutral. The repository currently holds landing zones and an **empty** virtual workspace — no production manifests or source.
- Keywords: workspace, crate boundaries, neutral api, landing zones, zero members

### 0045 — Implementation repository layout and test placement (Accepted)
Where every kind of file lives, and at which layer each test belongs.
- Rules: specifications under `architecture/`; `apps/` is composition roots only; `crates/` is reusable packages grouped by responsibility; neutral API crates carry no libp2p/UI/Android/SQLite/Claude/platform types; `tests/` is cross-crate/network/conformance/E2E; `tests/support` never becomes a production dependency; `fixtures/` is frozen and normative while `test-data/` is mutable; spikes map one-to-one to `roadmap/SPIKES.md` and never become production code by copying; a path becomes a real package only when its stage adds the manifest **and** the workspace member in the same change.
- Keywords: repository layout, apps, crates, tests, fixtures, test-data, spikes, xtask, workspace members

### 0046 — Bottom-up dependency-gated implementation order (Accepted)
The canonical construction order, which overrides the historical numbered phases.
- Rules: foundation/fixtures → neutral contracts/config → pure policies/state machines → persistence → minimal authenticated libp2p → **root connection/dial admission** → direct v2 → GossipSub → endpoint directory → non-Kademlia discovery → Kademlia → AutoNAT/Relay/DCUtR → runtime composition → daemon/IPC → human/Claude integrations → Android → security gate → packaging. Numbered phases remain scope/release labels, not a dependency order. A higher stage may not become functional until its lower gates pass. Spikes run just-in-time and their results become permanent tests.
- Keywords: implementation order, stage gates, bottom-up, phases are not order, just-in-time spikes

### 0047 — Adopt InterWeave as the project and wire namespace (Accepted)
Display name **InterWeave**; machine identifiers lowercase `interweave`.
- Rules: canonical protocol names `/interweave/direct/2.0.0`, `/interweave/endpoints/1.0.0`, `/interweave/kad/1.0.0/<network-hash>`; domain-separation prefixes and application/local identifiers likewise; genuine Claude integration names stay Claude-specific. Because the namespace participates in deterministic hashing, four goldens were **re-frozen** (direct content fingerprint, topic key, GossipSub message ID, Kademlia network namespace) — the vectors live in the ADR and `fixtures/`. The former pre-InterWeave identifiers are not compatibility aliases.
- Keywords: naming, wire namespace, interweave, re-frozen vectors, no compatibility alias

### 0048 — ADR authoring, amendment, and propagation (Accepted)
How this corpus is written, changed, and navigated.
- Rules: an ADR body reads **current** — amendments are folded into the section they qualify, never appended as end-matter prose; numbered decision items are permanent citable identifiers and a withdrawn one becomes a tombstone; a change of substance is a **superseding ADR**, not an edit (the test: would a reader who followed the old text now be wrong?). Every amendment is recorded three ways in one commit series — the in-place edit, a dated note in `history/NNNN-amendments.md` (`### Amendment YYYY-MM-DD — title`), and a row in the ADR's `## Amendments` end-matter table; the (date, title) pair is the identity, and `(ii)` suffixes exist only to break byte-identical headings. No changelog, no version counter. Propagation — README row, digest entry, dependent specs, fixtures only on intentional vector change — is part of the change, enforced by `tools/checks/validate_adr_index.sh`. The digest is the default entry point and is non-normative.
- Keywords: adr template, amendment, supersede vs amend, history file, propagation, digest is non-normative, no changelog

### 0049 — Machine-readable wire contracts with lifecycle status (Accepted)
Wire **shape** gets JSON Schema beside the prose; the prose stays normative for **behaviour**.
- Rules: schemas live under `architecture/contracts/schemas/<family>/<concept>.schema.json`, Draft 2020-12, one dialect; `$id` is `urn:interweave:schemas:<family>:<concept>` and both halves must match the file's own location; `x-contract.status` says what a contract is authoritative **about** — `active`/`deprecated` describe the current wire, **`approved` is an implementation target and never a claim that anything implements it**, `proposed` must not drive implementation (everything here is `approved` until an implementation exists); provenance (deciding ADRs + prose specification) is mandatory and existence-checked. Frozen vectors declare their algorithm and are **recomputed from the specification** by `verify_fixture_vectors.py` — an unknown algorithm is a failure, not a skip — and all vectors in a file must hash distinctly. `validate_contracts.py` checks meta-conformance, identity-against-location, manifests in both directions, status agreement, and `$ref` resolvability.
- Security: a schema is a shape check, **never** an authorization boundary — it cannot enforce that a narrowing policy is applied. It does mechanically bound the directory response (≤32 unique, TTL clamped) before a hostile reply reaches a cache or UI.
- Keywords: json schema, contract families, x-contract status, approved vs active, urn identity, frozen vectors, fixture drift, manifest

---

## Cluster 2 — Broadcast

### 0004 — GossipSub is the v1 broadcast primitive (Accepted)
Signed GossipSub with a frozen mesh-level message-ID function.
- Rules: logical channels map to domain-separated hashed topics; signed messages with strict cryptographic/protocol validation; explicit application validation reporting per ADR-0029; the mesh message ID is the full SHA-256 of a domain-separated tuple over the **signed source PeerId raw bytes + GossipSub wire sequence number** (`transport/libp2p/PUBSUB.md`); using the application-envelope message ID as the mesh duplicate key is **forbidden**; GossipSub never substitutes for directed delivery.
- Keywords: gossipsub, broadcast, signed messages, message id, mesh duplicate key, topic mapping

### 0025 — ASCII ChannelId with versioned hashed wire topic (Accepted)
- Rules: ChannelId is 1..128 ASCII bytes matching `[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}`, case-sensitive; the wire topic is SHA-256 over a domain/version prefix plus the raw ID.
- Keywords: channelid, topic hash, ascii, domain separation

### 0029 — Map GossipSub validation results separately from trust admission (Accepted)
The three-way mapping that keeps local authorization from looking like protocol invalidity.
- Rules: **Reject** = objective invalidity (bad signature/source association, malformed or version-invalid envelope, impossible length fields, invalid message-ID encoding) — not forwarded, scoring may penalise. **Ignore** = structurally and cryptographically valid but the authenticated original publisher is not locally authorized — not delivered, not forwarded, propagation peer not penalised. **Accept** = valid and locally authorized. Never `Accept` then locally drop for an unauthorized source; never `Reject` merely because allowlists differ between nodes.
- Keywords: reject, ignore, accept, validation mapping, unauthorized publisher, scoring

### 0014 — Defer application/group encryption above GossipSub (Accepted)
- Rules: v1 relies on a trust-gated data-plane overlay plus Noise-encrypted links; it explicitly does **not** promise secrecy from trusted forwarding peers. The payload stays opaque so a higher layer or future extension can encrypt it.
- Keywords: e2ee deferred, group encryption, forwarding peers can read, opaque payload

---

## Cluster 3 — Directed messaging and endpoints

### 0005 — Direct request-response protocol for one-to-one messages (Accepted)
- Rules: rust-libp2p `request_response` at `/interweave/direct/2.0.0`; each message is a bounded request carrying required source EndpointId and optional destination EndpointId; the reply is `AcceptedV2` with the resolved endpoint or a coarse `RejectedV2`; connections are reused, logical exchanges use independent substreams; **no automatic retry**. The architecture-only `/direct/1.0.0` is superseded before implementation and is not a compatibility target.
- Keywords: direct v2, request_response, acceptedv2, rejectedv2, no retry, substreams

### 0030 — Network-addressable local endpoints under one PeerId (Accepted) — **the Model B pillar**
One PeerId per profile with exclusive configured EndpointId leases and deterministic one-to-one routing.
- Rules: each direct-capable local session claims **one exclusive** configured EndpointId lease; destination is `{peer, endpoint?}` where an omitted endpoint means the receiver's **configured default**, never fan-out; the receiver resolves exactly one endpoint and sends `Accepted` only after enqueue to that endpoint's bounded queue; no local fan-out in v2; endpoint policy may **narrow, never widen** profile trust; leases are runtime-only and vanish on disconnect; EndpointIds are routing labels, not identity or authorization principals, and endpoint ACLs stay PeerId-based; no daemon-side buffering for an unavailable endpoint.
- Security: the sender cannot spoof a local source endpoint — the runtime derives it from the active local-session lease. A **remote** source EndpointId is peer-asserted metadata only. Claim conflicts fail closed.
- Supersedes ADR-0016 for routing semantics; 0016's profile identity rationale stands.
- Keywords: model b, endpointid, lease, default endpoint, no fan-out, narrow not widen, source endpoint derivation

### 0031 — Trust-gated remote endpoint directory (Accepted)
Optional `/interweave/endpoints/1.0.0` request-response.
- Rules: queries accepted only from a profile-trusted peer; the response is at most 32 lexicographically sorted EndpointIds that are simultaneously enabled, actively leased, `advertise: true`, and admissible for the querying peer; **no** display names, roles, avatars, client kinds, schemas, prompts, or trust claims are exposed; results are advisory, in-memory, short-lived; an explicit endpoint send needs no prior query; independently rate/concurrency bounded (12/min per remote PeerId, 16 in-flight per profile; ceilings 60 and 64).
- Keywords: endpoint directory, advertise, 32 endpoints, advisory cache, rate bounds

### 0018 — Realtime best-effort delivery only (Accepted)
- Rules: bounded local at-most-once presentation after deduplication; no global ordering, exactly-once, durable queue, or offline mailbox. For direct v2, **`AcceptedV2` means the remote transport resolved one EndpointId and enqueued into that endpoint's bounded queue** — it does not mean the human, Claude, or application processed or persisted anything.
- Keywords: best-effort, at-most-once, no ordering, what acceptedv2 proves

### 0019 — Bounded ephemeral duplicate suppression (Accepted)
- Rules: runtime-local LRU/TTL cache, default 10,000 entries / 5-minute TTL. Keys are `(broadcast, source_peer, channel, message_id)` and `(direct, source_peer, source_endpoint, destination_selector, message_id)`. A positive direct entry stores the first resolved endpoint plus **DirectContentFingerprintV1** (the SHA-256 canonicalization in `contracts/ENDPOINTS.md`); matching retries return the same acceptance and route without re-enqueue even if the default later changes; same key with different content is a duplicate-ID conflict and is rejected; persistence is prohibited. A bounded **in-flight reservation map** closes the concurrent-duplicate race — first request owns, duplicates share its result, different content fails immediately; 128 global / 8 per source peer by default (ceilings 512 / 32); a rejected owner removes the reservation without a positive entry so a later retry can succeed.
- Keywords: dedup, lru, ttl, content fingerprint, in-flight reservation, duplicate-id conflict

### 0020 — No persistent offline message store (Accepted)
- Rules: application messages are never written to disk for later network, endpoint, Claude, or human delivery; an unleased target endpoint yields `no_route` with nothing stored. ADR-0044 is the single narrow exception, and it lives in the human application store — not `TransportRuntime` — and creates no remote mailbox.
- Keywords: no mailbox, no offline store, no_route, transport is not durable

### 0016 — Profile-scoped identities with explicit endpoint multiplexing (Accepted; routing superseded by 0030)
- Rules: one persistent identity per named profile — not per conversation, not host-global; local applications share a profile only by explicitly selecting it; independent profiles have independent keys/state/sockets. **v1 direct fan-out is superseded by ADR-0030.** Broadcast remains per-client join-reference filtered.
- Keywords: profile identity, one peerid per profile, superseded fan-out

---

## Cluster 4 — Discovery and Kademlia

### 0006 — Explicit DiscoveryProvider interface (Accepted)
- Rules: an event-stream contract consumed **only** by DiscoveryManager; providers emit normalized candidate PeerIds, addresses, provenance, expiry, and health; providers never dial and never grant trust.
- Keywords: discoveryprovider, candidate events, provenance, no dialing

### 0007 — Concurrent discovery composition with configurable priority (Accepted)
- Rules: enabled providers run concurrently under DiscoveryManager and merge by PeerId/address provenance; priority and cost are configurable *guidance* for candidate selection, never a hard-coded sequence or a trust weight; active intensity may back off when connectivity is healthy.
- Keywords: composition, merge, priority guidance, backoff

### 0008 — v1 discovery providers are cache, optional mDNS, and static bootstrap (Accepted; rollout superseded by 0034)
- Rules: PeerCacheDiscovery, optional MdnsDiscovery, StaticBootstrapDiscovery form the minimum set. The **rollout** portion is superseded — the standard v1 build also includes Kademlia, default-enabled when configured. The three original providers' roles are unchanged.
- Keywords: mdns, static bootstrap, peer cache, minimum provider set

### 0009 — Kademlia as a trust-bounded peer-routing provider (Accepted) — **integration and security semantics**
- Rules (12 fixed semantics): **peer routing only** — `FIND_NODE`/closest-peers, never value or provider records, and never ChannelIds, membership, roles, trust documents, or payloads in the DHT; **private namespace** derived from the Kademlia wire major plus a non-secret `network_id` (never the public IPFS DHT); client mode by default with server mode an explicit operator choice; **manual routing-table admission** (`BucketInserts::Manual`) after address validation, trust policy, and Identify observation; **no untrusted discovery-only connections** — a routing peer must also be data-plane authorized, and every dial from Kademlia's own query engine passes ADR-0011's `DialAdmissionGate`; connection policy stays outside the provider; targeted lookup is capability-gated (independently trusted peer, fresh advisory observation that it advertised the exact server protocol, normal addresses unusable); bounded query/saturation strategy with an effective routing target and saturation back-off; lookup keys are cryptographically random or transport PeerIds — never derived from ChannelId, project names, contents, or application identity; routing state is ephemeral and advisory; record APIs disabled by policy with inbound record writes discarded; security-oriented fail-safe defaults.
- Keywords: kademlia, dht, peer routing only, no records, private namespace, manual insertion, capability-gated lookup, dial gate

### 0034 — Enable Kademlia by default in the standard v1 build (Accepted)
- Rules: the standard daemon build **must include** the implementation before it can be release-ready; a configured `type: kademlia` entry defaults to `enabled: true`; it stays **opt-out** (`enabled: false` means no task, advertisement, routing participation, or queries); composition remains explicit, so a profile that omits the entry does not get Kademlia; reduced builds must reject a defaulted-on entry as a hard startup error; **all ADR-0009 constraints remain**; SPIKE-003 becomes a v1 release gate; a Kademlia runtime failure degrades discovery rather than being transport-fatal.
- Keywords: kademlia default on, opt-out, release gate, reduced build must reject

### 0010 — Bootstrap peers are non-authoritative discovery hints (Accepted)
- Rules: a static bootstrap entry is a normal candidate that helps only when that PeerId is **separately authorized** by trust policy — configuration alone never authorizes; it carries no identity, trust-root, membership, channel-owner, coordination, storage, or broker authority.
- Keywords: bootstrap, not authority, reachability hint

### 0022 — Compile-time discovery providers with configuration composition (Accepted)
- Rules: replaceability via a Rust trait, a compile-time provider registry, namespaced typed config, and config-driven composition; runtime enable/disable/restart is designed for, but **no dynamic shared-library loading in v1**.
- Keywords: provider registry, compile-time, typed config, no dlopen

### 0027 — Peer cache is a discovery provider (Accepted)
- Rules: historical reachability and bounded authenticated **protocol observations** persist only through PeerCacheDiscovery; ConnectionManager/Identify adapters report successes to TransportRuntime, which feeds the provider as hints; GossipSub, Kademlia, and Claude never write cache state directly. Observations are freshness-bounded advisory facts keyed by exact opaque protocol identifier plus support state and time — they grant no trust, roles, membership, or current liveness.
- Keywords: peer cache, protocol observation, advisory, no liveness

---

## Cluster 5 — Connection, trust, and Internet reachability

### 0011 — Discovery and connection management are separate (Accepted) — **the dial-admission pillar**
- Rules: DiscoveryManager owns candidate knowledge; **ConnectionManager owns connection policy** — trust admission, reconnect, per-peer backoff, retention, limits. A data-plane candidate is not intentionally dialed unless trust policy authorizes it; an infrastructure-set peer may be dialed only for the ADR-0036 control purposes; a peer in neither set is closed. **Every outbound Swarm dial** — including behaviour-originated ones — passes an internal synchronous `DialAdmissionGate` enforcing destination class and dial purpose, current authorization, per-peer backoff, global pending-dial and connection limits, shutdown/drain state, and address policy. A behaviour may *request* a dial but never owns the decision; a denied dial must not silently reset retry state.
- Address-scoped failure: failure and backoff are tracked **per normalized address**, separately from peer-level punitive state. Recently authenticated addresses are preferred; a never-successful address failure does not push the whole PeerId into punitive backoff while a known-good address remains. A Noise handshake authenticating a different PeerId is an **address identity mismatch** — close, quarantine that address 30 minutes by default, record the provenance that supplied it, and do **not** penalise the expected peer.
- Keywords: connectionmanager, dialadmissiongate, behaviour-originated dial, address quarantine, identity mismatch, poisoned address, backoff

### 0012 — Deny-by-default static PeerId trust policy (Accepted)
- Rules: `PeerTrustPolicy` with a static allowlist, deny by default; **discovery never mutates it**; trust administration is a local privileged path, never a Channel tool driven by remote content. Applied consistently: connection admission, inbound message admission, outbound `send` (`UnauthorizedPeer` locally before dialing), broadcast propagation (unauthorized source ⇒ `Ignore`, per 0029), and revocation (evicts connectivity, emits events). Endpoint policy may only narrow. The local PeerId is intrinsically self-authorized but `send(local PeerId)` is `InvalidArgument` and never self-dials. Bootstrap config adds nobody to the allowlist. The ADR-0036 infrastructure set is separate and never widens this policy.
- Keywords: trust allowlist, deny by default, revocation, unauthorizedpeer, no self-dial, endpoint narrowing

### 0013 — Noise XX for libp2p TCP connection security (Accepted)
- Rules: Noise with the interoperable XX profile authenticates PeerIds and encrypts TCP; Yamux above it for streams.
- Keywords: noise xx, yamux, transport security

### 0035 — Mandatory v1 Internet reachability (Accepted) — **supersedes 0024**
The standard v1 build and release include the complete reachability stack.
- Rules: AutoNAT **v2 client** mandatory and active; Circuit Relay **v2 client transport** and reservation management mandatory and active; DCUtR mandatory and attempted for eligible trusted relayed connections; Identify mandatory and **explicitly wired** to the reachability/address manager (no assumption that libp2p components self-integrate); AutoNAT-server and relay-server are explicit infrastructure roles, disabled unless configured, and Android profiles are client-only; SPIKE-004 validates and tunes this fixed architecture rather than deciding whether it exists; a build omitting any of the three is non-standard and must advertise the limitation.
- Reachability state: `DirectInbound = Unknown | VerifiedPublic | NotVerified` and `RelayInbound = Unavailable | Partial | Ready`. `VerifiedPublic` needs fresh AutoNAT-v2 evidence from the configured minimum number of **distinct authorized probe servers**; configured or Identify-observed addresses never count. `Ready` needs the target reservation count.
- Reservations: 2 distinct relay PeerIds when direct is `Unknown`/`NotVerified`, 1 warm reservation when `VerifiedPublic`, maximum 4 active. Static configured relays by default; Identify-learned candidates require explicit opt-in and lose precedence to static ones. Relay discovery never uses Kademlia provider/value records.
- Path selection: reuse healthy direct → known direct addresses → after a bounded head-start, race a relay route → first authenticated usable path may satisfy the operation → DCUtR may upgrade in the background → a failed upgrade never tears down the working relay → after a stable upgrade, new streams prefer direct and the relayed connection retires after a grace period without pretending streams migrated. Every dial carries an attributable origin (`direct`, `relay-reservation`, `relay-circuit`, `autonat-probe`, `dcutr-hole-punch`) through the root gate.
- Advertisement: only AutoNAT-v2-verified direct addresses and live reservation addresses may be advertised; expired reservations are removed immediately; private/LAN or merely observed addresses are never promoted; relay-derived addresses are ephemeral and never identity, trust, bootstrap authority, or durable presence.
- Keywords: autonat v2, circuit relay v2, dcutr, reachability state, verifiedpublic, reservation targets, path selection, address advertisement, spike-004

### 0036 — Protocol-scoped connectivity infrastructure peers (Accepted)
A second authorization set that buys reachability without buying data-plane membership.
- Rules: `transport.connectivity.infrastructure.allowed_peers` is separate from `trust.allowed_peers`; class is `DataPlaneTrusted` (in trust set — wins if in both), `ConnectivityInfrastructureOnly`, or `Unauthorized`. Infrastructure-only peers may carry Noise/Yamux, Identify/bounded ping, AutoNAT v2 probe control, and relay reservation/circuit control — and **nothing else**: no DCUtR as application destination, no GossipSub, no direct v2, no endpoint directory, no Kademlia routing, no Channel/application trust. The root gate evaluates dial *purpose* as well as class. On an established infrastructure connection GossipSub excludes the peer from mesh exchange, direct and directory managers reject before payload admission, and Kademlia never inserts it. **Inbound relayed connections are evaluated against the authenticated remote application PeerId, not the relay's** — a trusted relay cannot smuggle an unauthorized source into the data plane. Every configured static AutoNAT/relay PeerId must appear in one of the two sets or configuration fails closed; discovery and Identify never modify either set.
- Keywords: infrastructure peer, control plane, protocol admission matrix, relay cannot smuggle, fail closed

### 0024 — Conservative v1 reachability (Historical; superseded by 0035)
- Rules: historical only — directly reachable TCP/LAN/static operation was once considered sufficient, with relay/AutoNAT/DCUtR deferred. **Read 0035 instead.**
- Keywords: superseded, deferred nat traversal, historical

---

## Cluster 6 — Local clients and IPC

### 0017 — Owner-protected UDS/named-pipe with endpoint-aware framing (Accepted)
- Rules: two owner-protected sockets per profile — data plane and administration; UTF-8 JSON framed with a four-byte big-endian length, payload bytes base64url; the data socket can never grant `admin.*` and the admin socket can never hold an endpoint lease or send application messages. **JSON body ceiling stays 131,072 bytes (128 KiB)**; implementation target is IPC v2. The data hello optionally claims one configured EndpointId, and direct-capable clients require a successful exclusive lease. Handshake errors are precise **locally** (`EndpointUnknown`, `EndpointDisabled`, `EndpointClientKindDenied`, `EndpointInUse`, `CapabilityDenied`) while the remote wire keeps the coarse `no_route` class. `human-client` receives `endpoints.query` by default only when the directory is enabled; `claude-channel` does not. IPC version is negotiated in hello, never configured. Keepalive: 30s interval, 10s timeout, three misses; **required by default for any connection claiming a lease** (omission ⇒ local `CapabilityDenied` before grant); expiry closes the connection and releases the lease; keepalive is liveness, not authentication.
- Keywords: ipc v2, uds, named pipe, 128 kib, hello handshake, lease claim, keepalive, capability denied, coarse no_route

### 0037 — Split data-plane and administrative IPC sockets (Accepted)
- Rules: `<profile>.sock` and `<profile>-admin.sock` are distinct authority domains; the data socket can never grant `admin.*` **regardless of `client.kind`**, and the admin socket can never acquire leases or send application traffic; both owner-protected by default with stricter ACLs permitted on admin. `client.kind` is endpoint-binding and configuration hygiene only — never the selector that turns a data connection into an administrator.
- Keywords: admin socket, authority domain, client.kind is not authority, privilege separation

### 0023 — Minimal Claude-facing transport tool surface (Accepted)
- Rules: seven tools — `broadcast`, `send(peer, endpoint?, …)`, `reply`, `join`, `leave`, `identity`, `status`. The bridge owns one configured EndpointId lease over IPC v2; `send.endpoint` selects the **remote** endpoint while the source always comes from the bridge lease; omitting the remote endpoint asks for the remote profile's configured default. `reply` uses route metadata from the inbound event including the remote source endpoint and the local lease epoch. **Not exposed:** trust approval/revocation, endpoint creation/rebinding, identity rotation/recovery, shutdown, forced discovery/Kademlia queries, private keys, raw Swarm/multiaddr internals. `peer_endpoints` is deliberately **not** a Claude tool and `claude-channel` gets no `endpoints.query` by default.
- Keywords: claude tools, send endpoint, reply token, lease epoch, not exposed, no peer_endpoints

### 0032 — Human client remains above transport (Accepted)
- Rules: the human client is an application above transport — desktop through IPC v2, Android through the embedded local-session adapter (ADR-0041). Its session owns one configured EndpointId such as `human`; it uses the same generic contracts as any local consumer; the UI/domain layer owns no libp2p policy or private keys; contacts, display names, avatars, verification state, conversation models, unread state, reactions, and attachment UX are application data. It keeps **no conventional permanent history** — only ADR-0044's three durable states. Trust mutation, endpoint configuration, discovery administration, and shutdown require the admin binding, and a data session can never self-upgrade. Network content can never invoke administrative capabilities. The UI must display EndpointId as a **remote peer-controlled route label** unless a higher-level identity system verifies a stronger binding. One desktop executable may host both surfaces but they are separate connections, authority domains, and IPC slots; on Android the split prevents confused-deputy wiring but is not an OS sandbox.
- Keywords: human client boundary, above transport, endpoint label warning, no self-upgrade, confused deputy

### 0040 — Desktop human client uses the shared daemon and IPC v2 (Accepted)
- Rules: messaging over the IPC v2 data socket with an exclusive configured lease; settings administration over the separate admin socket; ADR-0044 retention state lives in the human client, **never** the daemon; closing the UI releases the endpoint but does not stop the daemon by default.
- Keywords: desktop human client, admin socket, retention lives in client

### 0041 — Android human client embeds TransportRuntime in a foreground service (Accepted)
- Rules: the app embeds the Rust runtime in an Android foreground-service host rather than launching a daemon or exposing local TCP/UDS; the UI talks through the neutral in-process `LOCAL-CLIENT` adapter; the service owns the `human` lease while active. Continuous background reachability is explicit opt-in using the `remoteMessaging` foreground-service category subject to SPIKE-008 validation; foreground-only is supported; **no centralized push wake-up dependency**. `stay-reachable` combined with `user-presence` key unlock must expose `background_restart_requires_user_authentication=true` and must not claim automatic reachability after restart.
- Keywords: android, foreground service, localdatasession, no push dependency, spike-008, stay-reachable

---

## Cluster 7 — Identity and key material

### 0033 — Recover software PeerId with an optional 24-word mnemonic (Accepted)
- Rules: Ed25519 software identities; optional format `interweave-ed25519-bip39-entropy-v1` encoding the **exact 32-byte secret seed** as 256-bit BIP-39 entropy (English wordlist/checksum, exactly 24 words); **no PBKDF2 mnemonic-to-seed derivation and no BIP-39 passphrase**; backups pair the words with public `expected_peer_id` and format labels because the checksum is only 8 bits; export/restore are offline operations under identity-lock exclusivity and recovery material **never** crosses IPC, MCP, Channel, or the network; recovery must reproduce the expected PeerId exactly and fails closed on mismatch; SLIP-0039 splitting is future, not v1; hardware-backed identities may have no export at all.
- `transportctl identity verify` is a read-only drill: decode, derive, compare, discard — no key write, no profile mutation, no network. The phrase recovers **identity only**; full profile recovery also needs `config.yaml`, and human retention state is never restored by it.
- Keywords: mnemonic, bip-39, 24 words, exact seed, no passphrase, expected_peer_id, verify drill, identity only

### 0038 — Optional encrypted software identity at rest (Accepted)
- Rules: standard v1 stays `identity.key_protection=filesystem-only`; a v2.x passphrase-encrypted versioned envelope must decrypt to the **same portable Ed25519 identity and PeerId**; no bespoke cipher or KDF — SPIKE-007 must select and pin a maintained audited format providing a memory-hard password KDF and authenticated encryption, plus unlock UX and migration, before the option becomes selectable. The passphrase is separate from the recovery phrase and never appears in config, logs, network messages, Claude/MCP, the directory, or ordinary IPC.
- Keywords: encrypted at rest, passphrase, spike-007, audited format, same peerid

### 0042 — Android protects the portable seed with Keystore wrapping (Accepted)
- Rules: the Ed25519 secret is stored only as versioned authenticated ciphertext wrapped by an AES-256-GCM key generated in `AndroidKeyStore`, preferring hardware backing; explicit `background-compatible` and `user-presence` unlock policies; **never** silently substitute a different Android-native key algorithm.
- Keywords: android keystore, aes-gcm wrapping, user-presence, hardware backed

### 0043 — Concurrent human devices use distinct transport PeerIds (Accepted)
- Rules: each concurrently active device/profile has its **own** PeerId; the 24-word phrase restores or migrates one identity after loss or retirement and is **not** multi-device account synchronization; a client may group several PeerId/EndpointId routes under one person locally, but that association is not transport-authenticated.
- Keywords: multi-device, distinct peerids, not account sync, contact grouping is local

---

## Cluster 8 — Human application retention and UI stack

### 0039 — First-party human clients use a shared Rust core and Slint (Accepted)
- Rules: Rust owns domain logic, application protocol, storage, transport adapters, and view models; Slint is the reference GUI for Windows/macOS/Linux and Android; Slint and human models stay **above** transport contracts while IPC and network protocols stay language-neutral. Android may carry a minimal Java/Kotlin/JNI shim **only** for component/lifecycle/notification/Keystore APIs — no routing, trust, crypto policy, message parsing, or persistence logic there.
- Keywords: slint, rust core, jni shim scope, language-neutral ipc

### 0044 — Human messages are ephemeral by default (Accepted)
Durable only in three states, per the frozen contract in `clients/human/RETENTION.md`.
- Rules: content is durable only while **outbound pending** (not yet transport-terminal), **inbound unread** (committed but not yet read), or **inbound kept** (receiver explicitly chose `Keep` after reading). `AcceptedV2` is transport-terminal for direct sends; successful local publication is terminal for broadcast because no recipient acknowledgement exists. Transport-terminal outbound content is removed; read inbound content is removed unless kept. **`Keep` is receiver-only local state**, settable only after read, and never accepted from a remote payload, EndpointId, contact label, or sender request. A future encrypted backup may include **only** inbound unread and inbound kept content — pending outbound is excluded so a restored or second device never becomes an implicit replay/delayed-send source; Android system backup and device transfer stay disabled for all message-content storage.
- Keywords: retention, unread, keep after read, pending outbound, transport-terminal, no sender-forced persistence, backup exclusion

### 0050 — HumanChatV2 is markdown-native with fit-triggered bounded compression (Accepted)
Supersedes the HumanChatV1 envelope as the implementation target; no v1 implementation or compatibility obligation ever existed.
- Rules: `text` is CommonMark **0.31.2** plus the `table` and `strikethrough` grammars of GFM **0.29-gfm** (no other extension) under a closed subset — raw HTML renders as literal text, link schemes allowlisted (`https`, `mailto`), remote images never auto-fetched, nesting/table bounds, raw source always viewable; plain-text rendering is legal degradation with no negotiation. Compression is a **fit fallback only**: raw JSON whenever it fits `max_payload_bytes`, whole-envelope brotli (`;ce=br`) only when it does not, too-large otherwise — no chunking; a raw envelope over the decompressed ceiling is too large **before** compression, so the legal compression range is `max_payload_bytes < raw <= 196,608`. Receivers stream-decode under a hard **196,608-byte decompressed ceiling** (4 × the transport payload ceiling, set by this ADR), aborting mid-stream as soon as the output would EXCEED it, so exactly 196,608 decodes; there is deliberately no declared-length field. Decompression happens once, above transport, in a shared application library (desktop, Android, Claude bridge) — the daemon never decompresses; bridge defense-in-depth checks run on decompressed bytes, and `CHANNEL-EVENT.md` decodes a content-encoding parameter before classifying content (decoding is representation, not the application parsing that contract forbids). `DirectContentFingerprintV1` stays over wire bytes, so retries MUST resend stored byte-identical payloads (brotli output is non-canonical). Peer content is data, not instructions: agent-facing consumers frame it as untrusted with `source_peer`/`source_endpoint` provenance and never auto-follow links. Retention (ADR-0044) unchanged; attachments/artifact references stay out of scope for a future ADR.
- Security: prompt injection contained by provenance framing plus the ADR-0032/0037 authority split; decompression bombs (measured ≥87,381× expansion) bounded by the mid-stream cap; render injection closed by the subset; read-beacon exfiltration closed by never auto-fetching.
- Keywords: human chat v2, markdown, commonmark, compression, brotli, decompressed ceiling, decompression bomb, prompt injection, no auto-fetch

---

## Cluster 9 — Limits, configuration, and state

### 0026 — Bounded queues and conservative resource limits (Accepted)
- Rules: 48 KiB payload and 128 KiB IPC JSON-body ceilings; 128-byte ChannelId; 64-byte EndpointId; default 16 / max 64 configured endpoints; default 16 / max 32 advertised endpoints; one endpoint lease per local data-plane session; short-lived directory cache; direct dedup in-flight reservations bounded at 128 global / 8 per source peer (ceilings 512 / 32). Inbound direct is rejected as overloaded **before** `AcceptedV2` when the resolved endpoint queue is full. After Noise/trust admission every inbound direct request also consumes a per-trusted-PeerId token bucket (120/min, burst 32) and a global bucket (1200/min, burst 256); overflow returns coarse `overloaded` before endpoint routing. Broadcast keeps bounded best-effort local drop **and** consumes its own per-peer and global ingress buckets with the same defaults, accounted separately from direct's so neither mode spends the other's allowance; an over-rate broadcast is dropped before local delivery and still reported to the mesh (2026-08-27 amendment).
- Keywords: limits, 48 kib payload, token bucket, overloaded, queue bounds, endpoint caps, broadcast ingress rate

### 0028 — Separate config, identity, mutable state, cache, and runtime endpoints (Accepted)
- Rules: profile-specific platform directories for configuration (including endpoint definitions, default, and ACLs), the private identity key, mutable daemon state/logs, a replaceable peer cache, and the runtime socket/lock. **Endpoint leases and remote directory results are runtime state only** and are never persisted as authoritative configuration. Repository examples contain no private keys or secrets.
- Keywords: config vs state, profile directories, leases are runtime-only, no secrets in examples

---

## Superseded and historical pointers

| ADR | Status | Follow |
|---|---|---|
| 0008 | rollout portion superseded | **0034** (Kademlia in the standard build) |
| 0016 | direct-routing semantics superseded | **0030** (Model B endpoint routing) |
| 0024 | superseded | **0035** (mandatory Internet reachability) |
| 0005 | `/direct/1.0.0` superseded before implementation | **0005** itself, at `/interweave/direct/2.0.0` |

ADR bodies read **current** (ADR-0048): where a decision has been amended, the change is folded into the section it qualifies, so the body *is* the decision. Amendment notes live in [`history/`](./history/) and are for research — 0015 and 0037 each carry one, recording the Android deployment binding. Their `## Amendments` end-matter tables are the index into those notes.
