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

/// The ceiling on events waiting to be drained.
///
/// Two per peer `seen` can hold — one observation, one expiry — which is
/// what a well-behaved LAN produces. It is a TOTAL bound rather than a
/// per-peer one because the identity space is not bounded: peers that
/// have lapsed out of `seen` are exactly the ones whose queued events
/// were unbounded before.
pub const MAX_PENDING_EVENTS: usize = 2 * MAX_PEERS;

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

        // A DEAD ENTRY IS NOT AN OCCUPIED ONE, and clearing one is a
        // RETRACTION rather than a deletion. `seen` holds records until
        // they are swept, so an announcement arriving after the TTL but
        // before the next drain met a map of peers that had already
        // lapsed and was refused — and for mDNS that refusal is final,
        // since nothing repeats a one-shot announcement.
        //
        // This calls `sweep` rather than pruning here. A second prune was
        // the obvious shape and was wrong: it deleted the record while
        // `sweep` also QUEUES the expiry, so a peer whose addresses lapse
        // at different times lost the retraction for the first one, and
        // any pending observation still naming it kept the manager
        // dialling a route this provider had silently forgotten. One
        // place decides what lapsing means.
        self.sweep(now_ms);

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
        // ONLY THE LATEST UNDRAINED HEALTH STATE. Alternating multicast
        // availability appended a transition per flap, so a backpressured
        // consumer accumulated a history nobody wants — health is a
        // CURRENT VALUE, and an older reading is not evidence a newer one
        // lacks. Coalescing here also keeps this path inside
        // `MAX_PENDING_EVENTS` rather than beside it.
        self.pending
            .retain(|event| !matches!(event, DiscoveryEvent::HealthChanged { .. }));
        self.pending.push(DiscoveryEvent::HealthChanged {
            source: SOURCE.to_owned(),
            health: self.health(),
        });
        self.enforce_pending_bound();
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
        // THE PENDING OBSERVATION'S LIFETIME IS RECOMPUTED, not left
        // behind. `expires_at` on a candidate is the peer's LATEST
        // deadline across the addresses it names, so removing an address
        // from a queued observation without recomputing can leave the
        // survivors carrying a deadline that belonged to the address that
        // just went — extending a route past its real expiry at the
        // manager. `seen` no longer holds the departed address by this
        // point, so it is the right thing to ask.
        let remaining_expiry = self
            .seen
            .get(peer_id)
            .and_then(|addresses| addresses.values().copied().max());

        // A goodbye contradicts a pending observation of the same address.
        let mut merged: BTreeSet<String> = [address.to_owned()].into_iter().collect();
        self.pending.retain_mut(|event| match event {
            DiscoveryEvent::CandidateObserved { candidate } if candidate.peer_id == *peer_id => {
                candidate.addresses.remove(address);
                candidate.expires_at = remaining_expiry;
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

        // BOUNDED BY WHAT `seen` CAN HOLD, not by history. Merging alone
        // was still unbounded across DISTINCT addresses: discover a,
        // expire a, discover b, expire b — each rediscovery withdraws only
        // the address that came back, so every address ever expired
        // accumulated in one event while `seen` never held more than one.
        // The set is the memory the finding was about, so the set is what
        // has to be capped.
        //
        // The oldest go, because a consumer that has not drained in that
        // long has already been told less recent news; dropping a
        // retraction is a bounded loss (the manager ages the address out
        // on its own TTL) where dropping the bound is not.
        while merged.len() > MAX_ADDRESSES_PER_PEER {
            let Some(oldest) = merged.iter().next().cloned() else {
                break;
            };
            merged.remove(&oldest);
        }

        self.pending.push(DiscoveryEvent::CandidateExpired {
            peer_id: peer_id.clone(),
            source: SOURCE.to_owned(),
            // Names the addresses, so a peer announcing several loses only
            // the ones that went.
            addresses: merged,
        });
        self.enforce_pending_bound();
    }

    /// Hold `pending` to `MAX_PENDING_EVENTS`, oldest first.
    ///
    /// Per-peer coalescing is not a bound, and that took three rounds to
    /// see: it caps what one IDENTITY can queue, while the identity space
    /// is free. A LAN sender announces 256 peers, lets them lapse — which
    /// empties `seen` — and repeats with fresh ones, so every bound keyed
    /// on `seen` holds while `pending` grows a batch per cycle.
    ///
    /// So the queue is bounded by the queue. Oldest first, because a
    /// consumer this far behind has already missed more recent news, and
    /// because the alternative — refusing new events — would let an
    /// attacker freeze the provider's view by filling it once.
    ///
    /// A dropped event costs a retraction the manager will make itself on
    /// its own TTL, or an observation the next announcement repeats.
    fn enforce_pending_bound(&mut self) {
        if self.pending.len() <= MAX_PENDING_EVENTS {
            return;
        }
        // HEALTH IS NEVER THE THING DROPPED. Coalescing leaves at most one
        // health event, and the manager learns health ONLY from it: seed
        // `Unavailable` at registration, updated by nothing else. So
        // trimming the sole transition leaves the provider reported
        // unavailable indefinitely — discovering peers while aggregate
        // health says it does none — and no later event fixes it unless
        // multicast happens to flap.
        //
        // A candidate event is recoverable: the next announcement repeats
        // it, or the manager ages the address out itself. That asymmetry
        // is the whole reason for the exception.
        let mut excess = self.pending.len() - MAX_PENDING_EVENTS;
        self.pending.retain(|event| {
            if excess > 0 && !matches!(event, DiscoveryEvent::HealthChanged { .. }) {
                excess -= 1;
                return false;
            }
            true
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
        self.enforce_pending_bound();
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
        // THROUGH THE COALESCING PATH, not straight onto the queue.
        // Pushing here bypassed every bound `queue_expiry` enforces, so an
        // unauthenticated LAN sender could announce a batch of distinct
        // fake peers, let them lapse, and repeat with fresh identities:
        // `seen` never exceeded MAX_PEERS because each batch aged out,
        // while `pending` grew a batch per cycle for a consumer that was
        // not draining.
        //
        // A TTL lapse and a goodbye are the same statement about the same
        // address, so they belong on the same path — the earlier fix
        // bounded one caller of it and left this one.
        for (peer_id, addresses) in lapsed {
            for address in &addresses {
                self.queue_expiry(&peer_id, address);
            }
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
    /// A synthetic well-formed PeerId string. The grammar is a prefix,
    /// an alphabet and a length with no checksum, so a generated id is as
    /// valid to this provider as a captured one — and nothing here dials.
    fn synthetic_peer(n: u64) -> String {
        const B58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        let mut tail = [b'1'; 44];
        let (mut v, mut i) = (n as usize, 43usize);
        loop {
            tail[i] = B58[v % B58.len()];
            v /= B58.len();
            if v == 0 || i == 0 {
                break;
            }
            i -= 1;
        }
        format!("12D3KooW{}", core::str::from_utf8(&tail).expect("ascii"))
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
    #[test]
    fn cycling_through_distinct_addresses_cannot_grow_a_pending_expiry() {
        // The finding's shape: discover a, expire a, discover b, expire b.
        // Each rediscovery withdraws only the address that came back, so
        // merging alone let every address ever expired accumulate in one
        // event while `seen` never held more than one.
        let mut p = started();
        let _ = p.drain_events(0, 64);

        let mut now = 0u64;
        for i in 0..500 {
            let addr = format!("/ip4/10.0.0.1/tcp/{i}");
            p.push_discovered(P1, &addr, now);
            p.push_expired(P1, &addr, now + 1);
            now += 1_000;
        }

        let queued = p.drain_events(now, usize::MAX);
        for event in &queued {
            if let DiscoveryEvent::CandidateExpired { addresses, .. } = event {
                assert!(
                    addresses.len() <= MAX_ADDRESSES_PER_PEER,
                    "a pending expiry names at most {MAX_ADDRESSES_PER_PEER} \
                     addresses, got {}",
                    addresses.len()
                );
            }
        }
    }
    #[test]
    fn rotating_fake_peers_cannot_grow_the_queue_through_the_sweep() {
        // The finding's shape: an unauthenticated LAN sender announces a
        // batch of distinct identities, lets them lapse, and repeats with
        // fresh ones. `seen` never exceeds MAX_PEERS because each batch
        // ages out — but the sweep pushed straight onto `pending`, so a
        // consumer that was not draining accumulated a batch per cycle.
        let mut p = started();
        let _ = p.drain_events(0, 64);

        let mut now = 0u64;
        for cycle in 0..40u64 {
            for i in 0..8u64 {
                p.push_discovered(
                    &synthetic_peer(cycle * 8 + i),
                    "/ip4/10.0.0.1/tcp/4001",
                    now,
                );
            }
            // Past the observation TTL: the whole batch lapses, and the
            // sweep runs on the next drain-driven tick.
            now += OBSERVATION_TTL_MS + 1;
            p.sweep(now);
        }

        let queued = p.drain_events(now, usize::MAX);
        assert!(
            queued.len() <= MAX_PENDING_EVENTS,
            "the queue stays within its total bound, got {}",
            queued.len()
        );
    }

    #[test]
    fn rotating_identities_forever_cannot_grow_the_queue() {
        // The earlier version of this test used 320 identities and
        // allowed 512 events, so it passed WITHOUT any total bound —
        // per-peer coalescing was enough at that size. The rotation has
        // to exceed the bound by enough that only a real cap can hold it.
        let mut p = started();
        let _ = p.drain_events(0, 64);

        let mut now = 0u64;
        let mut minted = 0u64;
        for _ in 0..50 {
            for _ in 0..MAX_PEERS {
                p.push_discovered(&synthetic_peer(minted), "/ip4/10.0.0.1/tcp/4001", now);
                minted += 1;
            }
            // The whole batch lapses, so `seen` empties and every bound
            // keyed on it is satisfied while the queue keeps growing.
            now += OBSERVATION_TTL_MS + 1;
            p.sweep(now);
        }
        assert!(
            minted > MAX_PENDING_EVENTS as u64 * 4,
            "the rotation must outrun the bound by a wide margin, minted {minted}"
        );

        let queued = p.drain_events(now, usize::MAX);
        assert!(
            queued.len() <= MAX_PENDING_EVENTS,
            "{minted} distinct identities over 50 cycles left {} events queued; \
             the cap is {MAX_PENDING_EVENTS}",
            queued.len()
        );
    }

    #[test]
    fn a_consumer_that_drains_normally_loses_nothing() {
        // The control: the cap must not be reachable in ordinary use, or
        // it is silently dropping events a well-behaved consumer needed.
        let mut p = started();
        let _ = p.drain_events(0, 64);

        for i in 0..MAX_PEERS as u64 {
            p.push_discovered(&synthetic_peer(i), "/ip4/10.0.0.1/tcp/4001", 0);
        }
        let drained = p.drain_events(0, usize::MAX);
        let observations = drained
            .iter()
            .filter(|e| matches!(e, DiscoveryEvent::CandidateObserved { .. }))
            .count();
        assert_eq!(
            observations, MAX_PEERS,
            "every peer a full `seen` can hold is still reported"
        );
    }
    #[test]
    fn flapping_multicast_queues_one_health_state_not_a_history() {
        // Health is a CURRENT VALUE. Appending a transition per flap let
        // a backpressured consumer accumulate a history nobody reads, and
        // did it outside the total bound the other paths respect.
        let mut p = started();
        let _ = p.drain_events(0, 64);

        for i in 0..1_000u64 {
            p.report_backend_down(i * 10);
            p.report_backend_up(i * 10 + 5);
        }

        let queued = p.drain_events(20_000, usize::MAX);
        let health: Vec<_> = queued
            .iter()
            .filter(|e| matches!(e, DiscoveryEvent::HealthChanged { .. }))
            .collect();
        assert_eq!(
            health.len(),
            1,
            "one pending health event, whatever the flap count: {}",
            health.len()
        );
        assert!(
            matches!(
                health[0],
                DiscoveryEvent::HealthChanged { health, .. } if *health == ProviderHealth::Healthy
            ),
            "and it is the LATEST state, not the first: {:?}",
            health[0]
        );
    }
    #[test]
    fn the_health_transition_survives_a_trimmed_queue() {
        // The manager seeds a provider `Unavailable` and updates it only
        // from `HealthChanged`. Trimming the sole transition therefore
        // leaves this provider reported unavailable indefinitely while it
        // is plainly discovering peers, and no later event repairs it
        // unless multicast happens to flap.
        let mut p = started();

        // Stall the consumer and overrun the queue: one batch expires
        // while a second is discovered.
        let mut now = 0u64;
        for cycle in 0..4u64 {
            for i in 0..MAX_PEERS as u64 {
                p.push_discovered(
                    &synthetic_peer(cycle * 1_000 + i),
                    "/ip4/10.0.0.1/tcp/1",
                    now,
                );
            }
            now += OBSERVATION_TTL_MS + 1;
            p.sweep(now);
        }

        let queued = p.drain_events(now, usize::MAX);
        assert!(
            queued.len() > MAX_PEERS,
            "the queue must actually have been trimmed, got {}",
            queued.len()
        );
        assert!(
            queued
                .iter()
                .any(|e| matches!(e, DiscoveryEvent::HealthChanged { .. })),
            "the health transition is still delivered"
        );
    }
    #[test]
    fn a_lapsed_peer_map_does_not_reject_a_new_announcement() {
        // `sweep` runs on drain, so an announcement arriving after the
        // TTL but before the next drain met a map full of peers that had
        // already lapsed. For mDNS the refusal is final: nothing repeats
        // the announcement until that peer announces again.
        let mut p = started();
        for i in 0..MAX_PEERS as u64 {
            p.push_discovered(&synthetic_peer(i), "/ip4/10.0.0.1/tcp/4001", 0);
        }
        let _ = p.drain_events(0, usize::MAX);

        // Past every one of those lifetimes, with no drain in between.
        let late = OBSERVATION_TTL_MS + 1;
        let newcomer = synthetic_peer(9_999);
        assert!(
            p.push_discovered(&newcomer, "/ip4/10.0.0.9/tcp/4001", late),
            "a lapsed map holds no live peer, so the newcomer is admitted"
        );
    }

    #[test]
    fn lapsed_addresses_do_not_reject_a_new_one_for_a_known_peer() {
        let mut p = started();
        let subject = synthetic_peer(1);
        for i in 0..MAX_ADDRESSES_PER_PEER {
            p.push_discovered(&subject, &format!("/ip4/192.168.1.{i}/tcp/4001"), 0);
        }
        let _ = p.drain_events(0, usize::MAX);

        let late = OBSERVATION_TTL_MS + 1;
        assert!(
            p.push_discovered(&subject, "/ip4/10.0.0.9/tcp/4001", late),
            "every held address has lapsed, so a live one is admitted"
        );
    }

    #[test]
    fn a_live_peer_map_still_refuses_a_newcomer() {
        // The control: pruning must free only LAPSED entries, or the caps
        // stop bounding what unauthenticated LAN traffic can hold here.
        let mut p = started();
        for i in 0..MAX_PEERS as u64 {
            p.push_discovered(&synthetic_peer(i), "/ip4/10.0.0.1/tcp/4001", 0);
        }
        assert!(
            !p.push_discovered(&synthetic_peer(9_999), "/ip4/10.0.0.9/tcp/4001", 1),
            "the map is full of LIVE peers, so the newcomer is refused"
        );
    }
    #[test]
    fn an_address_lapsing_before_its_peers_others_is_retracted_not_deleted() {
        // One address lapses while another is still live, and an
        // announcement arrives before the next drain. Pruning the lapsed
        // record silently deleted it: no retraction was queued, and a
        // pending observation naming it gave every included address the
        // peer's MAXIMUM expiry — so the manager kept, or newly received,
        // a route this provider had already forgotten, and no later sweep
        // could repair it because the record was gone.
        let mut p = started();
        let subject = synthetic_peer(1);
        let short = "/ip4/192.168.1.5/tcp/4001";
        let long = "/ip4/192.168.1.6/tcp/4001";

        p.push_discovered(&subject, short, 0);
        // The second address is announced later, so it outlives the first.
        let later = OBSERVATION_TTL_MS / 2;
        p.push_discovered(&subject, long, later);
        let _ = p.drain_events(later, usize::MAX);

        // Past the first address's lifetime, not the second's. An
        // announcement arrives, which is what reaches the capacity path.
        let after_short = OBSERVATION_TTL_MS + 1;
        p.push_discovered(&subject, long, after_short);

        let events = p.drain_events(after_short, usize::MAX);
        assert!(
            events.iter().any(|e| matches!(
                e,
                DiscoveryEvent::CandidateExpired { addresses, .. }
                    if addresses.contains(short)
            )),
            "the lapsed address is retracted by name: {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                DiscoveryEvent::CandidateObserved { candidate }
                    if candidate.addresses.contains(short)
            )),
            "and no observation still names it: {events:?}"
        );
    }
    #[test]
    fn removing_an_address_from_a_pending_observation_recomputes_its_expiry() {
        // `expires_at` is the peer's LATEST deadline across the addresses
        // a candidate names. Remove the longest-lived address without
        // recomputing and the survivors carry its deadline — a route kept
        // dialable at the manager past its real expiry.
        let mut p = started();
        let subject = synthetic_peer(1);
        let short = "/ip4/192.168.1.5/tcp/4001";
        let long = "/ip4/192.168.1.6/tcp/4001";

        p.push_discovered(&subject, short, 0);
        // Announced later, so it holds the peer's maximum deadline.
        let later = OBSERVATION_TTL_MS / 2;
        p.push_discovered(&subject, long, later);

        // The backend withdraws the LONGER-lived one while the
        // observation is still queued.
        p.push_expired(&subject, long, later);

        let events = p.drain_events(later, usize::MAX);
        let observation = events
            .iter()
            .find_map(|e| match e {
                DiscoveryEvent::CandidateObserved { candidate } => Some(candidate),
                _ => None,
            })
            .expect("the peer is still observed at its surviving address");
        assert_eq!(
            observation.addresses.len(),
            1,
            "only the surviving address is named"
        );
        assert_eq!(
            observation.expires_at,
            Some(OBSERVATION_TTL_MS),
            "and it carries ITS deadline, not the withdrawn address's"
        );
    }
}
