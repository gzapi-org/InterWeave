# Failure model

| Scenario | Expected behavior | Health / visibility | Recovery |
|---|---|---|---|
| no peers discovered | daemon stays up; publish has no recipient guarantee | discovery degraded, connected=0 | providers continue/backoff; add hints/config |
| one discovery provider fails | others continue | provider unavailable; aggregate may be degraded | provider restart with backoff |
| all discovery providers fail | existing trusted connections continue | discovery unavailable; transport may be degraded | independent retries/config fix |
| bootstrap unavailable | no authority loss; other sources/connections remain | bootstrap candidate/dial diagnostics | retry/backoff, alternate hints |
| static bootstrap DNS lookup fails | configured candidate remains valid; dial fails | connection/address-resolution diagnostic, **not** provider-health failure | ConnectionManager retry/backoff; fix DNS/address |
| mDNS unavailable | LAN auto-discovery absent | mdns unavailable | static/cache continue |
| Kademlia configured enabled but unsupported by build | profile fails configuration/startup; provider is never silently omitted | fatal explicit unsupported-provider diagnostic | disable it or install a build where approved provider exists |
| Kademlia unavailable after future implementation | other providers/connections continue | provider unavailable | provider restart/disable |
| Kademlia supported+enabled but no trusted eligible seed | provider starts but cannot bootstrap; no daemon failure | Kademlia unavailable/degraded, `routing_peers=0` | add/fix trusted server seed; other providers continue |
| Kademlia protocol/network namespace mismatch | peer cannot join local DHT routing view | protocol mismatch/query failure diagnostic | align `network_id`/wire major |
| Kademlia bootstrap/query timeout | query fails under bounded retry/rate budget | provider degraded; timeout counters | retry after backoff; other providers continue |
| Kademlia routing peer trust revoked | remove from routing table and normal connection set | trust change + Kademlia routing removal | reseed through remaining trusted peers |
| inbound Kademlia value/provider write | do not persist record | record-write-attempt counter | continue peer routing; investigate abusive peer |
| Kademlia driver channel overload | never block Swarm; mark provider degraded and coalesce/drop noncritical driver diagnostics | overflow counter | workload drains/backoff; tune bounded capacity if evidence supports |
| network partition | connections drop; local daemon lives | peer disconnects/dial failures | backoff + rediscovery/reconnect |
| PeerId changes intentionally | peers see new identity | `IdentityChanged` / epoch | out-of-band trust update |
| identity key missing on established profile | fail closed | daemon unavailable, explicit identity error | restore key or explicit reinitialize/rotate |
| corrupt identity key | fail closed; never auto-rotate | fatal profile error | restore/rotate by local admin |
| duplicate addresses | merge by PeerId/address | optional dedup counter | normal aggregation |
| connection storm | bounded dial concurrency/backoff; unauthorized candidates are not dialed | overload/dial-limit counters | drain under limits |
| unauthorized inbound connection | authenticate PeerId then close before data-plane participation | policy disconnect/reject counter | explicit local trust change if intended |
| unauthorized outbound direct send | fail locally before dialing | `UnauthorizedPeer` | add trust out of band or choose authorized peer |
| GossipSub message objectively invalid | report validation `Reject`; no local delivery/forwarding | `validation_reject_invalid` | peer/network continues; scoring policy applies |
| GossipSub source locally unauthorized | report validation `Ignore`; no local delivery/forwarding, no invalidity attribution solely for trust mismatch | `validation_ignore_unauthorized` | trust update or alternate mesh path |
| GossipSub publish failure | return failure to caller | publish failure counter | caller may retry explicitly |
| GossipSub mesh empty | publish may be locally accepted but no delivery claim | channel reachability degraded | discovery/connectivity recovery |
| broadcast without caller join | fail locally; no implicit join/publish | `ChannelNotJoined` | caller joins channel then retries |
| broadcast reply after caller leaves channel | reply token remains syntactically valid but operation fails; no implicit rejoin | `ChannelNotJoined` | explicit join then explicit broadcast/reply as appropriate |
| bridge/plugin restart | daemon/network stay up | IPC client disconnect/reconnect | fresh handshake/resubscribe; no replay |
| daemon restart | PeerId persists; network reconnects | daemon unavailable then recovering | cache/providers reconnect |
| Claude Code restart | same as bridge restart | Channel unavailable while closed | daemon stays up; no offline Channel queue |
| local IPC disconnect | network continues | bridge degraded | bounded reconnect |
| Claude bridge requests daemon shutdown | IPC denies because `admin.shutdown` is not granted | IPC authorization diagnostic | service manager/authorized local control client performs shutdown |
| slow Claude consumer | per-client event drops after queue fills | overload counter/event | consumer recovers; no replay |
| malformed payload/frame | reject before local delivery | invalid counter, peer-local error | continue serving others |
| oversized transport payload | reject pre-allocation/early | too-large counter | sender must reduce size |
| IPC JSON body > 128 KiB | reject frame/client operation before dispatch | frame-too-large diagnostic | fix serializer/input; legal max payload fixture must still fit |
| unsupported direct protocol | direct send fails | protocol mismatch | upgrade/compat config |
| IPC version mismatch | client refused | explicit incompatibility | update bridge/daemon |
| NAT blocks inbound | direct reachability limited | dial/listen diagnostics | public addr/relay; later NAT features |
| relay unavailable | affected relayed paths fail | relay diagnostics | alternate direct/relay path |
| trust revoked while connected | emit trust-policy change, close data-plane connection, stop admission/propagation | `TrustPolicyChanged`; `PeerDisconnected(reason_class=policy)` | explicit re-allow if intended |
| daemon event queue saturated | drop according to bounded policy | overload event/counters | load reduction/tuning |

## Fatal vs recoverable

Fatal profile startup: invalid config, an enabled provider unsupported by the active build, private-key corruption/unsafe permissions, profile lock conflict, incompatible persisted schema requiring migration, IPC bind security failure.

Recoverable: individual authorized-peer failures, provider failures, publish failures, partitions, transient dial-time DNS/mDNS failures, bridge disconnects, empty mesh, relay loss.
