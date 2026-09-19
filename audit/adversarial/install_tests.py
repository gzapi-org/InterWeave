#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
"""Install test-only modules in a disposable checkout of AUDIT_BASE.

No production logic, dependencies, or contracts are changed. Negative tests
express independent contract expectations; a red test is a candidate finding,
not a reason to rewrite the contract. No test connects to an external service.
"""
from pathlib import Path
import subprocess

AUDIT_BASE = "8e853b908e5220893f615d5d67404788803b1d47"

DRIVER_TESTS = r'''

#[cfg(test)]
mod adversarial_campaign {
    use super::*;
    use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

    fn fixture() -> (libp2p::identity::Keypair, TransportIdentity, ConnectionManager, RelayState) {
        let key = libp2p::identity::Keypair::generate_ed25519();
        let peer = TransportIdentity::parse(key.public().to_peer_id().to_base58()).unwrap();
        let mut trust = ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(), 8,
        );
        let _ = trust.set_trust(
            interweave_transport_runtime::TrustSources::new(
                PeerTrustPolicy::new(std::iter::empty()).unwrap(),
                InfrastructureSet::new([peer.clone()]).unwrap(),
            ),
            &[],
        );
        let state = RelayState::new(&RelayClientSettings {
            use_authorized_identify_relays: true,
            ..RelayClientSettings::default()
        }).unwrap();
        (key, peer, trust, state)
    }

    fn info(key: &libp2p::identity::Keypair, addresses: Vec<Multiaddr>, hop: bool) -> identify::Info {
        // A nonempty protocol list without HOP is an explicit capability
        // withdrawal, not an omitted field in a partial Identify push.
        let mut protocols = vec![libp2p::StreamProtocol::new("/ipfs/id/1.0.0")];
        if hop {
            protocols.push(libp2p::StreamProtocol::new(HOP_PROTOCOL));
        }
        identify::Info {
            public_key: key.public(),
            protocol_version: "/audit/1".to_owned(),
            agent_version: "interweave-audit".to_owned(),
            listen_addrs: addresses,
            protocols,
            observed_addr: "/ip4/127.0.0.1/tcp/3000".parse().unwrap(),
            signed_peer_record: None,
        }
    }

    fn addresses_of(actions: Vec<Action>, peer: &TransportIdentity) -> Vec<String> {
        actions.into_iter().flat_map(|action| match action {
            Action::Reserve { relay, addresses } if relay == *peer => addresses,
            _ => Vec::new(),
        }).collect()
    }

    #[test]
    fn control_authorized_advertisement_becomes_a_reservation_candidate() {
        let (key, peer, trust, mut state) = fixture();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().unwrap();
        learn(&mut state, &key.public().to_peer_id(), &info(&key, vec![address.clone()], true), &trust);
        assert_eq!(addresses_of(state.manager.tick(0), &peer), vec![address.to_string()]);
    }

    #[test]
    fn control_unauthorized_advertisement_cannot_become_a_candidate() {
        let (key, _, _, mut state) = fixture();
        let nobody = ConnectionManager::new(interweave_transport_runtime::ConnectionPolicy::default(), 8);
        learn(&mut state, &key.public().to_peer_id(), &info(&key, vec!["/ip4/127.0.0.1/tcp/4001".parse().unwrap()], true), &nobody);
        assert!(state.manager.tick(0).is_empty());
    }

    #[test]
    fn fresh_identify_withdrawal_must_stop_new_reservation_requests() {
        // CONNECTIVITY.md section 7: eligibility requires configured or
        // freshly observed HOP support; fresh evidence supersedes cached.
        // Stay idle until after the withdrawal: no question about whether
        // an already active reservation should be torn down is involved.
        let (key, peer, trust, mut state) = fixture();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().unwrap();
        let id = key.public().to_peer_id();
        learn(&mut state, &id, &info(&key, vec![address.clone()], true), &trust);
        assert_eq!(state.manager.candidates(), 1, "positive precondition");
        learn(&mut state, &id, &info(&key, vec![address], false), &trust);
        let planned = addresses_of(state.manager.tick(1), &peer);
        assert!(planned.is_empty(), "a fresh explicit HOP withdrawal still planned a reservation at {planned:?}");
    }

    #[test]
    fn fresh_listen_address_must_survive_previous_address_capacity() {
        // One authorized relay changes all of its listener addresses.
        // We do not require retaining history, increasing the bound, or
        // choosing a particular eviction policy. At least the sole NEW
        // advertised address must become usable after eight old ones.
        let (key, peer, trust, mut state) = fixture();
        let old: Vec<Multiaddr> = (4100..4108)
            .map(|port| format!("/ip4/127.0.0.1/tcp/{port}").parse().unwrap()).collect();
        let fresh: Multiaddr = "/ip4/127.0.0.1/tcp/4900".parse().unwrap();
        let id = key.public().to_peer_id();
        learn(&mut state, &id, &info(&key, old, true), &trust);
        learn(&mut state, &id, &info(&key, vec![fresh.clone()], true), &trust);
        let planned = addresses_of(state.manager.tick(1), &peer);
        assert!(planned.contains(&fresh.to_string()), "the only current address was discarded; planned stale routes: {planned:?}");
    }

    #[test]
    fn control_configured_route_is_not_replaced_by_identify() {
        let (key, peer, trust, mut state) = fixture();
        let configured = "/ip4/127.0.0.1/tcp/5001";
        assert!(state.manager.add_static(peer.clone(), configured));
        learn(&mut state, &key.public().to_peer_id(), &info(&key, vec!["/ip4/127.0.0.1/tcp/5002".parse().unwrap()], true), &trust);
        assert_eq!(addresses_of(state.manager.tick(0), &peer), vec![configured.to_owned()]);
    }
}
'''

MANAGER_TESTS = r'''

#[cfg(test)]
mod adversarial_campaign {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn peer(i: usize) -> TransportIdentity {
        let names = [
            "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN",
            "12D3KooWHyNGMf9HTd3Zj6dStdkcc5ycsubW1rEgQSp6k6yfZBoy",
            "12D3KooWQYhTNQdmr3ArTeUHRYzFg94BKyTkoWBDWez9kSCVe2Xo",
        ];
        TransportIdentity::parse(names[i]).unwrap()
    }

    #[test]
    fn control_promotion_retains_only_operator_supplied_routes() {
        let mut manager = ReservationManager::new(ReservationConfig::default()).unwrap();
        let relay = peer(0);
        for i in 0..MAX_ADDRESSES_PER_RELAY {
            assert!(manager.learn(relay.clone(), &format!("/ip4/127.0.0.1/tcp/{}", 4100 + i)));
        }
        let configured = "/ip4/127.0.0.1/tcp/4900";
        assert!(manager.add_static(relay.clone(), configured));
        assert_eq!(manager.source(&relay), Some(RelaySource::Static));
        let actions = manager.tick(0);
        assert!(matches!(&actions[..], [Action::Reserve { addresses, .. }] if addresses == &[configured.to_owned()]));
    }

    #[test]
    fn generated_histories_preserve_bounds_and_advertisement_ownership() {
        // 512 deterministic histories, 128 operations each. The reference
        // ledger knows only successful acceptance, withdrawal and release;
        // it does not reuse production candidate selection or retry logic.
        for seed in 0..512_u64 {
            let mut random = seed + 1;
            let mut now = 0_u64;
            let mut manager = ReservationManager::new(ReservationConfig::default()).unwrap();
            let mut held: BTreeMap<TransportIdentity, BTreeSet<String>> = BTreeMap::new();
            for step in 0..128_u64 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let relay = peer(((random >> 16) % 3) as usize);
                let operation = (random >> 32) % 7;
                let route = format!("/ip4/127.0.0.1/tcp/{}", 4000 + step % 16);
                match operation {
                    0 => { let _ = manager.learn(relay.clone(), &route); }
                    1 => { let _ = manager.add_static(relay.clone(), &route); }
                    2 => {
                        if matches!(manager.state(&relay), Some(ReservationState::Requested { .. } | ReservationState::Active { .. })) {
                            let advertised = format!("/audit-reservation/{}/{}", relay.as_str(), step % 12);
                            if manager.record_accepted(&relay, &advertised, now).is_ok() {
                                held.entry(relay.clone()).or_default().insert(advertised);
                            }
                        }
                    }
                    3 => {
                        if matches!(manager.state(&relay), Some(ReservationState::Requested { .. } | ReservationState::Active { .. })) {
                            manager.record_failed(&relay, now, 0).unwrap();
                            held.remove(&relay);
                        }
                    }
                    4 => { let _ = manager.forget(&relay); held.remove(&relay); }
                    5 => {
                        let verdict = if random & 1 == 0 { DirectInboundState::VerifiedPublic } else { DirectInboundState::Unknown };
                        for action in manager.set_direct_inbound(verdict) {
                            if let Action::Release { relay, .. } = action { held.remove(&relay); }
                        }
                    }
                    _ => { now += 300_001; }
                }
                for action in manager.tick(now) {
                    if let Action::Release { relay, .. } = action { held.remove(&relay); }
                }
                let expected: BTreeSet<String> = held.values().flat_map(|set| set.iter().cloned()).collect();
                let actual: BTreeSet<String> = manager.advertised().into_iter().collect();
                assert_eq!(actual, expected, "seed {seed}, step {step}, operation {operation}");
                assert_eq!(manager.active(), held.len(), "seed {seed}, step {step}");
                assert!(manager.active() + manager.requested() <= manager.config().max_reservations as usize);
                assert!(actual.len() <= manager.active() * MAX_ADDRESSES_PER_RELAY);
            }
        }
    }
}
'''

def main():
    root = Path(__file__).resolve().parents[2]
    subprocess.run(["git", "cat-file", "-e", AUDIT_BASE + "^{commit}"], cwd=root, check=True)
    protected = ["crates", "third_party", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]
    subprocess.run(["git", "diff", "--exit-code", AUDIT_BASE, "--", *protected], cwd=root, check=True)
    modules = {
        "crates/transport/libp2p/src/runtime/relay_driver.rs": DRIVER_TESTS,
        "crates/transport/runtime/src/relay.rs": MANAGER_TESTS,
    }
    for name, tests in modules.items():
        path = root / name
        original = path.read_text()
        if "mod adversarial_campaign {" in original:
            raise RuntimeError("refusing duplicate test injection: " + name)
        path.write_text(original + tests)
    print("AUDIT_BASE=" + AUDIT_BASE)
    print("Installed test-only modules; production logic and lockfile are unchanged.")

if __name__ == "__main__":
    main()
