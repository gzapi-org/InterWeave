// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! [`MdnsDiscovery`]: LAN candidates, normalized.
//!
//! # This crate owns no socket
//!
//! The multicast mechanism is a libp2p behaviour in
//! `crates/transport/libp2p`, which is the only place allowed to own a
//! Swarm; a provider that opened its own socket would be a provider
//! owning transport (`DISCOVERY.md`). What is here is the half that can
//! be tested by enumeration: raw `(peer, address)` strings arrive through
//! [`MdnsDiscovery::push_discovered`], and validated bounded candidates
//! come out.
//!
//! **That backend does not exist yet, and the reason is a dependency
//! advisory rather than an oversight.** Enabling libp2p's `mdns` feature
//! pulls `libp2p-mdns 0.48`, which pins `hickory-proto 0.25.x` and its
//! RUSTSEC-2026-0118 (a DNSSEC validation loop with no safe upgrade) and
//! RUSTSEC-2026-0119. `check_dependencies.sh` refuses that, and
//! `CLAUDE.md` §8 makes it a gate rather than a warning. This crate is
//! therefore complete and untested against real multicast: every rule
//! below is driven through `push_discovered`/`push_expired`, which is how
//! it was always going to be tested, and the socket arrives when the
//! upstream crate moves to `hickory-proto` 0.26.
//!
//! # The input is unauthenticated by construction
//!
//! Any host on the multicast domain can advertise anything, so mDNS
//! grants **zero trust** (`providers/mdns.md`) and every bound is applied
//! BEFORE an observation is emitted rather than after. A peer string that
//! is not a valid identity is dropped, not repaired: a malformed packet
//! must not panic the runtime and must not become a candidate either.
//!
//! # Degraded is the honest answer to a broken network
//!
//! Networks block multicast, containers lack multicast routing, and
//! interfaces change. Those make this provider degraded — they do not
//! kill the transport and do not disturb the other providers.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use interweave_discovery_api::{
    CandidatePeer, DiscoveryEvent, DiscoveryProvider, HintDisposition, MAX_ADDRESS_BYTES, PeerHint,
    ProviderDescriptor, ProviderError, ProviderHealth, ProviderMode, ProviderScope,
};
use interweave_transport_api::TransportIdentity;

/// The provider name, and the `source` on every candidate it emits.
pub const SOURCE: &str = "mdns";

/// The provider-interface version this implements.
const INTERFACE_VERSION: &str = "1.0";

/// Peers this provider will hold at once.
///
/// Lower than the manager's aggregate bound on purpose: this is the one
/// provider whose input any host on the LAN can produce, so it carries
/// the tighter cap `providers/mdns.md` asks for rather than leaning on
/// the manager to absorb a flood.
pub const MAX_PEERS: usize = 256;

/// Addresses held per peer.
pub const MAX_ADDRESSES_PER_PEER: usize = 8;

/// How often a re-announcement is forwarded to the consumer.
///
/// mDNS re-announces constantly and the manager learns lifetimes ONLY
/// from an observation event, so a refresh that stays inside this
/// provider lets the manager expire a peer that is still announcing.
/// Forwarding every announcement would be a flood; forwarding none was a
/// liveness bug. This is the middle: a refresh is re-emitted at most once
/// per window, which is well inside `OBSERVATION_TTL_MS` so the manager's
/// provenance never lapses while announcements continue.
pub const REFRESH_INTERVAL_MS: u64 = 30_000;

/// How long an observation stays live without being seen again.
///
/// mDNS records carry their own TTL and the backend reports expiry
/// explicitly; this is the backstop for a peer that simply stops being
/// announced while no expiry arrives — a laptop closed mid-announcement,
/// or a backend that lost multicast without noticing.
pub const OBSERVATION_TTL_MS: u64 = 120_000;

/// LAN observations, normalized into candidates.
#[derive(Debug, Default)]
pub struct MdnsDiscovery {
    /// peer -> address -> when the observation lapses.
    seen: BTreeMap<TransportIdentity, BTreeMap<String, u64>>,
    /// peer -> when this provider last emitted an observation for it, so
    /// a refresh reaches the manager without every announcement doing so.
    last_emitted: BTreeMap<TransportIdentity, u64>,
    started: bool,
    stopped: bool,
    /// Whether the backend currently has working multicast.
    backend_up: bool,
    pending: Vec<DiscoveryEvent>,
}

impl MdnsDiscovery {
    /// A provider with nothing observed yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seen: BTreeMap::new(),
            last_emitted: BTreeMap::new(),
            started: false,
            stopped: false,
            backend_up: true,
            pending: Vec::new(),
        }
    }

    /// Peers currently held.
    #[must_use]
    pub fn peer_count(&self) -> usize {
        self.seen.len()
    }

    /// Take a raw observation from the backend.
    ///
    /// Returns whether it was accepted. `false` for a peer string outside
    /// the identity grammar, an address outside its bounds, or an
    /// observation past a bound — all of which are ordinary traffic on a
    /// multicast domain and none of which is an error worth propagating.
    pub fn push_discovered(&mut self, peer: &str, address: &str, now_ms: u64) -> bool {
        if !self.started || self.stopped {
            return false;
        }
        // NORMALIZE FIRST. A peer string that is not an identity is
        // dropped rather than repaired, and nothing downstream ever sees
        // the raw bytes.
        let Ok(peer_id) = TransportIdentity::parse(peer.to_owned()) else {
            return false;
        };
        if address.is_empty() || address.len() > MAX_ADDRESS_BYTES {
            return false;
        }

        let known = self.seen.contains_key(&peer_id);
        if !known && self.seen.len() >= MAX_PEERS {
            return false;
        }
        let addresses = self.seen.entry(peer_id.clone()).or_default();
        if !addresses.contains_key(address) && addresses.len() >= MAX_ADDRESSES_PER_PEER {
            return false;
        }
        let expires_at = now_ms.saturating_add(OBSERVATION_TTL_MS);
        let refreshed = addresses.insert(address.to_owned(), expires_at).is_some();

        // A NEW ADDRESS IS ALWAYS FORWARDED; a re-announcement is
        // forwarded at most once per REFRESH_INTERVAL_MS.
        //
        // Emitting every announcement would make a quiet LAN look busy.
        // Emitting NONE was worse and is the bug this shape fixes: the
        // manager learns a lifetime only from an observation event, so a
        // refresh that stopped here let it expire a peer that was still
        // announcing — and this provider, still holding the record, would
        // emit neither an expiry nor a later observation to restore it.
        let due = self
            .last_emitted
            .get(&peer_id)
            .is_none_or(|last| now_ms.saturating_sub(*last) >= REFRESH_INTERVAL_MS);
        if !refreshed || due {
            self.queue_observation(&peer_id, now_ms);
        }
        true
    }

    /// Take an explicit expiry from the backend.
    ///
    /// Returns whether anything was held for that `(peer, address)`.
    pub fn push_expired(&mut self, peer: &str, address: &str, _now_ms: u64) -> bool {
        if !self.started || self.stopped {
            return false;
        }
        let Ok(peer_id) = TransportIdentity::parse(peer.to_owned()) else {
            return false;
        };
        let Some(addresses) = self.seen.get_mut(&peer_id) else {
            return false;
        };
        if addresses.remove(address).is_none() {
            return false;
        }
        let emptied = addresses.is_empty();
        if emptied {
            self.seen.remove(&peer_id);
            self.last_emitted.remove(&peer_id);
        }
        self.queue_expiry(&peer_id, address);
        true
    }

    /// The backend lost multicast: degraded, not dead.
    pub fn report_backend_down(&mut self, now_ms: u64) {
        if self.backend_up {
            self.backend_up = false;
            self.queue_health(now_ms);
        }
    }

    /// The backend has multicast again.
    pub fn report_backend_up(&mut self, now_ms: u64) {
        if !self.backend_up {
            self.backend_up = true;
            self.queue_health(now_ms);
        }
    }

    fn queue_health(&mut self, _now_ms: u64) {
        if !self.started || self.stopped {
            return;
        }
        self.pending.push(DiscoveryEvent::HealthChanged {
            source: SOURCE.to_owned(),
            health: self.health(),
        });
    }

    /// Queue an observation, replacing any pending one for the same peer.
    ///
    /// COALESCED, not appended. `seen` is bounded but `pending` was not:
    /// a consumer that stops draining while 256 peers announce adds a
    /// batch every refresh window forever, which lets unauthenticated LAN
    /// traffic grow memory without limit. An older pending observation for
    /// a peer is strictly superseded by a newer one — same peer, same or
    /// wider address set, later expiry — so replacing it loses nothing and
    /// bounds the queue by the peer count.
    /// Queue that `address` is gone for `peer_id`, coalescing into any
    /// expiry already pending for that peer.
    ///
    /// BOUNDED BY `seen`, which is the point. Appending a fresh event per
    /// goodbye let a LAN-driven discover/goodbye cycle grow `pending`
    /// without limit while a consumer was stalled — the observation half
    /// coalesced and the expiry half did not, which is unauthenticated
    /// input choosing how much memory this holds. At most one expiry per
    /// peer is queued, and its address set is bounded by
    /// `MAX_ADDRESSES_PER_PEER` because that is what `seen` admits.
    ///
    /// Merging loses nothing: two expiries for one peer say exactly what
    /// one expiry naming both addresses says.
    fn queue_expiry(&mut self, peer_id: &TransportIdentity, address: &str) {
        // A goodbye contradicts a pending observation of the same address.
        let mut merged: BTreeSet<String> = [address.to_owned()].into_iter().collect();
        self.pending.retain_mut(|event| match event {
            DiscoveryEvent::CandidateObserved { candidate } if candidate.peer_id == *peer_id => {
                candidate.addresses.remove(address);
                !candidate.addresses.is_empty()
            }
            DiscoveryEvent::CandidateExpired {
                peer_id: expired_peer,
                addresses,
                ..
            } if expired_peer == peer_id => {
                merged.append(addresses);
                false
            }
            _ => true,
        });
        self.pending.push(DiscoveryEvent::CandidateExpired {
            peer_id: peer_id.clone(),
            source: SOURCE.to_owned(),
            // Names the addresses, so a peer announcing several loses only
            // the ones that went.
            addresses: merged,
        });
    }

    fn queue_observation(&mut self, peer_id: &TransportIdentity, now_ms: u64) {
        let Some(addresses) = self.seen.get(peer_id) else {
            return;
        };
        let live: BTreeSet<String> = addresses
            .iter()
            .filter(|(_, exp)| now_ms < **exp)
            .map(|(a, _)| a.clone())
            .collect();
        if live.is_empty() {
            return;
        }
        let expires_at = addresses.values().copied().max();
        self.last_emitted.insert(peer_id.clone(), now_ms);
        // Drop any pending observation this one supersedes, and withdraw
        // from any pending expiry the addresses this observation
        // re-announces — a discover after a goodbye for the same address
        // means the goodbye is stale, and forwarding both would tell the
        // consumer the address is gone right after saying it is back.
        self.pending.retain_mut(|event| match event {
            DiscoveryEvent::CandidateObserved { candidate } => candidate.peer_id != *peer_id,
            DiscoveryEvent::CandidateExpired {
                peer_id: expired_peer,
                addresses,
                ..
            } if expired_peer == peer_id => {
                addresses.retain(|a| !live.contains(a));
                !addresses.is_empty()
            }
            _ => true,
        });
        self.pending.push(DiscoveryEvent::CandidateObserved {
            candidate: Box::new(CandidatePeer {
                peer_id: peer_id.clone(),
                addresses: live,
                source: SOURCE.to_owned(),
                observed_at: now_ms,
                expires_at,
                protocol_observations: BTreeSet::new(),
            }),
        });
    }

    /// Drop observations whose backstop TTL has passed, queueing an
    /// expiry for each.
    fn sweep(&mut self, now_ms: u64) {
        let mut lapsed: Vec<(TransportIdentity, BTreeSet<String>)> = Vec::new();
        for (peer_id, addresses) in &mut self.seen {
            let gone: BTreeSet<String> = addresses
                .iter()
                .filter(|(_, exp)| now_ms >= **exp)
                .map(|(a, _)| a.clone())
                .collect();
            if !gone.is_empty() {
                addresses.retain(|_, exp| now_ms < *exp);
                lapsed.push((peer_id.clone(), gone));
            }
        }
        self.seen.retain(|_, addresses| !addresses.is_empty());
        self.last_emitted.retain(|p, _| self.seen.contains_key(p));
        for (peer_id, addresses) in lapsed {
            self.pending.push(DiscoveryEvent::CandidateExpired {
                peer_id,
                source: SOURCE.to_owned(),
                addresses,
            });
        }
    }
}

impl DiscoveryProvider for MdnsDiscovery {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            name: SOURCE.to_owned(),
            interface_version: INTERFACE_VERSION.to_owned(),
            config_version: None,
            scope: ProviderScope::Local,
            // It listens; it does not query.
            mode: ProviderMode::Passive,
            // Records carry TTLs and the backend reports expiry.
            supports_expiry: true,
            supports_hints: false,
        }
    }

    fn start(&mut self, _now_ms: u64) -> Result<(), ProviderError> {
        if self.started {
            return Err(ProviderError::AlreadyStarted);
        }
        self.started = true;
        // The manager learns health only from an event; see the same
        // note in the other providers.
        self.pending.push(DiscoveryEvent::HealthChanged {
            source: SOURCE.to_owned(),
            health: if self.backend_up {
                ProviderHealth::Healthy
            } else {
                ProviderHealth::Degraded
            },
        });
        Ok(())
    }

    fn drain_events(&mut self, now_ms: u64, max: usize) -> Vec<DiscoveryEvent> {
        if !self.started || self.stopped {
            return Vec::new();
        }
        self.sweep(now_ms);
        let take = max.min(self.pending.len());
        self.pending.drain(..take).collect()
    }

    fn add_hint(&mut self, _hint: PeerHint, _now_ms: u64) -> HintDisposition {
        // Nothing to tell a multicast listener. Explicit, per the
        // contract: silence would be this provider pretending to own
        // something it cannot act on.
        HintDisposition::Unsupported
    }

    fn health(&self) -> ProviderHealth {
        if !self.started || self.stopped {
            return ProviderHealth::Unavailable;
        }
        if self.backend_up {
            ProviderHealth::Healthy
        } else {
            // DEGRADED, NOT UNAVAILABLE. A network that blocks multicast
            // is the normal condition in a container, and it must not make
            // the node look broken — the other providers are unaffected.
            ProviderHealth::Degraded
        }
    }

    fn shutdown(&mut self, _now_ms: u64) {
        self.stopped = true;
        self.pending.clear();
        self.seen.clear();
        self.last_emitted.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }
    fn started() -> MdnsDiscovery {
        let mut p = MdnsDiscovery::new();
        p.start(0).expect("starts");
        p
    }
    /// Events other than the initial health transition, which every
    /// provider now queues at start so the manager learns it.
    fn candidate_events(events: &[DiscoveryEvent]) -> Vec<&DiscoveryEvent> {
        events
            .iter()
            .filter(|e| !matches!(e, DiscoveryEvent::HealthChanged { .. }))
            .collect()
    }

    fn observations(events: &[DiscoveryEvent]) -> Vec<(TransportIdentity, BTreeSet<String>)> {
        events
            .iter()
            .filter_map(|e| match e {
                DiscoveryEvent::CandidateObserved { candidate } => {
                    Some((candidate.peer_id.clone(), candidate.addresses.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_observation_becomes_a_normalized_candidate() {
        let mut p = started();
        assert!(p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0));
        let seen = observations(&p.drain_events(0, 8));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, peer(P1));
        assert_eq!(
            seen[0].1,
            ["/ip4/192.168.1.5/tcp/4001".to_owned()]
                .into_iter()
                .collect::<BTreeSet<String>>()
        );
    }

    #[test]
    fn garbage_from_the_multicast_domain_is_dropped_not_repaired() {
        // Anyone on the LAN can send anything. A peer string outside the
        // identity grammar is not a candidate and not a panic.
        let mut p = started();
        assert!(!p.push_discovered("not-a-peer-id", "/ip4/192.168.1.5/tcp/4001", 0));
        assert!(!p.push_discovered("", "/ip4/192.168.1.5/tcp/4001", 0));
        assert!(!p.push_discovered(P1, "", 0));
        assert!(!p.push_discovered(P1, &"a".repeat(MAX_ADDRESS_BYTES + 1), 0));
        assert!(
            candidate_events(&p.drain_events(0, 8)).is_empty(),
            "no candidate came of any of it"
        );
        assert_eq!(p.peer_count(), 0);
        // ...and a good one still works, so the filter discriminates.
        assert!(p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0));
        assert_eq!(p.peer_count(), 1);
    }

    #[test]
    fn re_announcements_are_coalesced_but_not_swallowed() {
        // Both halves matter. Emitting every announcement floods the
        // manager; emitting none lets the manager expire a peer that is
        // still announcing, because a lifetime reaches it ONLY through an
        // observation event.
        let mut p = started();
        p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0);
        assert_eq!(observations(&p.drain_events(0, 8)).len(), 1);

        // Inside the window: coalesced.
        for t in 1..10 {
            p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", t);
        }
        assert!(
            p.drain_events(10, 8).is_empty(),
            "a burst of re-announcements is not a burst of events"
        );

        // Past the window: forwarded, so the manager's lifetime is
        // refreshed while the peer keeps announcing.
        p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", REFRESH_INTERVAL_MS);
        let seen = observations(&p.drain_events(REFRESH_INTERVAL_MS, 8));
        assert_eq!(seen.len(), 1, "the refresh reaches the consumer");
        assert_eq!(seen[0].0, peer(P1));
    }

    #[test]
    fn a_stalled_consumer_cannot_grow_the_queue_without_limit() {
        // `seen` was bounded and `pending` was not: a consumer that stops
        // draining while the LAN keeps announcing would otherwise let
        // unauthenticated traffic grow memory forever.
        let mut p = started();
        let mut t = 0u64;
        // Two peers announcing for an hour, drained never.
        while t <= 60 * 60 * 1_000 {
            p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", t);
            p.push_discovered(P2, "/ip4/192.168.1.6/tcp/4001", t);
            t += REFRESH_INTERVAL_MS;
        }
        let queued = p.drain_events(t, 4096);
        let observations_queued = queued
            .iter()
            .filter(|e| matches!(e, DiscoveryEvent::CandidateObserved { .. }))
            .count();
        assert!(
            observations_queued <= 2,
            "one pending observation per peer, not one per announcement: got {observations_queued}"
        );
    }

    #[test]
    fn the_coalesced_observation_is_the_newest_one() {
        // Replacing an older pending observation must not lose the newer
        // address set: a peer announcing a second address while the
        // consumer is stalled must still have both when it drains.
        let mut p = started();
        p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0);
        p.push_discovered(P1, "/ip6/::1/tcp/4001", 1);
        let seen = observations(&p.drain_events(1, 8));
        assert_eq!(seen.len(), 1, "coalesced into one");
        assert_eq!(seen[0].1.len(), 2, "carrying both addresses");
    }

    #[test]
    fn a_peer_that_keeps_announcing_never_lapses_at_the_consumer() {
        // The liveness property the coalescing must not break: announce
        // every 10s for well past OBSERVATION_TTL_MS and the consumer is
        // told often enough that its own lifetime never runs out.
        let mut p = started();
        let mut last_seen_at = 0u64;
        let mut t = 0u64;
        while t <= OBSERVATION_TTL_MS * 2 {
            p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", t);
            for event in p.drain_events(t, 8) {
                if let DiscoveryEvent::CandidateObserved { .. } = event {
                    last_seen_at = t;
                }
                if let DiscoveryEvent::CandidateExpired { .. } = event {
                    panic!("a peer that never stopped announcing was expired at {t}");
                }
            }
            assert!(
                t - last_seen_at < OBSERVATION_TTL_MS,
                "gap of {}ms at t={t} would outlive a consumer's lifetime",
                t - last_seen_at
            );
            t += 10_000;
        }
    }

    #[test]
    fn a_second_address_for_a_known_peer_is_observed() {
        let mut p = started();
        p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0);
        let _ = p.drain_events(0, 8);
        p.push_discovered(P1, "/ip6/::1/tcp/4001", 1);
        let seen = observations(&p.drain_events(1, 8));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1.len(), 2, "the candidate carries both addresses");
    }

    #[test]
    fn the_peer_bound_holds_against_a_flood() {
        let mut p = started();
        for i in 0..MAX_PEERS + 50 {
            let id = synthetic(i);
            p.push_discovered(&id, "/ip4/192.168.1.5/tcp/4001", 0);
        }
        assert_eq!(
            p.peer_count(),
            MAX_PEERS,
            "a LAN flood cannot grow this map past its bound"
        );
    }

    #[test]
    fn the_address_bound_holds_per_peer() {
        let mut p = started();
        for i in 0..MAX_ADDRESSES_PER_PEER + 10 {
            p.push_discovered(P1, &format!("/ip4/192.168.1.5/tcp/{i}"), 0);
        }
        let seen = observations(&p.drain_events(0, 64));
        let last = seen.last().expect("at least one observation");
        assert_eq!(last.1.len(), MAX_ADDRESSES_PER_PEER);
    }

    #[test]
    fn an_explicit_expiry_retracts_only_that_address() {
        let mut p = started();
        p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0);
        p.push_discovered(P1, "/ip6/::1/tcp/4001", 0);
        let _ = p.drain_events(0, 8);

        assert!(p.push_expired(P1, "/ip6/::1/tcp/4001", 1));
        let events = p.drain_events(1, 8);
        assert!(
            events.iter().any(|e| matches!(
                e,
                DiscoveryEvent::CandidateExpired { peer_id, addresses, .. }
                    if peer_id == &peer(P1)
                        && addresses.contains("/ip6/::1/tcp/4001")
                        && !addresses.contains("/ip4/192.168.1.5/tcp/4001")
            )),
            "the retraction names the address that went"
        );
        assert_eq!(p.peer_count(), 1, "the peer survives its other address");
    }

    #[test]
    fn an_expiry_for_something_unknown_is_ignored() {
        let mut p = started();
        assert!(!p.push_expired(P1, "/ip4/192.168.1.5/tcp/4001", 0));
        assert!(candidate_events(&p.drain_events(0, 8)).is_empty());
    }

    #[test]
    fn an_unrefreshed_observation_lapses_on_the_backstop_ttl() {
        // The backend may lose multicast without saying so; a peer that
        // simply stops being announced must not linger forever.
        let mut p = started();
        p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0);
        let _ = p.drain_events(0, 8);

        let events = p.drain_events(OBSERVATION_TTL_MS, 8);
        assert!(
            events.iter().any(|e| matches!(
                e,
                DiscoveryEvent::CandidateExpired { peer_id, .. } if peer_id == &peer(P1)
            )),
            "the stale observation lapsed"
        );
        assert_eq!(p.peer_count(), 0);
    }

    #[test]
    fn a_backend_that_loses_multicast_is_degraded_not_unavailable() {
        // The normal condition in a container. It must not make the node
        // look broken, and it must not disturb the other providers.
        let mut p = started();
        assert_eq!(p.health(), ProviderHealth::Healthy);
        p.report_backend_down(1);
        assert_eq!(p.health(), ProviderHealth::Degraded);
        assert!(
            p.drain_events(1, 8).iter().any(|e| matches!(
                e,
                DiscoveryEvent::HealthChanged {
                    health: ProviderHealth::Degraded,
                    ..
                }
            )),
            "and the transition is reported rather than only readable"
        );
        p.report_backend_up(2);
        assert_eq!(p.health(), ProviderHealth::Healthy);
    }

    #[test]
    fn a_repeated_backend_report_does_not_repeat_the_event() {
        let mut p = started();
        p.report_backend_down(1);
        let _ = p.drain_events(1, 8);
        p.report_backend_down(2);
        assert!(
            p.drain_events(2, 8).is_empty(),
            "health events mark transitions, not states"
        );
    }

    #[test]
    fn nothing_is_taken_before_start_or_after_shutdown() {
        let mut p = MdnsDiscovery::new();
        assert!(
            !p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0),
            "an observation before start is not held"
        );
        p.start(0).expect("starts");
        p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0);
        p.shutdown(1);
        p.shutdown(2);
        assert!(p.drain_events(3, 8).is_empty());
        assert!(!p.push_discovered(P2, "/ip4/192.168.1.6/tcp/4001", 3));
        assert_eq!(p.health(), ProviderHealth::Unavailable);
    }

    #[test]
    fn a_second_start_is_refused() {
        let mut p = started();
        assert_eq!(p.start(1), Err(ProviderError::AlreadyStarted));
    }

    #[test]
    fn hints_are_refused_explicitly() {
        let mut p = started();
        assert!(!p.descriptor().supports_hints);
        assert_eq!(
            p.add_hint(
                PeerHint::ObservedReachable {
                    peer_id: peer(P2),
                    address: "/ip4/192.168.1.6/tcp/4001".to_owned(),
                    observed_at: 0,
                },
                0
            ),
            HintDisposition::Unsupported,
            "there is nothing to tell a multicast listener"
        );
    }

    #[test]
    fn the_drain_respects_the_callers_bound() {
        let mut p = started();
        // Three events pending: the start health transition and two
        // observations.
        p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0);
        p.push_discovered(P2, "/ip4/192.168.1.6/tcp/4001", 0);
        assert_eq!(p.drain_events(0, 1).len(), 1, "the caller sizes the batch");
        assert_eq!(p.drain_events(0, 1).len(), 1);
        assert_eq!(p.drain_events(0, 8).len(), 1, "the rest stays queued");
        assert!(p.drain_events(0, 8).is_empty(), "and then it is empty");
    }

    /// A distinct valid PeerId per index, for the flood test.
    fn synthetic(i: usize) -> String {
        const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        let mut s = String::from("12D3KooW");
        let mut n = i;
        for _ in 0..44 {
            s.push(char::from(ALPHABET[n % ALPHABET.len()]));
            n /= ALPHABET.len();
        }
        s
    }
    #[test]
    fn a_discover_goodbye_cycle_cannot_grow_the_pending_queue() {
        // The finding's exact shape: a stalled consumer while the LAN
        // repeatedly announces and withdraws the same (peer, address).
        // The observation half coalesced already; the expiry half
        // appended, so unauthenticated multicast traffic chose how much
        // memory this provider held.
        let mut p = started();
        let _ = p.drain_events(0, 64); // clear the start-time health event

        let addr = "/ip4/10.0.0.1/tcp/4001";
        let mut now = 0u64;
        for _ in 0..500 {
            p.push_discovered(P1, addr, now);
            p.push_expired(P1, addr, now + 1);
            now += 1_000;
        }

        // Draining with a generous bound yields everything queued, which
        // is how the rest of this module measures the queue.
        let queued = p.drain_events(now, usize::MAX);
        assert!(
            queued.len() <= 2,
            "a peer holds at most one pending observation and one pending \
             expiry, got {} after 500 discover/goodbye cycles",
            queued.len()
        );
    }

    #[test]
    fn expiries_for_one_peer_merge_into_a_single_event() {
        let mut p = started();
        for port in 1..=4 {
            p.push_discovered(P1, &format!("/ip4/10.0.0.1/tcp/{port}"), 0);
        }
        let _ = p.drain_events(0, 64);

        for port in 1..=3 {
            p.push_expired(P1, &format!("/ip4/10.0.0.1/tcp/{port}"), 10);
        }
        let events = p.drain_events(10, 64);
        let expiries: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, DiscoveryEvent::CandidateExpired { .. }))
            .collect();

        assert_eq!(expiries.len(), 1, "three goodbyes, one coalesced expiry");
        match expiries[0] {
            DiscoveryEvent::CandidateExpired { addresses, .. } => assert_eq!(
                addresses.len(),
                3,
                "and it names every address that went — merging loses nothing"
            ),
            other => panic!("expected an expiry, got {other:?}"),
        }
    }

    #[test]
    fn a_rediscovery_withdraws_the_pending_goodbye_for_that_address() {
        // Order matters, not just count: forwarding a stale goodbye after
        // the address came back tells the consumer it is gone when it is
        // present.
        let mut p = started();
        let addr = "/ip4/10.0.0.1/tcp/4001";
        p.push_discovered(P1, addr, 0);
        let _ = p.drain_events(0, 64);

        p.push_expired(P1, addr, 10);
        p.push_discovered(P1, addr, 20);

        let events = p.drain_events(20, 64);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, DiscoveryEvent::CandidateExpired { .. })),
            "the goodbye was superseded by the rediscovery, so it is not \
             forwarded: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, DiscoveryEvent::CandidateObserved { .. })),
            "and the peer is observed as present"
        );
    }

    #[test]
    fn a_goodbye_that_is_not_contradicted_still_reaches_the_consumer() {
        // The positive control: coalescing must not swallow a real expiry.
        let mut p = started();
        p.push_discovered(P1, "/ip4/10.0.0.1/tcp/1", 0);
        p.push_discovered(P1, "/ip4/10.0.0.2/tcp/2", 0);
        let _ = p.drain_events(0, 64);

        p.push_expired(P1, "/ip4/10.0.0.1/tcp/1", 10);
        let events = p.drain_events(10, 64);
        assert!(
            events.iter().any(|e| matches!(
                e,
                DiscoveryEvent::CandidateExpired { addresses, .. }
                    if addresses.contains("/ip4/10.0.0.1/tcp/1")
            )),
            "the address that actually went is reported: {events:?}"
        );
    }
}
