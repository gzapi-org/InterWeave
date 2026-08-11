# Threat model

## Assets and trust boundaries

Assets: local Claude session integrity, private PeerId key, trust configuration, message confidentiality/integrity, daemon availability, local host resources.

Trust boundaries:

1. remote network -> Noise/libp2p parser;
2. authenticated PeerId -> PeerTrustPolicy;
3. admitted payload -> resource/dedup pipeline;
4. daemon -> owner-protected IPC;
5. bridge -> Claude Channel notification;
6. local administrator -> trust/key/config mutation.

| Threat | Risk | v1 mitigation | Residual risk | Future mitigation |
|---|---|---|---|---|
| Rogue peer | remote peer injects prompt content | Noise identity + deny-by-default PeerId allowlist before Channel delivery | stolen/incorrectly allowed identity can send untrusted content | signed membership, enterprise policy, richer channel-scoped auth |
| Discovery poisoning | bogus addresses cause waste/misdirection | discovery is advisory; address/peer bounds; ConnectionManager rate/backoff; trust independent | dial resources can still be wasted | source reputation, diversity policies |
| Malicious bootstrap | steers node to hostile topology | bootstrap has no authority/trust; multiple independent hints; existing peers survive | eclipse if sole viable entry point | diverse bootstrap sets, rendezvous/DHT diversity |
| Kademlia poisoning | DHT returns hostile peers | Kademlia deferred; future trust separation, query/candidate bounds | Sybil/eclipsing remains hard | peer diversity/scoring, monitored seed diversity |
| Sybil attack | many PeerIds exhaust resources/mesh | trust allowlist, peer/candidate/concurrency caps | public/AllowAll future modes vulnerable | admission credentials, stake/reputation not in v1 |
| Eclipse attack | malicious peers dominate view | static trust, source diversity, connection limits, multiple bootstrap hints | small trust sets can still be captured | topology diversity policies, independent discovery |
| Replay | old messages reappear | signed transport source + message IDs + 5-min bounded dedup | replay outside window/restart may deliver | higher-level nonce/session/replay protocol if required |
| Flooding | trusted peer overwhelms runtime/Claude | per-peer/global limits, bounded queues, drop counters, direct concurrency caps | allowed peer can cause message loss/degradation | configurable token buckets, peer scoring |
| Oversized payload | memory/CPU exhaustion | 48 KiB app cap, framing length checked before allocation, 64 KiB IPC frame | many valid-size frames can still flood | rate limits/load shedding |
| Slow-consumer attack | Claude/IPC client stalls | independent bounded client queues; drop oldest ordinary events; reserved health lane | message loss | optional consumer flow-control/capability negotiation |
| Local IPC attack | another local process controls daemon | UDS/named-pipe owner ACL, peer credentials where supported, no loopback default | same-OS-user malicious process can connect | local capability token/keychain binding, sandboxing |
| Private-key theft | attacker impersonates PeerId | owner-only key storage, no key over IPC/logs, explicit rotation/revocation guidance | compromise persists until trust lists updated | hardware-backed keys, signed rotation/revocation |
| Topic enumeration | channel names reveal context | hashed domain-separated topic IDs | low-entropy names dictionary-guessable; peers see subscribed topic hash | keyed topic derivation with managed group secret |
| Message confidentiality | forwarding GossipSub peer reads payload | Noise per hop; restrict data-plane peers via trust; explicit no-E2EE claim | authorized intermediary sees plaintext | standardized group E2EE/higher-layer encryption |
| Prompt injection via network payload | remote text asks Claude to perform unsafe local actions | Channel instructions label remote content untrusted; normal Claude permissions; no trust-admin tool | model/user may still choose actions | application policy, stronger sandbox/approval controls |
| Trust-policy prompt injection | remote asks to approve itself | trust mutation absent from Channel tool surface; local-user-only admin rule | same-user local compromise bypasses | signed admin policy / managed config |
| Address SSRF-like abuse | malicious multiaddr makes dials to sensitive endpoints | address validation, allowed transport families, ConnectionManager policy, candidate limits | local/private network dialing may still be intended/ambiguous | deployment egress policy/deny ranges |
| Log leakage | payload/key/secret written to logs | payload logging false; secret redaction; structured classes | peer/channel identifiers can be sensitive | configurable pseudonymization, audit review |

## Security non-goals in v1

- proving application/person identity from PeerId;
- group end-to-end encryption;
- anonymous routing/metadata privacy;
- Byzantine consensus or membership;
- durable anti-replay across long offline periods;
- protection from malicious code running as the same OS user.
