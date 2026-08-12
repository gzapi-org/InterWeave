# Threat model

Scope: generic transport, local daemon/bridge boundary, and network participation. Application-level authorization is outside this transport.

Assumption: every remote payload is untrusted input even after the transport PeerId is allowlisted.

Trust boundaries:

1. remote network -> Noise/libp2p parser;
2. Noise-authenticated PeerId -> `PeerTrustPolicy` connection admission;
3. GossipSub original publisher / direct source -> message trust validation;
4. admitted payload -> resource/dedup pipeline;
5. daemon -> owner-protected, capability-scoped IPC;
6. bridge -> Claude Channel notification;
7. local administrator -> trust/key/config mutation.

| Threat | Risk | v1 mitigation | Residual risk | Future mitigation |
|---|---|---|---|---|
| Rogue peer | remote peer injects prompt content or joins data plane | Noise identity + deny-by-default PeerId allowlist before dialing/retaining ordinary data-plane connection and before message delivery | stolen/incorrectly allowed identity can connect and send untrusted content | signed membership, enterprise policy, richer channel-scoped auth |
| Discovery poisoning | bogus addresses cause waste/misdirection | discovery is advisory; address/peer bounds; ConnectionManager rate/backoff; **unauthorized candidates cannot pass outbound dial admission** | trusted peer's poisoned/stale addresses can still waste dial resources | source reputation, diversity policies |
| Malicious bootstrap | steers node to hostile topology | bootstrap has no authority/trust; configured candidate requires separate allowlist; multiple independent hints; existing peers survive | eclipse if trusted/bootstrap set is too narrow or incorrectly trusted | diverse bootstrap sets, rendezvous/DHT diversity |
| Kademlia poisoning | DHT returns hostile peers | default disabled; optional design uses trust-gated manual routing insertion, disjoint paths, query/candidate bounds, no records | compromised trusted routers can still bias observations; Sybil/eclipsing remains hard | measured diversity/scoring or alternate discovery backends |
| Sybil attack | many PeerIds exhaust candidate state or future public mesh | trust allowlist, candidate caps, no data-plane connection for unauthorized identities | discovery candidate storage/processing still attackable within bounds | admission credentials, stronger discovery diversity |
| Eclipse attack | malicious peers dominate usable view | static trust, source diversity, connection limits, multiple bootstrap hints | small/compromised trust sets can still be captured | topology diversity policies, independent discovery |
| Replay | old messages reappear | signed transport source + exactly-128-bit message IDs + 5-min bounded dedup keyed by mode/source/channel context | replay outside window/restart may deliver | higher-level nonce/session/replay protocol if required |
| Flooding | trusted peer overwhelms runtime/Claude | per-peer/global limits, bounded queues, drop counters, direct concurrency caps | allowed peer can cause message loss/degradation | configurable token buckets, peer scoring |
| Oversized payload | memory/CPU exhaustion | 48 KiB app cap, declared lengths checked before large allocation, 128 KiB IPC JSON-body cap with proven max-payload fit | many valid-size frames can still flood | rate limits/load shedding |
| Slow-consumer attack | Claude/IPC client stalls | independent bounded client queues; drop oldest ordinary events; reserved health lane | message loss | optional consumer flow-control/capability negotiation |
| Local IPC attack | another local process controls daemon | UDS/named-pipe owner ACL, peer credentials where supported, no loopback default, capability-scoped administrative methods | same-OS-user malicious process can still invoke ordinary commands if it can connect | local capability token/keychain binding, sandboxing |
| IPC daemon shutdown abuse | ordinary bridge kills shared network daemon | `shutdown` requires `admin.shutdown`; `claude-channel` kind never receives it | authorized local admin client can still stop daemon by design | stronger local operator identity if required |
| Private-key theft | attacker impersonates PeerId | owner-only key storage, no key over IPC/logs, explicit rotation/revocation guidance | compromise persists until trust lists updated | hardware-backed keys, signed rotation/revocation |
| Topic enumeration | channel names reveal context | hashed domain-separated topic IDs; untrusted peers are not admitted to ordinary local data-plane mesh | low-entropy names remain dictionary-guessable; a trusted peer can enumerate its visible topic hashes | keyed topic derivation with managed group secret |
| Message confidentiality | GossipSub forwarding participant reads payload | Noise per hop + trust-gated data-plane connections + explicit no-E2EE claim | **any trusted forwarding peer can read plaintext**; trust is not group encryption | standardized group E2EE/higher-layer encryption |
| Trust asymmetry in GossipSub | local allowlists differ; invalid mapping partitions or penalizes honest relay | ADR-0029: valid-but-unauthorized original source -> `Ignore`, objectively invalid -> `Reject`, authorized valid -> `Accept` | an `Ignore` node stops propagation, so a downstream peer may need another path | channel-scoped membership/policy or compatible overlay design |
| Prompt injection via network payload | remote text asks Claude to perform unsafe local actions | Channel instructions label remote content untrusted; normal Claude permissions; no trust-admin tool | model/user may still choose actions | application policy, stronger sandbox/approval controls |
| Trust-policy prompt injection | remote asks to approve itself | trust mutation absent from Channel tool surface; local-user-only admin rule | same-user local compromise bypasses | signed admin policy / managed config |
| Address SSRF-like abuse | malicious multiaddr makes dials to sensitive endpoints | address validation, allowed transport families, ConnectionManager policy, candidate limits, root trust gate before connection admission | trusted peer/private-network addresses may still be intended/ambiguous | deployment egress policy/deny ranges |
| Log leakage | payload/key/secret written to logs | payload logging false; secret redaction; structured classes | peer/channel identifiers can be sensitive | configurable pseudonymization, audit review |

## Broadcast confidentiality boundary

v1 does **not** rely on topic-name secrecy. The confidentiality boundary is the local profile's trusted data-plane peer set plus the fact that Noise protects each individual link. A PeerId that is merely discovered, cached, returned by future Kademlia, or configured as bootstrap is not admitted to ordinary GossipSub/direct connectivity unless separately trusted.

This still does not create end-to-end group secrecy. Any trusted peer that forwards a plaintext GossipSub message can inspect it.

## Security non-goals in v1

- proving application/person identity from PeerId;
- group end-to-end encryption;
- anonymous routing/metadata privacy;
- Byzantine consensus or membership;
- durable anti-replay across long offline periods;
- protection from malicious code running as the same OS user.


## Kademlia-specific threat treatment

Kademlia remains disabled by default. When the optional provider is implemented and explicitly enabled:

| Threat | Risk | v1 Kademlia mitigation | Residual risk | Future mitigation |
|---|---|---|---|---|
| malicious routing response | hostile/stale PeerIds and addresses | advisory candidates, trust gate, address/candidate caps, manual insertion | compromised trusted router can bias observations | stronger peer diversity/scoring/evidence fusion |
| Sybil routing population | many attacker PeerIds | first integration admits only data-plane-trusted routing peers | operator may trust attacker-controlled IDs | signed/enterprise membership policy, diversity controls |
| eclipse | local DHT view surrounded by malicious trusted peers | disjoint query paths, multiple independent bootstrap seeds, random exploration | no Byzantine guarantee | measured diversity policy / additional discovery systems |
| bootstrap capture | seeds bias initial view | multiple seeds; bootstrap is not authority; trust remains separate | all configured trusted seeds may be compromised | managed seed rotation/diversity monitoring |
| namespace collision/misconfiguration | unrelated deployment joins same private DHT | protocol derived from explicit `network_id`; custom protocol not public IPFS DHT | network_id is non-secret and can be copied/guessed | authenticated control-plane membership if required |
| record-store abuse | peers try to turn node into DHT storage | no record APIs; incoming inserts filtered/not persisted | request processing still consumes bounded resources | per-peer Kademlia request rate controls if needed |
| query traffic analysis | routing peers observe lookups | random exploration keys never encode channel/application names | PeerId/address/query timing still visible | privacy-preserving discovery backend if required |

The first Kademlia integration deliberately does **not** create untrusted discovery-only connections. Such connections would require per-protocol admission on multiplexed libp2p links and a renewed GossipSub confidentiality analysis.
