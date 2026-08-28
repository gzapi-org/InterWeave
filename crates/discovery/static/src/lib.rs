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

/// Configured bootstrap entries, presented as a discovery provider.
#[derive(Debug, Default)]
pub struct StaticBootstrapDiscovery {
    /// peer -> the addresses configured for it.
    entries: BTreeMap<TransportIdentity, BTreeSet<String>>,
    started: bool,
    stopped: bool,
    pending: Vec<DiscoveryEvent>,
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

        if self.started && !self.stopped {
            for (peer_id, addresses) in &self.entries {
                match replacement.get(peer_id) {
                    // Gone entirely.
                    None => self.pending.push(DiscoveryEvent::CandidateExpired {
                        peer_id: peer_id.clone(),
                        source: SOURCE.to_owned(),
                        addresses: BTreeSet::new(),
                    }),
                    // Still configured, but some addresses went.
                    Some(kept) => {
                        let dropped: BTreeSet<String> =
                            addresses.difference(kept).cloned().collect();
                        if !dropped.is_empty() {
                            self.pending.push(DiscoveryEvent::CandidateExpired {
                                peer_id: peer_id.clone(),
                                source: SOURCE.to_owned(),
                                addresses: dropped,
                            });
                        }
                    }
                }
            }
            for (peer_id, addresses) in &replacement {
                if self.entries.get(peer_id) != Some(addresses) {
                    self.pending.push(observed(peer_id, addresses, now_ms));
                }
            }
        }
        self.entries = replacement;
        Ok(())
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
        self.pending.push(DiscoveryEvent::HealthChanged {
            source: SOURCE.to_owned(),
            health: ProviderHealth::Healthy,
        });
        for (peer_id, addresses) in &self.entries {
            self.pending.push(observed(peer_id, addresses, now_ms));
        }
        Ok(())
    }

    fn drain_events(&mut self, _now_ms: u64, max: usize) -> Vec<DiscoveryEvent> {
        if !self.started || self.stopped {
            return Vec::new();
        }
        let take = max.min(self.pending.len());
        self.pending.drain(..take).collect()
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
}
