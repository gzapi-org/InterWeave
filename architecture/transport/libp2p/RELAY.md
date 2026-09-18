# Circuit Relay v2 design

Status: **Required standard-v1 libp2p backend component**

## 1. Selection and role

Use Circuit Relay **v2** client transport/reservation support for inbound reachability when direct inbound is not verified. Relay server capability is supported but explicit opt-in.

A relay forwards a libp2p connection. It is not a message broker, mailbox, discovery authority, trust authority, endpoint directory, or Kademlia authority.

## 2. Authorization

Relay candidates must be operator-authorized as either:

- `DataPlaneTrusted`; or
- `ConnectivityInfrastructureOnly`.

Identify/discovery can provide reachability/protocol evidence but cannot add authorization.

A relay PeerId's authorization never authorizes the application peer at the other end of a relayed connection. The remote application PeerId is Noise-authenticated and evaluated independently under normal data-plane trust.

**Nor does it authorize the relay as a circuit DESTINATION.** Authorizing a peer `ConnectivityInfrastructureOnly` buys reservation and circuit control *with* it; a circuit whose far end IS that peer uses it as an application destination and is refused, exactly as DCUtR toward it is (`DCUTR.md` §2). A relay may carry a circuit without becoming a party a circuit may terminate at. Who an exchange is with is a different question from who it is for (ADR-0036 Amendment 2026-09-03, and `CONNECTIVITY.md` §4's matrix).

## 3. Candidate sources

Initial sources:

1. statically configured relay multiaddrs with PeerId;
2. fresh Identify protocol observations from already authorized connected peers only when the operator explicitly sets `use_authorized_identify_relays=true`. This flag defaults **false**. Static configured relays have selection precedence; Identify-learned candidates are considered only when static candidates cannot satisfy the reservation target.

Kademlia provider/value records are not used for relay service advertisement in v1. Kademlia may help reach an already trusted application/router peer, but it does not become a relay directory.

## 4. Reservation state machine

Per candidate:

```text
idle -> dialing -> reserving -> active -> refreshing
  ^        |          |           |          |
  +--backoff/retry----+-----------+----------+
```

Global desired state:

- direct inbound `unknown`/`not_verified`: target 2 active reservations;
- `verified_public`: target 1 warm reservation;
- hard configurable client maximum: 4 by default.

A profile may use more candidates than target; only the bounded target is kept active.

Reservation acquisition/refresh failures use bounded backoff (default 5 s minimum, 5 min maximum). Do not hammer one failed relay.

**Note (2026-09-18, step 5).** The policy half of this section is `ReservationManager` (`crates/transport/runtime/src/relay.rs`), pure: it reads no clock and opens no socket. Its states are `Idle`, `Requested`, `Active` and `Backoff` — `dialing` and `reserving` above are one `Requested` to it, because the pinned client (`libp2p-relay` 0.21.1) obtains a reservation by LISTENING on `<relay>/p2p-circuit` and reports neither the control dial nor the request as a separate event; the adapter observes the listener's address (acceptance), its renewal, and its closing (loss). The ladder is per relay, `retry_min` doubling to `retry_max`, plus a jitter the adapter draws and the manager caps at the delay itself; the attempt count is carried through a re-ask, so a relay that fails its second ask continues at the second rung, and only an acceptance resets it. The target is capped by the maximum AND by the candidates that exist, and a shortfall with nothing left to ask is `Partial` (`CONNECTIVITY.md` §8), which the adapter reports rather than retries. When the target falls — a `VerifiedPublic` verdict — the surplus is released learned-first, newest-first by ACCEPTANCE time (a renewal does not re-age it), so the oldest configured relay stays warm. An acceptance from a relay never offered, or for one never asked, is refused by name and its address never advertised; so is a failure reported for an idle relay — the close of a listener the manager itself released, echoed back — which backs nothing off; so is an acceptance carrying no address. A reservation advertises every address the relay reported for it, up to eight — the pinned client reports the relay's own external addresses one listener event at a time, as many as the relay has, with no batch boundary and no expiry for any of them, and a renewal reports them again — so an acceptance adds an address and only the reservation's loss removes any. A relay's address list is bounded at eight — the operator's for a static relay, to which Identify adds nothing; the peer's own claims for a learned one — and a learned relay promoted to static counts against the static bound and keeps only the operator's address.

## 5. Relay-derived address lifecycle

An active reservation contributes a relay-derived listen address conceptually equivalent to:

```text
<relay-address>/p2p/<relay-peer>/p2p-circuit/p2p/<local-peer>
```

The exact multiaddr construction follows the pinned libp2p implementation.

Rules:

- add only after reservation acceptance;
- advertise only while reservation is active;
- remove immediately from current address registry when reservation closes/expires/fails;
- never persist the active reservation itself as durable truth;
- peer caches may temporarily retain stale observed relay addresses under normal TTL rules, so dial failures remain expected/recoverable.

## 6. Path selection

For a trusted application destination:

1. start/prefer direct dial candidates;
2. after `direct_head_start` (750 ms default), a usable relay path may race when direct has not established;
3. first policy-valid successful application peer connection may carry work;
4. if relayed, schedule bounded DCUtR when eligible;
5. direct success after DCUtR becomes preferred after the stability timer;
6. do not claim migration of already-open streams.

The relay path is transparent to DirectMessageV2 and EndpointId routing.

## 7. Redundancy and independence

Production deployment guidance should provide at least two independently operated/reachable authorized relay/probe services when a profile depends on inbound relay reachability. Two reservations on the same failure domain do not provide meaningful operational independence; configuration/diagnostics should make operator identity/domain visible without treating it as protocol trust.

This is an operational recommendation, not an automated trust inference.

## 8. Server role

When `relay.server.enabled=true`, enforce bounded resources. Defaults:

```text
max_reservations              64
max_reservations_per_peer      1
reservation_duration           1h
max_circuits                 128
max_circuits_per_source_peer   4
max_circuit_duration           1h
max_circuit_bytes             64 MiB
max_pending_control            64
```

Architecture ceilings are defined in config/resource-limits. Rate limiters should be used where supported by the pinned rust-libp2p API.

Standard project relay service admission is explicit: only peers classified `DataPlaneTrusted` or `ConnectivityInfrastructureOnly` may obtain reservations/circuits. Open anonymous relay service is not a standard-v1 deployment mode and would require a separate service-policy ADR plus stronger abuse controls. A project relay service does not grant clients application membership merely because it accepts a reservation/circuit.

## 9. Security/privacy

The end peers retain authenticated encrypted libp2p connectivity across the relay. The relay can still observe metadata such as participating PeerIds, timing, connection duration, and traffic volume and can deny/delay service.

Do not describe relay usage as anonymity.

Mitigations include redundant relays, quotas, service authorization, direct-path preference, bounded reservation retry and operational monitoring.

## 10. Failure semantics

- one reservation lost -> remain online through other active paths; replenish target;
- all reservations lost while private/not-verified -> relay inbound unavailable/degraded; no unauthorized fallback relay;
- relay circuit denied -> try another authorized route under backoff;
- relay disappears during application traffic -> connection failure follows normal transport semantics; no offline buffering/replay;
- server at capacity -> explicit operational rejection, not trust/policy mutation.

## 11. Observability

```text
relay_reservations_active
relay_reservation_target
relay_reservation_events_total{outcome,relay_class}
                                 outcome: accepted | renewed | lost | failed | released
                                          | refused_unknown_relay | refused_unrequested_acceptance
                                          | refused_unrequested_failure | refused_empty_address
                                 (the four refused_* are ReservationManager's
                                  RefusedRelayReport, counted so a refusal is never
                                  read as "recorded, no change" -- the SPIKE-004 shape)
relayed_peer_paths_active
relay_circuit_events_total{outcome}
relay_server_reservations_used
relay_server_circuits_used
relay_server_bytes_forwarded
```

Ordinary Claude status sees only normalized connectivity; raw relay topology belongs to admin diagnostics.
