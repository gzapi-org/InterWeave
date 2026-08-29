// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! [`StaticBootstrapDiscovery`]: operator-configured reachability entries.
//!
//! The simplest provider, and the one whose semantics are most easily
//! over-read. A configured entry is a candidate ADDRESS with configured
//! provenance — it is not an identity authority, a trust root, a
//! membership server, a coordinator, a broker, or permanent infrastructure
//! (ADR-0010). Configuration does not grant trust: a configured PeerId
//! still needs an explicit trust rule before ConnectionManager will hold
//! an ordinary data-plane connection to it, and this crate cannot reach a
//! trust decision at all.
//!
//! # Addresses are not resolved here
//!
//! `/dns4/bootstrap.example.net/tcp/4001` is validated and emitted as
//! written. Resolution happens when the dial path consumes it, which is
//! what keeps a DNS outage a dial diagnostic rather than a provider health
//! failure — this provider's health covers configuration parsing and its
//! own lifecycle, nothing it cannot see.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use interweave_discovery_api::{
    CandidatePeer, DiscoveryError, DiscoveryEvent, DiscoveryProvider, HintDisposition,
    MAX_ADDRESS_BYTES, PeerHint, ProviderDescriptor, ProviderError, ProviderHealth, ProviderMode,
    ProviderScope,
};
use interweave_transport_api::TransportIdentity;

/// The provider name, and the `source` on every candidate it emits.
pub const SOURCE: &str = "static-bootstrap";

/// The provider-interface version this implements.
const INTERFACE_VERSION: &str = "1.0";

/// Configured entries permitted (`providers/static-bootstrap.md`).
pub const MAX_ENTRIES: usize = 64;

/// The ceiling on events waiting to be drained.
///
/// Two per configured entry — one observation, one retraction — which is
/// the most a single reload can produce for one peer.
pub const MAX_PENDING_EVENTS: usize = 2 * MAX_ENTRIES;

/// One configured entry: a peer and one address at which to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticEntry {
    /// The peer.
    pub peer_id: TransportIdentity,
    /// An opaque address, validated for length and emitted unresolved.
    pub address: String,
}

impl StaticEntry {
    /// Build an entry, checking what this layer can check.
    ///
    /// The address is bounded and non-empty. It is NOT parsed as a
    /// multiaddr: the neutral contract keeps addresses opaque, and a
    /// backend grammar here would put libp2p's syntax into a crate that
    /// must not know about it.
    ///
    /// # Errors
    /// [`DiscoveryError::InvalidLength`] for an empty or over-long address.
    pub fn new(
        peer_id: TransportIdentity,
        address: impl Into<String>,
    ) -> Result<Self, DiscoveryError> {
        let address = address.into();
        if address.is_empty() || address.len() > MAX_ADDRESS_BYTES {
            return Err(DiscoveryError::InvalidLength {
                field: "address",
                got: address.len(),
                max: MAX_ADDRESS_BYTES,
            });
        }
        Ok(Self { peer_id, address })
    }
}

/// A queued event, with the bookkeeping needed to undo it.
///
/// `before` is what `emitted` held for this peer immediately BEFORE the
/// event was queued. An event says what CHANGED, and a whole-peer
/// retraction deliberately says nothing about the addresses it
/// withdraws — so a rollback reconstructing state from the event, or
/// from the configuration as it stands now, cannot recover what the
/// consumer was actually holding.
#[derive(Debug, Clone)]
struct Queued {
    event: DiscoveryEvent,
    before: Option<BTreeSet<String>>,
}

/// Configured bootstrap entries, presented as a discovery provider.
#[derive(Debug, Default)]
pub struct StaticBootstrapDiscovery {
    /// peer -> the addresses configured for it.
    entries: BTreeMap<TransportIdentity, BTreeSet<String>>,
    /// What the consumer has been told, so the outstanding difference is
    /// always derivable from state rather than accumulated from events.
    emitted: BTreeMap<TransportIdentity, BTreeSet<String>>,
    started: bool,
    stopped: bool,
    pending: Vec<Queued>,
}

impl StaticBootstrapDiscovery {
    /// Build from configured entries.
    ///
    /// # Errors
    /// [`DiscoveryError::TooManyItems`] past [`MAX_ENTRIES`]. Entries are
    /// counted as CONFIGURED, before grouping by peer: an operator who
    /// lists sixty-five lines has exceeded the bound whether or not some
    /// share a PeerId, and counting the grouped result would let a long
    /// list past by collapsing it.
    pub fn new(entries: Vec<StaticEntry>) -> Result<Self, DiscoveryError> {
        if entries.len() > MAX_ENTRIES {
            return Err(DiscoveryError::TooManyItems {
                field: "entries",
                got: entries.len(),
                max: MAX_ENTRIES,
            });
        }
        let mut grouped: BTreeMap<TransportIdentity, BTreeSet<String>> = BTreeMap::new();
        for entry in entries {
            grouped
                .entry(entry.peer_id)
                .or_default()
                .insert(entry.address);
        }
        Ok(Self {
            entries: grouped,
            emitted: BTreeMap::new(),
            started: false,
            stopped: false,
            pending: Vec::new(),
        })
    }

    /// Entries configured, counted by peer.
    #[must_use]
    pub fn peer_count(&self) -> usize {
        self.entries.len()
    }

    /// Replace the configuration, queueing the difference.
    ///
    /// A peer that is gone is retracted; one that is new or whose address
    /// set changed is re-observed. This is the config-reload path, and
    /// emitting the DIFFERENCE rather than everything is what stops a
    /// reload from looking like a burst of fresh discoveries.
    ///
    /// # Errors
    /// As [`Self::new`].
    pub fn set_entries(
        &mut self,
        entries: Vec<StaticEntry>,
        now_ms: u64,
    ) -> Result<(), DiscoveryError> {
        let replacement = Self::new(entries)?.entries;

        self.entries = replacement;
        if self.started && !self.stopped {
            self.refresh(now_ms);
        }
        Ok(())
    }

    /// Queue the difference between what the consumer was told and what
    /// is configured now.
    ///
    /// DERIVED FROM STATE, NOT ACCUMULATED FROM EVENTS. Emitting the diff
    /// eagerly in `set_entries` compared only the previous configuration
    /// to the next one, so an event the queue bound later discarded could
    /// never be recreated — a subsequent reload had nothing to compare
    /// against that would produce it again. A dropped RETRACTION was
    /// unrecoverable and permanent: the manager holds static provenance
    /// without expiry, so the removed address stayed a candidate for
    /// good.
    ///
    /// Computing against `emitted` instead makes every queued event
    /// reproducible from state, which is what lets the bound discard one
    /// safely. It is the same model `PeerCacheDiscovery` uses, for the
    /// same reason.
    fn refresh(&mut self, now_ms: u64) {
        let mut queued: Vec<Queued> = Vec::new();

        for (peer_id, addresses) in &self.emitted {
            match self.entries.get(peer_id) {
                None => queued.push(Queued {
                    event: DiscoveryEvent::CandidateExpired {
                        peer_id: peer_id.clone(),
                        source: SOURCE.to_owned(),
                        addresses: BTreeSet::new(),
                    },
                    before: Some(addresses.clone()),
                }),
                Some(kept) => {
                    let dropped: BTreeSet<String> = addresses.difference(kept).cloned().collect();
                    if !dropped.is_empty() {
                        queued.push(Queued {
                            event: DiscoveryEvent::CandidateExpired {
                                peer_id: peer_id.clone(),
                                source: SOURCE.to_owned(),
                                addresses: dropped,
                            },
                            before: Some(addresses.clone()),
                        });
                    }
                }
            }
        }
        for (peer_id, addresses) in &self.entries {
            if self.emitted.get(peer_id) != Some(addresses) {
                queued.push(Queued {
                    event: observed(peer_id, addresses, now_ms),
                    before: self.emitted.get(peer_id).cloned(),
                });
            }
        }

        // APPENDED, and `emitted` advanced to match. Wiping what was
        // already queued would discard events whose effect `emitted` has
        // already absorbed — a reload that changes nothing then computes
        // an empty difference and the earlier retraction is simply gone.
        // Growth from repeated reloads is the bound's job, and the bound
        // can do it safely precisely because every event it drops is
        // rolled back out of `emitted` and recomputed here.
        for queued in &queued {
            match &queued.event {
                DiscoveryEvent::CandidateObserved { candidate } => {
                    self.emitted.insert(
                        candidate.peer_id.clone(),
                        candidate.addresses.iter().cloned().collect(),
                    );
                }
                DiscoveryEvent::CandidateExpired {
                    peer_id, addresses, ..
                } => {
                    if addresses.is_empty() {
                        self.emitted.remove(peer_id);
                    } else if let Some(held) = self.emitted.get_mut(peer_id) {
                        held.retain(|a| !addresses.contains(a));
                    }
                }
                DiscoveryEvent::HealthChanged { .. } => {}
            }
        }
        self.pending.extend(queued);
        self.enforce_pending_bound();
    }

    /// Hold `pending` to [`MAX_PENDING_EVENTS`], oldest first, rolling
    /// back the bookkeeping behind each dropped event.
    ///
    /// The rollback is what makes the drop safe: `refresh` recomputes the
    /// outstanding difference from `emitted`, so undoing the record an
    /// event was going to establish means the next drain queues it again.
    fn enforce_pending_bound(&mut self) {
        if self.pending.len() <= MAX_PENDING_EVENTS {
            return;
        }
        // Health is never the thing dropped: the manager learns health
        // only from that event, and nothing recomputes a transition that
        // already happened.
        let mut excess = self.pending.len() - MAX_PENDING_EVENTS;
        let mut kept: Vec<Queued> = Vec::with_capacity(MAX_PENDING_EVENTS);
        let mut dropped: Vec<Queued> = Vec::new();
        for queued in self.pending.drain(..) {
            if excess > 0 && !matches!(queued.event, DiscoveryEvent::HealthChanged { .. }) {
                excess -= 1;
                dropped.push(queued);
            } else {
                kept.push(queued);
            }
        }
        self.pending = kept;

        // UNDONE IN REVERSE, restoring each event's own `before`. Reverse
        // order because a peer may have several dropped events and the
        // EARLIEST one's snapshot is the state to end at. Restoring a
        // recorded snapshot rather than deriving one is what makes a
        // whole-peer retraction recoverable: it carries no addresses, and
        // the configuration may have been reloaded since.
        for queued in dropped.into_iter().rev() {
            let peer_id = match &queued.event {
                DiscoveryEvent::CandidateObserved { candidate } => candidate.peer_id.clone(),
                DiscoveryEvent::CandidateExpired { peer_id, .. } => peer_id.clone(),
                DiscoveryEvent::HealthChanged { .. } => continue,
            };
            match queued.before {
                Some(addresses) => {
                    self.emitted.insert(peer_id, addresses);
                }
                None => {
                    self.emitted.remove(&peer_id);
                }
            }
        }
    }
}

/// One configured peer as a candidate observation.
fn observed(
    peer_id: &TransportIdentity,
    addresses: &BTreeSet<String>,
    now_ms: u64,
) -> DiscoveryEvent {
    DiscoveryEvent::CandidateObserved {
        candidate: Box::new(CandidatePeer {
            peer_id: peer_id.clone(),
            addresses: addresses.clone(),
            source: SOURCE.to_owned(),
            observed_at: now_ms,
            // NO EXPIRY. A configured entry is true until the operator
            // says otherwise, and `supports_expiry: false` on the
            // descriptor says the same thing. The manager still applies
            // its own bound, which is what keeps "no expiry" from meaning
            // "forever".
            expires_at: None,
            protocol_observations: BTreeSet::new(),
        }),
    }
}

impl DiscoveryProvider for StaticBootstrapDiscovery {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            name: SOURCE.to_owned(),
            interface_version: INTERFACE_VERSION.to_owned(),
            config_version: None,
            scope: ProviderScope::Configured,
            // It never queries: it reports what an operator wrote.
            mode: ProviderMode::Passive,
            supports_expiry: false,
            supports_hints: false,
        }
    }

    fn start(&mut self, now_ms: u64) -> Result<(), ProviderError> {
        if self.started {
            return Err(ProviderError::AlreadyStarted);
        }
        self.started = true;
        // THE INITIAL TRANSITION IS AN EVENT. The manager registers a
        // provider as Unavailable and learns health only from
        // `HealthChanged`, so a provider that merely becomes healthy
        // internally leaves aggregate health wrong forever.
        self.pending.push(Queued {
            event: DiscoveryEvent::HealthChanged {
                source: SOURCE.to_owned(),
                health: ProviderHealth::Healthy,
            },
            before: None,
        });
        // The configured entries, as the difference from having told
        // the consumer nothing.
        self.refresh(now_ms);
        Ok(())
    }

    fn drain_events(&mut self, now_ms: u64, max: usize) -> Vec<DiscoveryEvent> {
        if !self.started || self.stopped {
            return Vec::new();
        }
        // RECOMPUTE BEFORE HANDING ANYTHING OVER. The queue bound rolls a
        // dropped event out of `emitted`, which only says the difference
        // is outstanding again — something has to look. Recomputing only
        // in `set_entries` meant a consumer that resumed WITHOUT a further
        // reload never saw it, so the last trim of a churn burst left a
        // retraction missing indefinitely. That is permanent here: the
        // manager holds static provenance without expiry.
        //
        // Cheap and idempotent: with nothing rolled back the difference is
        // empty and this queues nothing.
        self.refresh(now_ms);
        let take = max.min(self.pending.len());
        self.pending.drain(..take).map(|q| q.event).collect()
    }

    fn add_hint(&mut self, _hint: PeerHint, _now_ms: u64) -> HintDisposition {
        // `supports_hints: false`, and the refusal is explicit for every
        // class: a provider that quietly accepted would be taking
        // ownership of something the operator's file is supposed to own.
        HintDisposition::Unsupported
    }

    fn health(&self) -> ProviderHealth {
        if !self.started || self.stopped {
            return ProviderHealth::Unavailable;
        }
        // CONFIGURATION PARSED IS ALL THIS PROVIDER CAN SEE. It never
        // resolves a name or opens a socket, so there is no failure mode
        // for it to report — a DNS outage or an unreachable bootstrap host
        // is a dial diagnostic, and claiming otherwise would be claiming
        // visibility it does not have.
        ProviderHealth::Healthy
    }

    fn shutdown(&mut self, _now_ms: u64) {
        self.stopped = true;
        self.pending.clear();
        self.emitted.clear();
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
    /// An entry for an already-parsed identity.
    fn entry_id(id: &TransportIdentity, address: &str) -> StaticEntry {
        StaticEntry::new(id.clone(), address.to_owned()).expect("legal entry")
    }

    /// A synthetic well-formed identity: the grammar is a prefix, an
    /// alphabet and a length with no checksum, and nothing here dials.
    fn synthetic(n: usize) -> TransportIdentity {
        const B58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        let mut tail = [b'1'; 44];
        let (mut v, mut i) = (n, 43usize);
        loop {
            tail[i] = B58[v % B58.len()];
            v /= B58.len();
            if v == 0 || i == 0 {
                break;
            }
            i -= 1;
        }
        TransportIdentity::parse(format!(
            "12D3KooW{}",
            core::str::from_utf8(&tail).expect("ascii")
        ))
        .expect("matches the grammar")
    }

    fn entry(p: &str, address: &str) -> StaticEntry {
        StaticEntry::new(peer(p), address).expect("within bounds")
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
    fn configured_entries_are_emitted_on_start() {
        let mut p = StaticBootstrapDiscovery::new(vec![
            entry(P1, "/ip4/10.0.0.1/tcp/4001"),
            entry(P2, "/ip4/10.0.0.2/tcp/4001"),
        ])
        .expect("within bounds");

        assert!(p.drain_events(0, 8).is_empty(), "nothing before start");
        p.start(1_000).expect("starts");
        let seen = observations(&p.drain_events(1_000, 8));
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().any(|(id, _)| id == &peer(P1)));
    }

    #[test]
    fn a_dns_address_is_emitted_unresolved() {
        // The whole point of the DNS-ownership rule: this name does not
        // resolve anywhere, and the provider neither notices nor cares.
        // Resolution is the dial path's job, which keeps a DNS outage a
        // dial diagnostic rather than a provider health failure.
        let name = "/dns4/bootstrap.invalid.example/tcp/4001";
        let mut p = StaticBootstrapDiscovery::new(vec![entry(P1, name)]).expect("within bounds");
        p.start(0).expect("starts");

        let seen = observations(&p.drain_events(0, 8));
        assert_eq!(
            seen[0].1,
            [name.to_owned()].into_iter().collect::<BTreeSet<String>>(),
            "emitted exactly as configured, not as an IP"
        );
        assert_eq!(
            p.health(),
            ProviderHealth::Healthy,
            "an unresolvable name is not this provider's failure"
        );
    }

    #[test]
    fn several_addresses_for_one_peer_become_one_candidate() {
        let mut p = StaticBootstrapDiscovery::new(vec![
            entry(P1, "/ip4/10.0.0.1/tcp/4001"),
            entry(P1, "/dns4/host.example/tcp/4001"),
        ])
        .expect("within bounds");
        assert_eq!(p.peer_count(), 1);
        p.start(0).expect("starts");
        let seen = observations(&p.drain_events(0, 8));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1.len(), 2);
    }

    #[test]
    fn a_reload_emits_the_difference_not_everything() {
        // THREE peers, so "the difference" and "everything" are different
        // answers: P1 changes, P2 is removed, P3 is untouched. A reload
        // that re-emitted everything would also re-observe P3, and the
        // assertion below is what catches that.
        const P3: &str = "12D3KooWQYV9dGMFoRzNStwpXztXaBUjtPqi6aU76ZgUriHhKust";
        let mut p = StaticBootstrapDiscovery::new(vec![
            entry(P1, "/ip4/10.0.0.1/tcp/4001"),
            entry(P2, "/ip4/10.0.0.2/tcp/4001"),
            entry(P3, "/ip4/10.0.0.3/tcp/4001"),
        ])
        .expect("within bounds");
        p.start(0).expect("starts");
        let _ = p.drain_events(0, 8);

        // P2 goes, P1 gains an address, P3 is untouched.
        p.set_entries(
            vec![
                entry(P1, "/ip4/10.0.0.1/tcp/4001"),
                entry(P1, "/ip4/10.0.0.9/tcp/4001"),
                entry(P3, "/ip4/10.0.0.3/tcp/4001"),
            ],
            10,
        )
        .expect("within bounds");

        let events = p.drain_events(10, 8);
        assert!(
            events.iter().any(|e| matches!(
                e,
                DiscoveryEvent::CandidateExpired { peer_id, addresses, .. }
                    if peer_id == &peer(P2) && addresses.is_empty()
            )),
            "the removed peer is retracted whole"
        );
        let seen = observations(&events);
        assert_eq!(
            seen.len(),
            1,
            "only the CHANGED peer is re-observed; P3 was untouched"
        );
        assert_eq!(seen[0].0, peer(P1));
        assert_eq!(seen[0].1.len(), 2);
    }

    #[test]
    fn a_reload_that_drops_one_address_retracts_only_that_address() {
        let mut p = StaticBootstrapDiscovery::new(vec![
            entry(P1, "/ip4/10.0.0.1/tcp/4001"),
            entry(P1, "/ip4/10.0.0.9/tcp/4001"),
        ])
        .expect("within bounds");
        p.start(0).expect("starts");
        let _ = p.drain_events(0, 8);

        p.set_entries(vec![entry(P1, "/ip4/10.0.0.1/tcp/4001")], 10)
            .expect("within bounds");
        let events = p.drain_events(10, 8);
        assert!(
            events.iter().any(|e| matches!(
                e,
                DiscoveryEvent::CandidateExpired { addresses, .. }
                    if addresses.contains("/ip4/10.0.0.9/tcp/4001")
                        && !addresses.contains("/ip4/10.0.0.1/tcp/4001")
            )),
            "the retraction names the dropped address only"
        );
    }

    #[test]
    fn a_reload_before_start_queues_nothing() {
        // No events before start, and a reload is not a back door around
        // that rule.
        let mut p = StaticBootstrapDiscovery::new(vec![entry(P1, "/ip4/10.0.0.1/tcp/1")])
            .expect("within bounds");
        p.set_entries(vec![entry(P2, "/ip4/10.0.0.2/tcp/1")], 5)
            .expect("within bounds");
        assert!(p.drain_events(5, 8).is_empty());
        // ...and starting now emits the CURRENT configuration.
        p.start(6).expect("starts");
        let seen = observations(&p.drain_events(6, 8));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, peer(P2));
    }

    #[test]
    fn the_entry_count_is_bounded_before_grouping() {
        // Sixty-five configured lines is over the bound even though they
        // name one peer — counting the grouped result would let any list
        // past by collapsing it.
        let too_many: Vec<StaticEntry> = (0..=MAX_ENTRIES)
            .map(|i| entry(P1, &format!("/ip4/10.0.0.1/tcp/{i}")))
            .collect();
        assert_eq!(
            StaticBootstrapDiscovery::new(too_many).err(),
            Some(DiscoveryError::TooManyItems {
                field: "entries",
                got: MAX_ENTRIES + 1,
                max: MAX_ENTRIES,
            })
        );
        // Exactly at the bound is fine.
        let at_bound: Vec<StaticEntry> = (0..MAX_ENTRIES)
            .map(|i| entry(P1, &format!("/ip4/10.0.0.1/tcp/{i}")))
            .collect();
        assert!(StaticBootstrapDiscovery::new(at_bound).is_ok());
    }

    #[test]
    fn an_empty_or_oversized_address_is_refused_at_construction() {
        assert!(StaticEntry::new(peer(P1), "").is_err());
        assert!(StaticEntry::new(peer(P1), "a".repeat(MAX_ADDRESS_BYTES + 1)).is_err());
        assert!(StaticEntry::new(peer(P1), "a".repeat(MAX_ADDRESS_BYTES)).is_ok());
    }

    #[test]
    fn every_hint_class_is_refused_explicitly() {
        let mut p = StaticBootstrapDiscovery::new(vec![entry(P1, "/ip4/10.0.0.1/tcp/1")])
            .expect("within bounds");
        p.start(0).expect("starts");
        assert!(!p.descriptor().supports_hints);
        assert_eq!(
            p.add_hint(
                PeerHint::ObservedReachable {
                    peer_id: peer(P2),
                    address: "/ip4/10.0.0.2/tcp/1".to_owned(),
                    observed_at: 0,
                },
                0
            ),
            HintDisposition::Unsupported,
            "an operator's file is not edited by a runtime observation"
        );
    }

    #[test]
    fn shutdown_is_idempotent_and_closes_the_stream() {
        let mut p = StaticBootstrapDiscovery::new(vec![entry(P1, "/ip4/10.0.0.1/tcp/1")])
            .expect("within bounds");
        p.start(0).expect("starts");
        p.shutdown(1);
        p.shutdown(2);
        assert!(p.drain_events(3, 8).is_empty());
        assert_eq!(p.health(), ProviderHealth::Unavailable);
    }

    #[test]
    fn a_second_start_is_refused() {
        let mut p = StaticBootstrapDiscovery::new(Vec::new()).expect("empty is valid");
        p.start(0).expect("starts");
        assert_eq!(p.start(1), Err(ProviderError::AlreadyStarted));
    }

    #[test]
    fn the_drain_respects_the_callers_bound() {
        let mut p = StaticBootstrapDiscovery::new(vec![
            entry(P1, "/ip4/10.0.0.1/tcp/1"),
            entry(P2, "/ip4/10.0.0.2/tcp/1"),
        ])
        .expect("within bounds");
        p.start(0).expect("starts");
        // Three events queued: the start health transition and two
        // configured entries.
        assert_eq!(p.drain_events(0, 1).len(), 1, "the caller sizes the batch");
        assert_eq!(p.drain_events(0, 8).len(), 2, "the rest stays queued");
        assert!(p.drain_events(0, 8).is_empty(), "and then it is empty");
    }
    #[test]
    fn repeated_reloads_do_not_grow_the_queue() {
        // `entries` is capped at 64; `pending` was not, so a stalled
        // consumer plus repeated reloads grew it without limit.
        let mut p = StaticBootstrapDiscovery::new(vec![entry(P1, "/ip4/10.0.0.1/tcp/4001")])
            .expect("legal entries");
        p.start(0).expect("starts");
        let _ = p.drain_events(0, 64);

        for round in 0..500u64 {
            let addr = format!("/ip4/10.0.0.{}/tcp/4001", round % 200 + 1);
            p.set_entries(vec![entry(P1, &addr)], round)
                .expect("a legal reload");
            let _ = p.drain_events(round, 0);
        }

        let queued = p.drain_events(500, usize::MAX);
        assert!(
            !queued.is_empty(),
            "the scenario must queue something, or the bound is asserted \
             against nothing"
        );
        assert!(
            queued.len() <= MAX_PENDING_EVENTS,
            "the queue stays within its bound, got {}",
            queued.len()
        );
    }

    #[test]
    fn a_removed_address_stays_retracted_across_a_later_reload() {
        // The manager is ADDITIVE, so a retraction that gets coalesced
        // away strands the address there. Only the addresses an
        // observation re-announces may be withdrawn from a pending
        // expiry.
        let mut p = StaticBootstrapDiscovery::new(vec![
            entry(P1, "/ip4/10.0.0.1/tcp/1"),
            entry(P1, "/ip4/10.0.0.2/tcp/2"),
        ])
        .expect("legal entries");
        p.start(0).expect("starts");
        let _ = p.drain_events(0, 64);

        p.set_entries(vec![entry(P1, "/ip4/10.0.0.1/tcp/1")], 10)
            .expect("reload");
        // A second reload with the same content still queues nothing new
        // for the dropped address, and must not erase the retraction.
        p.set_entries(vec![entry(P1, "/ip4/10.0.0.1/tcp/1")], 20)
            .expect("reload");

        let events = p.drain_events(20, usize::MAX);
        assert!(
            events.iter().any(|e| matches!(
                e,
                DiscoveryEvent::CandidateExpired { addresses, .. }
                    if addresses.contains("/ip4/10.0.0.2/tcp/2")
            )),
            "the removed address is still retracted: {events:?}"
        );
    }
    #[test]
    fn a_retraction_dropped_by_the_bound_is_recreated() {
        // The manager holds static provenance WITHOUT expiry, so a lost
        // retraction is permanent: the removed address stays a candidate
        // for good. Eager diffing could not recreate one, because a later
        // reload compared only the previous configuration to the next.
        let mut p = StaticBootstrapDiscovery::new(vec![entry(P1, "/ip4/10.0.0.1/tcp/1")])
            .expect("legal entries");
        p.start(0).expect("starts");

        // The consumer learns the entry, then stalls.
        let learned = p.drain_events(0, usize::MAX);
        assert!(
            learned
                .iter()
                .any(|e| matches!(e, DiscoveryEvent::CandidateObserved { .. })),
            "the consumer was told about the peer"
        );

        // Reload it away, then churn far past the bound while stalled.
        p.set_entries(vec![], 10).expect("reload");
        for round in 0..(MAX_PENDING_EVENTS as u64 * 3) {
            let addr = format!("/ip4/10.9.0.{}/tcp/4001", round % 250 + 1);
            p.set_entries(vec![entry(P2, &addr)], 100 + round)
                .expect("reload");
            let _ = p.drain_events(100 + round, 0);
        }

        // P1 must still be retracted, however much churn buried it.
        let mut retracted = false;
        for _ in 0..40 {
            for event in p.drain_events(10_000, usize::MAX) {
                if let DiscoveryEvent::CandidateExpired { peer_id, .. } = event
                    && peer_id == peer(P1)
                {
                    retracted = true;
                }
            }
        }
        assert!(
            retracted,
            "the retraction for a peer removed from configuration survives \
             the queue bound"
        );
    }
    #[test]
    fn a_trimmed_retraction_is_recreated_without_a_further_reload() {
        // The rollback marks the difference outstanding again; something
        // has to look. Recomputing only on reload meant a consumer that
        // simply resumed never saw it, and here that is permanent — the
        // manager holds static provenance without expiry.
        //
        // The shape has to be exact. A retraction trimmed EARLY is
        // recomputed by the next reload and survives, which is how a
        // first version of this test passed with the fix removed. The bug
        // needs the trim to happen on the LAST refresh, with the dropped
        // event queued by an earlier one — so the final reload has
        // already recomputed before the trim discards it.
        let mut others: Vec<StaticEntry> = (1..MAX_ENTRIES)
            .map(|i| entry_id(&synthetic(i), "/ip4/10.0.0.1/tcp/1"))
            .collect();
        let mut all = vec![entry(P1, "/ip4/10.0.0.1/tcp/1")];
        all.extend(others.clone());

        let mut p = StaticBootstrapDiscovery::new(all).expect("legal entries");
        p.start(0).expect("starts");
        let _ = p.drain_events(0, usize::MAX);

        // P1 is removed. One retraction is queued, and `emitted` no
        // longer holds P1 — so no later refresh will recompute it.
        p.set_entries(others.clone(), 10).expect("reload");

        // Churn the rest, without draining, so the SECOND reload's trim
        // discards that queued retraction. Exactly two: a third would
        // find P1 restored in `emitted` and recompute the retraction, and
        // the newly queued copy survives — which is how a first version
        // of this test passed with the fix removed.
        for round in 1..=2u64 {
            others = (1..MAX_ENTRIES)
                .map(|i| {
                    entry_id(
                        &synthetic(i),
                        &format!("/ip4/10.9.{round}.{}/tcp/4001", i % 250),
                    )
                })
                .collect();
            p.set_entries(others.clone(), 100 + round).expect("reload");
        }

        // The consumer simply resumes. No configuration change.
        let mut retracted = false;
        for _ in 0..40 {
            for event in p.drain_events(10_000, usize::MAX) {
                if let DiscoveryEvent::CandidateExpired { peer_id, .. } = event
                    && peer_id == peer(P1)
                {
                    retracted = true;
                }
            }
        }
        assert!(
            retracted,
            "the retraction is recreated by draining alone, with no \
             configuration change to prompt it"
        );
    }

    #[test]
    fn draining_a_settled_provider_queues_nothing() {
        // The control: refresh-on-drain must be idempotent, or every
        // drain manufactures events for an unchanged configuration.
        let mut p = StaticBootstrapDiscovery::new(vec![entry(P1, "/ip4/10.0.0.1/tcp/1")])
            .expect("legal entries");
        p.start(0).expect("starts");
        let _ = p.drain_events(0, usize::MAX);

        for tick in 1..=5 {
            assert!(
                p.drain_events(tick, usize::MAX).is_empty(),
                "nothing changed, so draining queues nothing"
            );
        }
    }
    #[test]
    fn the_health_transition_survives_a_trimmed_queue() {
        // Same rule as the other providers. This one is queued once at
        // start and never again, so an oldest-first trim takes it first.
        let entries: Vec<StaticEntry> = (1..MAX_ENTRIES)
            .map(|i| entry_id(&synthetic(i), "/ip4/10.0.0.1/tcp/1"))
            .collect();
        let mut p = StaticBootstrapDiscovery::new(entries).expect("legal entries");
        p.start(0).expect("starts");

        // Never drained, so the health event is the oldest thing queued.
        for round in 1..=3u64 {
            let churned: Vec<StaticEntry> = (1..MAX_ENTRIES)
                .map(|i| {
                    entry_id(
                        &synthetic(i),
                        &format!("/ip4/10.9.{round}.{}/tcp/4001", i % 250),
                    )
                })
                .collect();
            p.set_entries(churned, 100 + round).expect("reload");
        }

        let queued = p.drain_events(1_000, usize::MAX);
        assert!(
            queued
                .iter()
                .any(|e| matches!(e, DiscoveryEvent::HealthChanged { .. })),
            "the health transition is still delivered after {} events",
            queued.len()
        );
    }
    #[test]
    fn dropping_a_retraction_and_its_observation_together_still_recreates_both() {
        // A peer whose configured address set shrank queues a retraction
        // and an observation ADJACENTLY, so queue pressure takes both or
        // neither. Rolling them back in queue order restored the
        // retracted addresses and then deleted the whole record — and
        // here the loss is permanent, because the manager holds static
        // provenance without expiry.
        //
        // Driven at the bound directly with the state `refresh` produces:
        // burying the pair through reloads alone would have to leave the
        // peer untouched for long enough, and any reload that touches it
        // requeues its observation at the end.
        let kept = "/ip4/10.0.0.1/tcp/1";
        let removed = "/ip4/10.0.0.2/tcp/2";
        let mut p = StaticBootstrapDiscovery::new(vec![entry(P1, kept)]).expect("legal entries");
        p.start(0).expect("starts");
        let _ = p.drain_events(0, usize::MAX);

        // The consumer was told about both; only one is configured.
        p.emitted.insert(
            peer(P1),
            [kept.to_owned(), removed.to_owned()].into_iter().collect(),
        );
        p.refresh(20);
        assert!(
            p.pending.iter().any(|q| matches!(
                &q.event,
                DiscoveryEvent::CandidateExpired { addresses, .. }
                    if addresses.contains(removed)
            )),
            "the pair is queued"
        );

        for i in 0..MAX_PENDING_EVENTS {
            p.pending.push(Queued {
                event: observed(
                    &synthetic(1_000 + i),
                    &[kept.to_owned()].into_iter().collect(),
                    30,
                ),
                before: None,
            });
        }
        p.enforce_pending_bound();
        assert!(
            !p.pending.iter().any(|q| matches!(
                &q.event,
                DiscoveryEvent::CandidateExpired { addresses, .. }
                    if addresses.contains(removed)
            )),
            "the retraction was indeed trimmed, or this proves nothing"
        );

        let mut recreated = false;
        for _ in 0..10 {
            for event in p.drain_events(40, usize::MAX) {
                if let DiscoveryEvent::CandidateExpired { addresses, .. } = event
                    && addresses.contains(removed)
                {
                    recreated = true;
                }
            }
        }
        assert!(
            recreated,
            "the retraction is recreated after both halves were dropped"
        );
    }
    #[test]
    fn a_dropped_whole_peer_retraction_still_retracts_the_old_address() {
        // A peer removed from configuration and re-added at a DIFFERENT
        // address before the drain. The whole-peer retraction carries no
        // addresses, so a rollback rebuilding from the current entries
        // lands on the re-added state — `refresh` then finds no
        // difference and recreates neither event, leaving the manager
        // holding a non-expiring static address for a route the operator
        // removed.
        let old_address = "/ip4/10.0.0.1/tcp/1";
        let new_address = "/ip4/10.0.0.2/tcp/2";
        let mut p =
            StaticBootstrapDiscovery::new(vec![entry(P1, old_address)]).expect("legal entries");
        p.start(0).expect("starts");
        let _ = p.drain_events(0, usize::MAX);

        p.set_entries(vec![], 10).expect("removed");
        p.set_entries(vec![entry(P1, new_address)], 20)
            .expect("re-added elsewhere");

        for i in 0..MAX_PENDING_EVENTS {
            p.pending.push(Queued {
                event: observed(
                    &synthetic(2_000 + i),
                    &[new_address.to_owned()].into_iter().collect(),
                    30,
                ),
                before: None,
            });
        }
        p.enforce_pending_bound();
        assert!(
            !p.pending.iter().any(|q| matches!(
                &q.event,
                DiscoveryEvent::CandidateExpired { peer_id, .. } if *peer_id == peer(P1)
            )),
            "the retraction was indeed trimmed, or this proves nothing"
        );

        let mut retracted = false;
        for _ in 0..10 {
            for event in p.drain_events(40, usize::MAX) {
                if let DiscoveryEvent::CandidateExpired { peer_id, .. } = event
                    && peer_id == peer(P1)
                {
                    retracted = true;
                }
            }
        }
        assert!(
            retracted,
            "the whole-peer retraction is recreated, so the removed \
             address is withdrawn rather than left dialable for good"
        );
    }
}
