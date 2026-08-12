# Failure model

| Scenario | Expected behavior | Health / visibility | Recovery |
|---|---|---|---|
| no peers discovered | daemon stays up; publish has no recipient guarantee | discovery degraded, connected=0 | providers continue/backoff; add hints/config |
| one discovery provider fails | others continue | provider unavailable; aggregate may be degraded | provider restart with backoff |
| all discovery providers fail | existing trusted connections continue | discovery unavailable; transport may be degraded | independent retries/config fix |
| bootstrap unavailable | no authority loss; other sources/connections remain | bootstrap candidate/dial diagnostics | retry/backoff, alternate hints |
| static bootstrap DNS lookup fails | candidate remains; dial fails | connection/address-resolution diagnostic, not provider health | ConnectionManager retry/backoff |
| mDNS unavailable | LAN discovery absent | mdns unavailable | static/cache continue |
| Kademlia enabled/default-enabled on a reduced build without implementation | profile fails startup/config | fatal unsupported-provider diagnostic | use standard v1 supporting build or explicitly disable/omit Kademlia |
| Kademlia runtime failure | other providers/connections continue | provider unavailable/degraded | provider restart/disable |
| Kademlia no trusted eligible seed | cannot bootstrap but daemon stays up | routing_peers=0 | add/fix trusted server seed |
| Kademlia namespace mismatch | no shared DHT routing | query/protocol failure | align network_id/wire major |
| Kademlia query timeout | bounded query fails | provider degraded counters | retry/backoff |
| Kademlia routing peer trust revoked | remove from DHT/connection set | trust + routing removal | reseed through trusted peers |
| inbound Kademlia record write | never persist | record-write-attempt counter | continue routing |
| network partition | connections drop; daemon lives | disconnect/dial failures | rediscovery/reconnect |
| PeerId intentional rotation | peers see new identity | IdentityChanged | out-of-band trust update |
| identity key missing/corrupt | fail closed; never silent rotate | daemon unavailable | restore exact Ed25519 identity from approved recovery record or explicit new-identity action |
| recovery phrase decodes but derived PeerId mismatches expected backup metadata | refuse restore | identity-recovery mismatch | verify phrase/record; never overwrite established identity |
| pre-Noise inbound handshake budget/rate exhausted | close/refuse unauthenticated attempts before PeerId admission | preauth pending/rate/timeout counters | source/global window drains; deployment firewall may block abusive sources |
| connection storm | bounded dial concurrency/backoff | overload counters | drain under limits |
| trusted PeerId candidate address authenticates as another PeerId | close mismatch; quarantine/failure-score that address only | address_identity_mismatch + provenance | try eligible known-good address; peer-wide punitive backoff not advanced solely by mismatch |
| unauthorized inbound connection | authenticate then close before data-plane participation | policy counter | explicit trust update if intended |
| unauthorized outbound direct send | fail before dial | UnauthorizedPeer | out-of-band trust change |
| local client claims unknown endpoint | IPC handshake fails | EndpointUnknown | fix endpoint ID/config |
| local client claims disabled endpoint | IPC handshake fails | EndpointDisabled | enable endpoint or choose another |
| local client kind violates endpoint hygiene policy | IPC handshake fails | EndpointClientKindDenied | correct config/client kind |
| local client requests ungranted capability | request/handshake fails locally | CapabilityDenied | use authorized client/policy |
| data-socket client claims `transportctl` and requests admin.* | categorical CapabilityDenied; never dispatched | admin-domain denial | connect to authorized admin socket |
| admin socket unavailable/ACL denied | data-plane transport continues; administrative operations unavailable | admin IPC degraded/unavailable | repair local ACL/socket/service; do not widen data socket |
| two clients claim same EndpointId | second handshake fails | EndpointInUse | close owner or configure another endpoint |
| endpoint disabled while leased | lease revoked; no reroute | EndpointLeaseChanged(revoked) | re-enable/reconnect explicitly |
| human/Claude endpoint process disconnects | route becomes immediately unavailable; no queue | endpoint offline/release | client reconnect/reclaim |
| direct explicit endpoint unavailable/unknown/policy-denied | remote sends coarse no_route | sender sees RemoteEndpointUnavailable; receiver local reason counter | correct route/policy/start endpoint |
| direct omitted endpoint but receiver has no usable default | coarse no_route | RemoteEndpointUnavailable/default-route diagnostic | configure/start default endpoint or address explicit endpoint |
| retry of previously accepted default-routed direct after default/endpoint state changes | return cached AcceptedV2 for original resolved route without second local delivery while dedup entry lives | duplicate-accepted diagnostic | caller treats as prior transport acceptance; after TTL no idempotency guarantee |
| same direct dedup key reused with different payload/media | reject as duplicate-ID/content conflict; never deliver second body | local duplicate-conflict counter; coarse malformed wire reason | sender generates a new MessageId |
| direct ingress token bucket exhausted for trusted peer/global | coarse overloaded before endpoint route work | direct_ingress_rate_limited | retry only after retry-after/window; investigate abusive peer |
| direct target endpoint queue full | reject before Accepted | endpoint overload counter | consumer/load recovery |
| remote AcceptedV2 contains invalid/mismatched resolved endpoint | fail operation locally as ProtocolViolation; do not cache/surface metadata | peer protocol-violation diagnostic | peer upgrade/fix; caller may retry only after compatibility issue resolved |
| endpoint directory response invalid, duplicate, or >32 entries | reject response as ProtocolViolation; cache unchanged | directory protocol-violation diagnostic | query fixed/upgraded peer |
| endpoint directory response valid but unsorted or TTL exceeds local ceiling | sort locally; clamp TTL from receipt time | noncanonical-order/ttl-clamped diagnostic | no operator action normally |
| stale direct reply token after lease epoch change | fail locally; no fallback | stale-route diagnostic | use a new inbound route/explicit send |
| endpoint directory disabled/unsupported | explicit endpoint sends still work | ProtocolUnsupported/disabled status | manual/out-of-band route or enable directory |
| endpoint directory returns stale route | send may receive no_route | stale-directory/send failure | refresh/retry after current state known |
| endpoint directory query by untrusted peer | no directory data exposed | policy reject counter | trust explicitly if intended |
| endpoint directory query budget exceeded | reject/rate-limit without route list; direct sends remain unaffected | directory rate/overload counter | client honors cache/backoff |
| GossipSub invalid | Reject; no local delivery/forwarding | validation_reject_invalid | continue |
| GossipSub source locally unauthorized | Ignore; no local delivery/forwarding | validation_ignore_unauthorized | trust update/alternate path |
| GossipSub publish failure | return failure | publish failure counter | explicit caller retry |
| GossipSub mesh empty | no delivery claim | channel degraded | connectivity recovery |
| broadcast without caller join | fail locally | ChannelNotJoined | join then retry |
| broadcast reply after leave | fail; no implicit rejoin | ChannelNotJoined | explicit join |
| profile-desired broadcast with no joined local client | validate/propagate then local drop; no buffer | no-local-consumer | future realtime join |
| direct send to own PeerId | fail locally | InvalidArgument | local IPC/app communication instead |
| bridge/human-client restart | daemon/network stay up; endpoint lease drops/reacquires | client disconnect/endpoint state | fresh handshake; no replay |
| daemon restart | PeerId persists; all endpoint leases initially offline | recovering | local clients reconnect |
| Claude Code restart | bridge route offline temporarily | Channel unavailable while closed | no offline queue |
| local IPC disconnect | network continues; endpoint lease released | client degraded | bounded reconnect |
| endpoint-claiming IPC client omits required keepalive | handshake/claim denied before lease grant | CapabilityDenied | negotiate keepalive or explicitly relax profile policy |
| local IPC connection half-open/wedged with keepalive negotiated | daemon closes after bounded missed probes and releases endpoint lease | keepalive timeout / EndpointLeaseChanged(released) | client reconnects; tune/disable keepalive requirement only by explicit policy |
| Claude requests endpoint admin/shutdown | IPC capability denial | authorization diagnostic | explicit admin path |
| slow broadcast consumer | per-client broadcast drops when queue full | overload | consumer recovers; no replay |
| slow direct endpoint consumer | new direct requests reject overloaded before Accepted | overload | consumer recovers |
| malformed/oversized frame | reject early | invalid/too-large counter | sender fixes |
| IPC JSON body >128 KiB | reject before dispatch | frame-too-large | fix serializer/input |
| unsupported direct v2 | direct send fails | protocol mismatch | upgrade peer |
| IPC v2 mismatch | client refused | incompatibility | update client/daemon |
| AutoNAT cannot verify direct inbound reachability | keep daemon up; classify `unknown`/`not_verified` | normalized connectivity + probe diagnostics | maintain required relay target; bounded probe retry |
| one relay reservation lost | existing other paths continue | `relay_inbound=partial` until replaced | acquire alternate authorized relay with backoff |
| all relay reservations unavailable while not verified-public | inbound Internet reachability unavailable/degraded | `relay_inbound=unavailable` + reservation diagnostics | retry only authorized relays; existing direct/outbound sessions may survive |
| relay service at capacity/denies reservation | candidate unavailable | reservation outcome/rate diagnostics | try alternate authorized relay; bounded retry |
| DCUtR hole punch fails | working relay path remains | hole-punch failure + cooldown | retry later after cooldown; no message-level retry |
| DCUtR succeeds | direct path established | path-change diagnostic | hold relay until direct stability interval then retire redundant path |
| network interface/address changes | prior evidence/routes may be stale | connectivity state transitions | invalidate affected evidence, rebuild reservations/advertisement |
| connectivity-infrastructure peer attempts application protocol | deny that protocol; connection class unchanged | protocol-denied diagnostic | configuration change only if operator intentionally grants data-plane trust |
| trust revoked while connected | close data-plane connection and remove endpoint directory/query access | trust + disconnect events | explicit re-allow |
| daemon event queue saturated | bounded drop/reject | overload | load reduction/tuning |

## Fatal vs recoverable

Fatal profile startup includes invalid schema-v2 endpoint configuration, enabled unsupported provider, private-key corruption/unsafe permissions, profile lock conflict, incompatible persisted schema, and IPC bind security failure.

Recoverable includes endpoint client downtime, route staleness, trusted-peer failures, provider failures, partitions, bridge/human disconnects, empty mesh, and relay loss.


## Mandatory reachability failures

Phase 9 is part of standard v1. Reachability failure is therefore represented explicitly rather than deferred to a future feature phase.

- Loss or disagreement of AutoNAT observers changes direct-inbound evidence; it does not revoke PeerId trust.
- A private/not-verified node targets redundant relay reservations. Partial reservation coverage is `degraded`; zero usable reservations is `unavailable` for relay inbound.
- Relay denial/capacity/exhaustion is retried only against authorized relay candidates and never broadens trust automatically.
- Failed DCUtR retains the working relay connection and enters per-peer cooldown. Successful DCUtR waits for direct-path stability before redundant relay retirement.
- A connectivity-infrastructure-only peer that negotiates an application protocol is rejected; such a violation does not upgrade its connection class.
- No failure path creates a durable message queue or changes direct-message acceptance semantics.

## Human platform failures

| Failure | Required behavior |
|---|---|
| Desktop UI crash | release IPC endpoint lease; daemon/other endpoints continue |
| Desktop admin socket unavailable | messaging continues; settings/admin unavailable |
| Android Activity destroyed | if foreground service lives, runtime/endpoint continue; UI rebinds |
| Android foreground service/process killed | revoke embedded session/endpoint; stop network activity; restart rebuilds ephemeral state; no queued delivery |
| Android background service start denied | report offline/reachability-disabled to UI; do not fake availability |
| Android Keystore unwrap fails/invalidated | fail established profile identity unlock; never silently generate new PeerId |
| Android network changes | invalidate direct evidence/affected paths; rebind/reconcile relays/Kademlia; preserve identity/config/history |
| AutoNAT server request target mismatches observed source IP | reject probe before dial; record bounded policy failure |
| Identify-learned infrastructure disabled | ignore as candidate; no health failure if static target is satisfied |
| DCUtR adds stable direct path | emit PeerPathChanged, not a duplicate logical PeerConnected |
