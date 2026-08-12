# Testing architecture

## Unit

- ChannelId validation/topic hashing fixtures;
- EndpointId grammar/length fixtures;
- endpoint config uniqueness/default/subset/advertisement cross-field validation;
- IPC keepalive config validation (`response_timeout < interval`, `require_for_endpoint_lease => keepalive.enabled`) and fixed v2 version-not-in-config rule;
- EndpointId claim without negotiated keepalive is `CapabilityDenied` when `require_for_endpoint_lease=true`, while endpoint-less admin/diagnostics sessions remain eligible;
- endpoint lease grant/conflict/release/revoke/epoch state machine;
- endpoint inbound/outbound policy is intersection with profile trust and cannot widen it;
- default-route resolution is deterministic and never connection-order-based;
- transport payload and IPC frame limit rejection before large allocation;
- exact 49,152-byte payload base64url/JSON sizing in request/event directions with max EndpointIds;
- discovery PeerId dedup/address merge/provenance expiry;
- provider lifecycle/health aggregation;
- known-but-unsupported provider config rejection when enabled;
- trust decisions independent of discovery/endpoints;
- outbound UnauthorizedPeer before dial;
- connection admission/revocation policy;
- GossipSub Accept|Ignore|Reject mapping;
- exact `GossipSubMessageIdV1` source+wire-sequence golden fixture, cross-publisher collision resistance, and authenticity-before-valid-cache ordering;
- broadcast dedup key `(mode, source_peer, channel, message_id)`;
- direct dedup key `(mode, source_peer, source_endpoint, destination_selector, message_id)` plus stored first resolved endpoint/DirectContentFingerprintV1;
- Direct v2 `media_type_len=0` decodes as **absent**, never empty string, and maps to `media_present=0` in DirectContentFingerprintV1;
- DirectContentFingerprintV1 binary canonicalization and golden SHA-256 fixture (`text/plain`, `hello` -> `3dad2f134909e51812e261b56c84b5ab040de681a9e900c9180b2e88a4b47efe`);
- direct in-flight reservation global/per-source-peer bounds and overload rejection; rate-limited duplicate retries do not re-enqueue or erase an existing positive dedup record;
- backoff/cancellation state machines;
- direct reply-token exact endpoint route + lease-epoch invalidation;
- broadcast reply-after-leave;
- IPC v2 framing/version/capability/endpoint handshake;
- endpoint-directory response filtering/sorting/bounds/TTL.

## Discovery provider conformance

Every provider runs `contracts/DISCOVERY-CONFORMANCE.md` tests. Endpoint routing/directory is not a DiscoveryProvider and must not enter this suite.

## Network integration

1. two trusted peers: Noise + direct v2 accepted/rejected/timeouts;
2. same PeerId with local `human` and `claude` leases: remote send to `human` reaches only human;
3. remote send to `claude` reaches only Claude;
4. omitted destination endpoint reaches exactly configured remote default;
5. missing/offline default returns coarse no_route / local RemoteEndpointUnavailable;
6. explicit unknown/offline/endpoint-policy-denied routes are indistinguishable remotely as no_route;
7. sender source endpoint is runtime-derived from the local-session lease (desktop IPC or Android embedded) and cannot be spoofed in command params;
8. AcceptedV2 is not emitted until target endpoint queue admission succeeds;
9. full target endpoint queue -> overloaded rejection, no false acceptance;
10. retry of an accepted default-routed message with matching content fingerprint returns the originally stored resolved endpoint even after default changes, with no second local delivery;
11. same dedup key/message ID with different payload/media fingerprint is rejected and not re-delivered;
12. concurrent matching same-key retries share one in-flight reservation, yield one local enqueue, and all observe the same acceptance/rejection;
13. same message ID from one peer but two source endpoints produces independent deliveries;
14. endpoint-specific trust subset narrows but never widens profile trust;
15. endpoint lease disconnect immediately removes route and advertised presence;
16. endpoint directory shows only active advertise=true routes allowed for querying peer;
17. endpoint directory disabled/unsupported does not break explicit endpoint send;
18. stale directory cache entry followed by endpoint shutdown yields normal no_route;
19. two distinct signed publishers reuse the same 16-byte broadcast envelope ID and produce distinct mesh-level GossipSub message IDs/deliveries;
20. a trusted publisher cannot suppress another publisher merely by prepublishing the other's envelope ID under its own PeerId;
21. endpoint directory query from untrusted peer yields no directory disclosure;
22. three trusted peers: GossipSub broadcast reaches subscribed peers;
23. human+Claude same profile both joined -> both receive broadcast because both joined;
24. only human joined -> Claude receives no broadcast;
25. mDNS/static/cache/discovery trust-gated paths retain prior behavior;
26. trust revocation closes data plane and removes directory/query access;
27. daemon restart preserves PeerId but starts with all endpoint leases offline.

## IPC/local integration

- configured endpoint claim succeeds;
- malformed endpoint claim -> InvalidArgument;
- unknown endpoint claim -> EndpointUnknown;
- disabled endpoint claim -> EndpointDisabled;
- allowed_client_kinds mismatch -> EndpointClientKindDenied;
- ungranted capability -> CapabilityDenied;
- second live claim for same EndpointId -> EndpointInUse;
- one connection cannot switch EndpointId without reconnect;
- direct-capable client without endpoint -> EndpointNotRegistered;
- client kind mismatch is rejected as configuration policy but is not treated as cryptographic auth in security tests;
- admin endpoint revoke requires `admin.endpoints` on the admin socket;
- data socket rejects `admin.endpoints`/`admin.shutdown` for every client kind, including `transportctl` spoofing;
- admin socket rejects EndpointId claims and ordinary application messaging capabilities;
- Claude/human data-plane connections use only the data socket;
- config disable revokes live endpoint without auto-rebinding;
- reconnect and daemon restart create a fresh non-repeating 128-bit lease epoch and stale reply routes fail;
- max legal payload fits under 131072-byte IPC v2 body with maximum endpoint metadata;
- IPC body above ceiling is rejected;
- human-client default grant includes endpoints.query only when directory is enabled; claude-channel default grant excludes endpoints.query;
- one human data-plane socket connection plus one human admin-socket connection consumes two total IPC slots and the admin connection consumes the admin sublimit;
- negotiated keepalive releases a wedged endpoint lease after configured missed probes; keepalive-disabled clients remain governed by OS connection liveness/admin revoke.

## Claude integration

- direct normalized event -> Channel metadata includes source_endpoint/destination_endpoint;
- transport `media_type` -> Claude `content_type`;
- non-UTF8 payload -> base64url metadata;
- `send(peer, endpoint, ...)` emits endpoint-addressed IPC command with no caller source endpoint field;
- `send(peer, ...)` requests remote default route;
- direct reply uses source peer/source endpoint and current local lease epoch;
- stale reply token after endpoint reconnect fails, no fallback;
- bridge endpoint collision is surfaced and not silently renamed;
- status includes local endpoint/lease epoch + joined channels;
- broadcast semantics and ChannelNotJoined remain unchanged;
- remote network message cannot invoke trust/endpoint/shutdown administration.

## Human-client architecture tests

- human endpoint and Claude endpoint share one PeerId without duplicate direct delivery;
- human client local history survives its own restart if its application store chooses, while daemon provides no missed-message replay;
- network send while human endpoint offline returns no_route and produces no daemon backlog;
- contact display name/avatar never changes transport identity/trust decision;
- endpoint-directory route label is displayed as unverified routing metadata unless app-level identity verifies it;
- explicit local user gesture/admin capability is required for trust or endpoint-config mutation;
- multiple UI windows share one application endpoint owner or use explicitly separate EndpointIds.

## Security/load

- rogue PeerId denied before endpoint/direct/directory data plane;
- endpoint enumeration is impossible for untrusted peers and bounded for trusted peers;
- malicious trusted peer probes many EndpointIds -> coarse no_route, rate/resource bounds hold;
- endpoint claim flood from local same-user processes remains bounded by IPC client limits;
- oversized endpoint/direct fields rejected pre-allocation;
- endpoint directory max 32 entries, per-peer query rate limit, global in-flight bound, and response size are enforced;
- flood cannot create unbounded endpoint/directory/cache/queue state;
- corrupt cache/config/identity behavior remains fail-safe;
- recovery export fixture decodes exact Ed25519 secret bytes; restore reproduces expected PeerId; wrong checksum/mismatched expected PeerId fails closed;
- `identity verify` derives/compares expected PeerId with no key-file write/profile mutation/network activity;
- recovery phrase never appears in logs, config, IPC fixtures, crash reports, or Channel events.

## Compatibility fixtures

Keep wire/IPC golden fixtures by major version.

Required endpoint-v2 fixtures:

- DirectMessageV2 with 64-byte source and destination EndpointIds and 49,152-byte payload;
- DirectMessageV2 with destination length zero (remote default route);
- DirectMessageV2 with `media_type_len=0` round-trips to absent media type and the `media_present=0` fingerprint branch;
- AcceptedV2 resolved-endpoint response;
- RejectedV2 no_route response;
- IPC v2 hello with endpoint claim and granted lease epoch;
- maximum legal IPC v2 direct request/event under 131,072-byte body;
- endpoint-directory empty/max-32 response;
- DirectContentFingerprintV1 canonical byte fixture and SHA-256 value;
- exact IPC endpoint-claim/capability error fixtures;
- identity recovery zero-secret -> 24-word phrase -> expected PeerId fixture.

Because no production v1 exists, Phase 1 does not require a v1 fan-out compatibility fixture. Unsupported major versions fail clearly.

## Kademlia standard-v1 provider test suite

These tests are a standard-v1 release gate before shipping configured Kademlia entries default-enabled. Tests also verify explicit `enabled: false` produces zero Kademlia activity.

### Configuration/unit

- disabled Kademlia creates no provider task/query/protocol/routing state;
- unsupported build + enabled=true is hard failure;
- validate network_id/mode/bounds/record_mode and cross-field constraints;
- deterministic protocol derivation fixture;
- random exploration independent of ChannelId/EndpointId/application input;
- manual K-bucket insertion refuses unauthorized peers;
- query budgets deterministic under fake time;
- PeerInfo normalization applies limits/TTL;
- peer-cache server-capability observations survive restart within TTL;
- KadCommand::Snapshot returns bounded correlated SnapshotResult.

### Multi-peer integration

1. trusted server seed + two clients bootstrap;
2. 10-20 trusted nodes converge within query bounds;
3. targeted lookup only with fresh exact server-protocol observation;
4. client-mode peers do not become Kademlia servers;
5. server-mode peer routing without value/provider records;
6. behavior-originated dials obey ConnectionManager policy;
7. small allowlist reaches effective target/saturation and backs off;
8. server-mode health consumes mandatory Phase-9 evidence: AutoNAT-verified direct or active relay reservation is strong; configured/Identify hints remain weak;
9. trust revocation removes Kademlia routing peer;
10. unauthorized returned peer cannot pass routing/dial admission;
11. provider provenance merges without duplicate identity;
12. namespace mismatch fails without public-DHT fallback;
13. Kademlia failure does not stop direct/GossipSub;
14. no EndpointId or endpoint-directory data is stored/queryable through Kademlia records.

### Adversarial/load

- hostile/stale address sets respect caps;
- Sybil/eclipse simulation measures disjoint path/bootstrap diversity;
- query flood budgets hold;
- PUT_VALUE/ADD_PROVIDER flood creates no record growth;
- driver event flood preserves Swarm responsiveness;
- repeated bootstrap failure degrades without restart storm.


## Mandatory Phase 9 connectivity tests

These tests are standard-v1 release tests, not optional hardening tests.

1. AutoNAT v2 successful probes from the configured number of distinct authorized servers produce `verified_public`; one stale/expired observer is insufficient.
2. conflicting/failed evidence cannot advertise an unverified direct public address as verified.
3. private/not-verified node reaches two active relay reservations by default; verified-public node retains one warm reservation.
4. reservation loss triggers bounded replacement and removes the expired relay-derived advertised address immediately.
5. all authorized relays unavailable produces explicit degraded/unavailable relay state without widening authorization.
6. infrastructure-only peer can carry Identify/AutoNAT/relay control traffic but cannot join GossipSub, direct v2, endpoint directory, or Kademlia routing.
7. a relayed inbound application connection is accepted/rejected using the end application's PeerId trust, not relay trust.
8. direct dial wins when available; relay may race only after the configured head-start and remains fallback.
9. successful DCUtR creates a direct path, observes the stability interval, then retires only redundant relay path; existing streams are not asserted to migrate.
10. failed DCUtR leaves the relayed path working and enters per-peer cooldown.
11. global/per-peer dial and connection ceilings apply to AutoNAT/relay/DCUtR behaviour-originated work.
12. relay server role enforces reservation/circuit/per-peer/duration/byte/pending-control limits when enabled.
13. AutoNAT server role enforces authorization, concurrency and rate limits when enabled.
14. network-interface change invalidates affected evidence, refreshes reservations/advertisement and preserves PeerId/EndpointId leases.
15. Kademlia never inserts connectivity-infrastructure-only peers into routing tables.
16. Model B direct routing remains identical over direct vs relayed peer paths.
17. GossipSub delivery remains application-trust-gated over direct and relayed connections.
18. `ConnectivitySummary` and `ConnectivityChanged` expose normalized state without leaking application payloads or granting control authority.
19. runtime class changes reconcile GossipSub/Kademlia/application state before privilege change; close/reopen fallback is safe if in-place transition cannot be atomic.

## Security review additions (2026-08-12)

Phase 1/2 contract and wire fixtures additionally assert:

- `GossipSubMessageIdV1` canonical source PeerId bytes + `u64be` wire-sequence mapping and the repository golden hash;
- `AcceptedV2` response message ID and `resolved_endpoint` are validated before cache/tool/UI use; explicit routes require exact endpoint equality;
- endpoint-directory responses reject >32 entries, invalid grammar and duplicates; valid unsorted responses are sorted locally; remote TTL is clamped and starts from local receipt;
- keepalive nonces are 128-bit CSPRNG values, only one is outstanding, and stale/wrong pongs do not satisfy liveness;
- data socket can never grant `admin.*` even when `client.kind=transportctl`; admin socket cannot claim an EndpointId or send/receive application messages.

Phase 4/7 security tests additionally assert:

- 65th default pre-Noise pending inbound handshake is refused and 10-second timeout frees a slot; per-source/global rate windows are enforced before PeerId state exists;
- a poisoned never-successful address for trusted PeerId A that authenticates as B is quarantined as an address failure and does not suppress a concurrently known-good A address or advance A's peer punitive tier solely from the mismatch;
- trusted-peer direct ingress token buckets enforce 120/minute burst-32 per PeerId and 1200/minute burst-256 global defaults with coarse `overloaded` response;
- all `no_route` causes have identical wire code/shape; exact timing equivalence is not asserted and remains a documented residual side channel.

## Desktop and Android human-client architecture tests

Shared conformance:
- desktop IPC and Android embedded adapters run the same LOCAL-CLIENT session/lease/source-endpoint/queue tests;
- HumanChatV1 parser rejects oversize/invalid JSON and treats direct transport metadata as authoritative;
- desktop and Android exchange the same transport/direct/GossipSub golden fixtures.

Desktop:
- daemon start/attach, shared profile with Claude, data/admin socket split, UI crash/reconnect, daemon restart, SQLite migration failure isolation.

Android:
- Activity recreation while service remains; service/process kill/restart; foreground-only vs stay-reachable; background-start denial; persistent-notification lifecycle; Wi-Fi/cellular transition; Kademlia client-only; relay fallback/DCUtR; mDNS multicast/permission lifecycle; Keystore wrap/user-presence/background-compatible modes; message callback graph cannot access LocalAdminPort;
- recovery screen uses secure-window/task-snapshot protection, has no clipboard or normal free-text mnemonic path, and phrase material does not enter saved state/log/analytics/crash artifacts;
- Android system cloud backup and device-transfer exclude identity envelope/recovery/config/trust/human SQLite state; a half-restored install cannot silently recreate/replace an established PeerId;
- `stay-reachable + user-presence` exposes `background_restart_requires_user_authentication=true` and remains offline after restart until user authentication.

HumanChatV1:
- `app_message_id`/`reply_to` accept exactly 32 lowercase hexadecimal characters and reject uppercase/prefixed/hyphenated/wrong-length encodings;
- `sent_at_ms` accepts only `0..253402300799999` and remains diagnostic only;
- unknown `reply_to` renders the current message without transport lookup or rejection.

Phase-9 V-review additions:
- authorized AutoNAT probe client requests loopback/private/ULA/link-local/multicast/unrelated-public/DNS targets and server emits no dial; only literal candidate IP equal to observed requester source IP is eligible;
- Identify infrastructure auto-candidate flags default false; when enabled static candidates win until target cannot be met;
- relayed inbound handshakes with no source IP consume the relay-connection/PeerId pre-auth bucket plus global cap;
- relay service refuses unauthorized/open-anonymous reservations under standard policy;
- `connectivity()` succeeds with ordinary `commands`;
- stable DCUtR upgrade yields one PeerConnected followed by PeerPathChanged, never a second logical connect event.
