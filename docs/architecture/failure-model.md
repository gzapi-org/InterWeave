# Failure model

| Scenario | Expected behavior | Health / visibility | Recovery |
|---|---|---|---|
| no peers discovered | daemon stays up; publish has no recipient guarantee | discovery degraded, connected=0 | providers continue/backoff; add hints/config |
| one discovery provider fails | others continue | provider unavailable; aggregate may be degraded | provider restart with backoff |
| all discovery providers fail | existing connections continue | discovery unavailable; transport may be degraded | independent retries/config fix |
| bootstrap unavailable | no authority loss; other sources/connections remain | bootstrap provider degraded | retry/backoff, alternate hints |
| mDNS unavailable | LAN auto-discovery absent | mdns unavailable | static/cache continue |
| Kademlia unavailable | v1 unaffected; future provider isolated | provider unavailable | provider restart/disable |
| network partition | connections drop; local daemon lives | peer disconnects/dial failures | backoff + rediscovery/reconnect |
| PeerId changes intentionally | peers see new identity | `IdentityChanged` / epoch | out-of-band trust update |
| identity key missing on established profile | fail closed | daemon unavailable, explicit identity error | restore key or explicit reinitialize/rotate |
| corrupt identity key | fail closed; never auto-rotate | fatal profile error | restore/rotate by local admin |
| duplicate addresses | merge by PeerId/address | optional dedup counter | normal aggregation |
| connection storm | bounded dial concurrency/backoff | overload/dial-limit counters | drain under limits |
| GossipSub publish failure | return failure to caller | publish failure counter | caller may retry explicitly |
| GossipSub mesh empty | publish may be locally accepted but no delivery claim | channel reachability degraded | discovery/connectivity recovery |
| bridge/plugin restart | daemon/network stay up | IPC client disconnect/reconnect | fresh handshake/resubscribe; no replay |
| daemon restart | PeerId persists; network reconnects | daemon unavailable then recovering | cache/providers reconnect |
| Claude Code restart | same as bridge restart | Channel unavailable while closed | daemon stays up; no offline Channel queue |
| local IPC disconnect | network continues | bridge degraded | bounded reconnect |
| slow Claude consumer | per-client event drops after queue fills | overload counter/event | consumer recovers; no replay |
| malformed payload/frame | reject before local delivery | invalid counter, peer-local error | continue serving others |
| oversized payload | reject pre-allocation/early | too-large counter | sender must reduce size |
| unsupported direct protocol | direct send fails | protocol mismatch | upgrade/compat config |
| IPC version mismatch | client refused | explicit incompatibility | update bridge/daemon |
| NAT blocks inbound | direct reachability limited | dial/listen diagnostics | public addr/relay; later NAT features |
| relay unavailable | affected relayed paths fail | relay diagnostics | alternate direct/relay path |
| trust revoked while connected | stop local delivery; disconnect according to policy | trust-change event | explicit re-allow if intended |
| daemon event queue saturated | drop according to bounded policy | overload event/counters | load reduction/tuning |

## Fatal vs recoverable

Fatal profile startup: invalid config, private-key corruption/unsafe permissions, profile lock conflict, incompatible persisted schema requiring migration, IPC bind security failure.

Recoverable: individual peer failures, provider failures, publish failures, partitions, transient DNS/mDNS, bridge disconnects, empty mesh, relay loss.
