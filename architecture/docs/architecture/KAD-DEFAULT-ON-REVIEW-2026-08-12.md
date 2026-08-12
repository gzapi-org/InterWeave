# Kademlia default-on and recovery-policy amendment review — 2026-08-12

This amendment closes the final Phase-1 precision items and changes Kademlia rollout posture.

## Frozen precision changes

- Direct v2 `media_type_len=0` is the sole wire encoding of absent media type and maps to `media_present=0` in the content fingerprint.
- EndpointId leases require negotiated IPC keepalive by default; a client that omits required keepalive receives local `CapabilityDenied` before lease grant. Operators may explicitly relax this compatibility policy.
- `transportctl identity verify` is a no-write/no-network recovery drill that derives and compares the expected PeerId without entering restore.
- Complete transport-profile disaster recovery requires the recovery phrase plus a separate `config.yaml` backup. The phrase alone recovers a bare PeerId identity; caches, leases, messages, and application history are excluded.
- SPIKE-006 must identify the exact 32-byte Ed25519 seed accessor/import boundary and must never mnemonic-encode an opaque or expanded private-key protobuf representation.

## Kademlia rollout change

ADR-0034 supersedes only the previous default-disabled/optional rollout posture:

- the standard v1 daemon build includes Kademlia support;
- a configured Kademlia provider entry defaults to `enabled: true`;
- remote/composite/Kademlia shipped examples set `enabled: true`;
- `enabled: false` remains a supported explicit opt-out with zero Kademlia activity;
- profiles may omit the provider entirely when intentionally minimal;
- a reduced/custom build without Kademlia fails configuration/startup for an enabled/default-enabled entry;
- SPIKE-003 and Kademlia conformance/security tests are standard-v1 release gates.

No Kademlia value/provider records, EndpointIds, ChannelIds, trust state, or application payloads are introduced. Trust-gated routing and Swarm-wide dial admission remain unchanged.
