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
- broadcast dedup key `(mode, source_peer, channel, message_id)`;
- direct dedup key `(mode, source_peer, source_endpoint, destination_selector, message_id)` plus stored first resolved endpoint/DirectContentFingerprintV1;
- Direct v2 `media_type_len=0` decodes as **absent**, never empty string, and maps to `media_present=0` in DirectContentFingerprintV1;
- DirectContentFingerprintV1 binary canonicalization and golden SHA-256 fixture (`text/plain`, `hello` -> `3dad2f134909e51812e261b56c84b5ab040de681a9e900c9180b2e88a4b47efe`);
- direct in-flight reservation global/per-source-peer bounds and overload rejection;
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
7. sender source endpoint is daemon-derived from IPC lease and cannot be spoofed in command params;
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
19. endpoint directory query from untrusted peer yields no directory disclosure;
20. three trusted peers: GossipSub broadcast reaches subscribed peers;
21. human+Claude same profile both joined -> both receive broadcast because both joined;
22. only human joined -> Claude receives no broadcast;
23. mDNS/static/cache/discovery trust-gated paths retain prior behavior;
24. trust revocation closes data plane and removes directory/query access;
25. daemon restart preserves PeerId but starts with all endpoint leases offline.

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
- admin endpoint revoke requires `admin.endpoints`;
- Claude/human data-plane connections never receive `admin.endpoints`/`admin.shutdown`;
- config disable revokes live endpoint without auto-rebinding;
- reconnect and daemon restart create a fresh non-repeating 128-bit lease epoch and stale reply routes fail;
- max legal payload fits under 131072-byte IPC v2 body with maximum endpoint metadata;
- IPC body above ceiling is rejected;
- human-client default grant includes endpoints.query only when directory is enabled; claude-channel default grant excludes endpoints.query;
- one human data-plane connection plus one human admin connection consumes two IPC client slots;
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
