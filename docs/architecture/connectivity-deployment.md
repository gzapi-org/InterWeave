# Mandatory Internet reachability deployment architecture

## 1. Purpose

This document turns ADR-0035/0036 into deployment shapes. It does not create a centralized authority: relay/probe nodes are replaceable connectivity infrastructure and all application traffic still authenticates the end PeerId.

## 2. Standard client profile

```text
                         Internet
                            |
                +-----------+-----------+
                |                       |
          Infra node R1             Infra node R2
        AutoNAT + Relay           AutoNAT + Relay
                ^                       ^
                | reservations/probes  |
                +-----------+-----------+
                            |
                      NAT/private peer A
                            |
                     one profile PeerId
                     /              \
                human endpoint    Claude endpoint
```

A private/not-verified client targets two active reservations on distinct authorized relay PeerIds. A verified-public client keeps one warm reservation by default.

## 3. Infrastructure node role

An infrastructure node may host:

- ordinary libp2p listener/Noise/Yamux/Identify;
- AutoNAT-v2 server;
- Circuit Relay-v2 server;
- optional bootstrap/static-discovery address role;
- optionally its own AutoNAT/relay client capabilities because the standard build contains them.

These roles are configured and monitored independently even when colocated. Hosting bootstrap + relay + probe on one machine does not make bootstrap authoritative and does not count as independent redundancy.

## 4. Failure-domain recommendation

Where inbound-private availability matters, operate at least two authorized relay/probe services in independent failure domains. Prefer differences in machine/provider/network/power/administration where practical.

The architecture does not automatically infer independence from DNS names, IPs, ASNs, or operators. Operators declare/select service peers; diagnostics show which PeerIds currently satisfy reservation/probe roles.

## 5. Client authorization

Client profile:

```text
trust.allowed_peers
  = application peers allowed for GossipSub/direct/endpoint/Kademlia

transport.connectivity.infrastructure.allowed_peers
  = additional relay/probe PeerIds allowed for control-plane use only
```

Infrastructure service profile uses the same class mechanism for service clients unless those clients are already data-plane trusted. Static service configuration is invalid unless every referenced PeerId belongs to one of the two authorization sets.

## 6. Address publication

A profile publishes Internet-facing addresses from the runtime address registry:

```text
fresh AutoNAT-verified direct addresses
+
currently active relay-reservation addresses
```

It does not publish a merely guessed/observed public address as verified by default. Relay addresses disappear from the live advertisement set when their reservation ends.

Address publication is independent from EndpointId directory advertisement. Peer reachability answers “how to connect to PeerId”; Model B endpoint directory answers “which local direct route labels are currently advertised after connecting to that trusted PeerId.”

## 7. Path examples

### Directly reachable

```text
Peer A ======================= Peer B
          direct Noise path
```

### Relay fallback

```text
Peer A ===== Relay R1 ===== Peer B
        relayed end-peer session
```

### Relay then DCUtR

```text
A === R1 === B
|            |
+-- DCUtR ---+
      |
      v
A ========== B
 stable direct path; redundant relay path can retire
```

## 8. Network change

Laptop/mobile network changes are expected. On change:

1. invalidate affected direct verification evidence;
2. retain only still-valid connections/reservations;
3. run bounded AutoNAT re-evaluation;
4. reconcile relay target;
5. update advertised addresses;
6. re-evaluate direct-vs-relay paths;
7. preserve profile PeerId, trust config, EndpointId configuration and active local IPC leases unless the daemon itself restarts.

## 9. Outage behavior

| Condition | Result |
|---|---|
| one relay lost | other path remains; replacement attempted |
| all relays lost, peer verified-public | direct inbound may continue; relay state unavailable |
| all relays lost, peer private/not-verified | new inbound Internet reachability may be unavailable; existing direct/outbound sessions may continue |
| AutoNAT observers unavailable | direct state becomes unknown as evidence expires; relay target remains conservative |
| DCUtR always fails | relay remains normal long-lived fallback |
| infrastructure-only peer compromised | availability/metadata risk; application protocols remain denied by class policy |

## 10. Server capacity planning

Relay/probe service deployment must size:

- file descriptors and established connections;
- reservation count and expiry churn;
- concurrent circuits and forwarded bytes;
- AutoNAT dial-back concurrency/rate;
- CPU/memory for Noise/Yamux and routing control;
- egress bandwidth and abuse monitoring.

Architecture defaults are intentionally bounded, not claims of production capacity. SPIKE-004 supplies measured values for the target hardware/network and may tune defaults without changing the trust/path semantics.

## 11. Operational roll-out gate

Before declaring standard-v1 Internet-ready:

- run SPIKE-004 on the pinned rust-libp2p version;
- test at least public, home-NAT and restrictive-NAT/firewall classes available to the deployment team;
- kill one relay during active use and verify failover;
- remove all relays and verify honest degraded state;
- verify infrastructure-only peers cannot enter GossipSub/direct/endpoint/Kademlia;
- verify relay path still authenticates the intended end PeerId;
- verify successful and failed DCUtR do not change message/EndpointId semantics;
- record measured reservation/probe/circuit resource use.
