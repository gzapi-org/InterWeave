// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Swarm side of mDNS: announcements in, candidates out.
//!
//! `crates/discovery/mdns` is the NORMALIZATION half -- PeerId grammar,
//! address bounds, dedup, expiry -- driven by pushed observations. This
//! is the half that hears the packets, and it is the Swarm's for the
//! reason the Kademlia driver is: every mutation stays in the Swarm
//! task, and the provider crate keeps no libp2p type.
//!
//! # The boundary runs HERE, and that is ADR-0052's own placement
//!
//! A discovered candidate is an address this runtime would dial because
//! a peer supplied it, so it is inside ADR-0052's boundary (rule 1), and
//! `providers/mdns.md` §Address class states this provider's instance of
//! rules 3 and 4. What makes the site unusual is rule 5: mDNS emits no
//! `ToSwarm::Dial` -- measured, the crate has none -- so there is no
//! crate dial to deny and reissue. The boundary binds where the
//! candidate is LEARNED, which is rule 5's last clause, "what this node
//! learns and offers is held inside the boundary too".
//!
//! An address refused on class never becomes an observation. Not
//! filtered downstream, not carried with a flag: this is the only door,
//! and past it the pair is an ordinary candidate.
//!
//! Together with [`crate::mdns_scope::MdnsScope`], which swallows the
//! crate's `NewExternalAddrOfPeer`, that is ADR-0011 §Discovery never
//! writes the address book seen from both sides -- nothing the crate
//! hears reaches either book except through this filter and the
//! pipeline.
//!
//! # What it does not do
//!
//! It holds no provider. `crates/transport/libp2p` depends on
//! `discovery-api` and not on a concrete provider crate, the way the
//! Kademlia driver does, so this emits `CandidatePeer` values and the
//! composition root (plan §15) is where a `DiscoveryManager` receives
//! them. Until then the manager is a library composed in tests, which
//! is the state the Stage 9 record already describes.

use std::collections::{BTreeMap, BTreeSet};

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::reachability::{CandidateRefusal, is_discovered_address};
use libp2p::{Multiaddr, PeerId, mdns};

use super::to_transport_identity;
use crate::mdns_scope::MdnsScope;

/// The mDNS field's type in the composed behaviour.
///
/// `Toggle`, `None` by default: mDNS is present only when a profile
/// configured it, the shape every optional behaviour here takes.
///
/// NO `Attributing` AND NO `ClassGated`, and both absences are
/// decisions rather than omissions. `Attributing` announces the origin
/// of a dial, and mDNS originates none -- the crate has no
/// `ToSwarm::Dial` at all, measured. `ClassGated` decides which peers
/// are offered a protocol on a connection, and mDNS opens no substream
/// to a peer: it listens on a multicast group and reports. What it
/// needs instead is [`MdnsScope`], which is about what it may push INTO
/// the Swarm rather than what it may be asked for.
pub type MdnsField = libp2p::swarm::behaviour::toggle::Toggle<MdnsScope<mdns::tokio::Behaviour>>;

/// What a profile sets when it turns mDNS on.
///
/// Every field is a bound the crate takes as a `Duration`; they are
/// milliseconds here because a settings struct that is validated by
/// enumeration cannot carry a type whose invalid values it cannot name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdnsSettings {
    /// TTL announced for this node's own records.
    pub ttl_ms: u64,
    /// How often to re-query, so a lost initial packet is not a silent
    /// failure until the next peer joins.
    pub query_interval_ms: u64,
    /// Announce over IPv6 rather than IPv4.
    pub enable_ipv6: bool,
}

impl Default for MdnsSettings {
    fn default() -> Self {
        // The crate's own defaults, restated rather than borrowed: a
        // default that moves with a dependency bump is a configuration
        // change nobody reviewed.
        Self {
            ttl_ms: 6 * 60 * 1000,
            query_interval_ms: 5 * 60 * 1000,
            enable_ipv6: false,
        }
    }
}

impl MdnsSettings {
    /// # Errors
    /// The first bound that is not usable, named.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.ttl_ms == 0 {
            return Err("mdns ttl_ms must be non-zero");
        }
        if self.query_interval_ms == 0 {
            return Err("mdns query_interval_ms must be non-zero");
        }
        // A QUERY INTERVAL PAST THE TTL IS A PROVIDER THAT FORGETS ITSELF.
        // Records lapse before the next query refreshes them, so a peer
        // that never left is announced, expired and re-announced on the
        // interval -- churn the normalization half then has to absorb,
        // and a health signal that flaps for a network that is fine.
        if self.query_interval_ms >= self.ttl_ms {
            return Err("mdns query_interval_ms must be below ttl_ms");
        }
        Ok(())
    }
}

/// Build the wrapped behaviour.
///
/// # Errors
/// The socket the crate binds for the multicast group.
pub fn build_behaviour(
    settings: &MdnsSettings,
    local_pid: PeerId,
) -> std::io::Result<MdnsScope<mdns::tokio::Behaviour>> {
    let config = mdns::Config {
        ttl: std::time::Duration::from_millis(settings.ttl_ms),
        query_interval: std::time::Duration::from_millis(settings.query_interval_ms),
        enable_ipv6: settings.enable_ipv6,
    };
    mdns::tokio::Behaviour::new(config, local_pid).map(MdnsScope::new)
}

/// Peers one announcement batch may yield, however many announce.
///
/// `MAX_ADDRESSES` bounds the addresses of ONE peer; nothing bounded
/// the number of peers, so a single host announcing distinct PeerIds
/// chose the size of one `SwarmEvent::MdnsDiscovered` and the work the
/// Swarm task did building it. `may_buffer_delivery` does not help:
/// it bounds how many events sit in the outbox, not how large one is.
/// `DISCOVERY-CONFORMANCE.md` guarantee 5 bounds emitted BATCHES by
/// name, and mDNS input is the least trusted this process takes --
/// any host on the multicast domain, unauthenticated.
///
/// RESTATED RATHER THAN IMPORTED. `crates/discovery/mdns`'s own
/// `MAX_PEERS` is the same 256, but this crate does not depend on the
/// provider and must not start: the transport learns, the provider
/// normalizes, and the two meet through `discovery-api`. Its
/// `MdnsSettings::default` restates the crate's defaults for the same
/// reason.
const MAX_PEERS_PER_BATCH: usize = 256;

/// What the learn-site filter did, by class.
///
/// Counted rather than logged: an address refused on class is never
/// written to a log (ADR-0052 rule 5), and a count is what lets a test
/// tell a filter that ran from a multicast domain that was quiet.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MdnsCounters {
    /// Pairs that passed the boundary and became candidates.
    pub admitted: usize,
    /// Pairs refused, by the class they were refused for.
    pub refused: BTreeMap<&'static str, usize>,
    /// Pairs dropped because the batch was already at
    /// [`MAX_PEERS_PER_BATCH`] distinct peers.
    ///
    /// Separate from `refused`, which is ADR-0052's classes: a bound is
    /// not a judgement about the address, and folding it in would make
    /// a flood read as a boundary refusal.
    pub over_peer_bound: usize,
}

impl MdnsCounters {
    fn refuse(&mut self, class: &CandidateRefusal) {
        let label = match class {
            CandidateRefusal::NotLiteral => "not_literal",
            CandidateRefusal::Relayed => "relayed",
            CandidateRefusal::SpecialUse => "special_use",
            CandidateRefusal::PrivateWithoutPrivateListener => "private_without_private_listener",
        };
        *self.refused.entry(label).or_default() += 1;
    }

    /// Every refusal, whatever its class.
    #[must_use]
    pub fn refused_total(&self) -> usize {
        self.refused.values().sum()
    }
}

/// The driver's state: what the filter has done, and nothing else.
#[derive(Debug, Default)]
pub struct MdnsState {
    counters: MdnsCounters,
}

impl MdnsState {
    /// A fresh driver.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What the learn-site filter has done so far.
    #[must_use]
    pub const fn counters(&self) -> &MdnsCounters {
        &self.counters
    }

    /// Turn one `Discovered` into the candidates that survive the
    /// boundary.
    ///
    /// `own_listeners` is this node's bound listener set, which rule 3
    /// needs: a private candidate is admitted only beside a private
    /// listener of the same family.
    pub fn on_discovered<'a>(
        &mut self,
        pairs: &[(PeerId, Multiaddr)],
        own_listeners: impl IntoIterator<Item = &'a str> + Clone,
        now_ms: u64,
    ) -> Vec<interweave_discovery_api::CandidatePeer> {
        let mut by_peer: BTreeMap<TransportIdentity, BTreeSet<String>> = BTreeMap::new();
        for (peer, address) in pairs {
            let Ok(identity) = to_transport_identity(peer) else {
                continue;
            };
            let text = address.to_string();
            if let Err(class) = is_discovered_address(&text, own_listeners.clone()) {
                self.counters.refuse(&class);
                continue;
            }
            // THE BOUNDS ARE CHECKED WHILE READING, not after: the
            // announcement is remote-authored, so collecting first and
            // capping later would hold an oversized address in the
            // accumulator and emit what downstream validation refuses.
            // The Kademlia driver reads its query results the same way,
            // including the PEER bound -- `kademlia_driver` breaks at
            // `max_results_per_query`, and an earlier version of this
            // comment claimed that parity while having no peer bound at
            // all.
            if text.is_empty() || text.len() > interweave_discovery_api::MAX_ADDRESS_BYTES {
                continue;
            }
            // A PEER THIS BATCH HAS NOT SEEN COSTS A SLOT; one it has
            // costs nothing. Testing membership first is what stops a
            // host that announces one peer on many addresses from
            // spending the peer budget.
            if !by_peer.contains_key(&identity) && by_peer.len() >= MAX_PEERS_PER_BATCH {
                self.counters.over_peer_bound += 1;
                continue;
            }
            let addresses = by_peer.entry(identity).or_default();
            if addresses.len() >= interweave_discovery_api::MAX_ADDRESSES {
                continue;
            }
            addresses.insert(text);
            self.counters.admitted += 1;
        }
        by_peer
            .into_iter()
            .map(
                |(peer_id, addresses)| interweave_discovery_api::CandidatePeer {
                    peer_id,
                    addresses,
                    source: "mdns".to_owned(),
                    observed_at: now_ms,
                    expires_at: None,
                    protocol_observations: BTreeSet::new(),
                },
            )
            .collect()
    }

    /// Turn one `Expired` into the retractions the provider takes.
    ///
    /// NO BOUNDARY HERE, and that is deliberate. A retraction removes a
    /// pair the provider may already hold; refusing it on class would
    /// leave an address this node once admitted in place forever,
    /// because the only event that would clear it is the one being
    /// dropped. The floor decides what may be DIALLED, not what may be
    /// forgotten.
    pub fn on_expired(&mut self, pairs: &[(PeerId, Multiaddr)]) -> Vec<(TransportIdentity, String)> {
        // BOUNDED FOR THE SAME REASON THE DISCOVERY IS, and this is not
        // a class judgement: the retraction is still unfiltered on
        // address class (above). What is bounded is the SIZE of one
        // emitted batch, which a remote announcer would otherwise
        // choose. An expiry that does not fit is dropped rather than
        // truncated silently -- the provider ages its own entries out,
        // which is the backstop for a retraction that never arrives.
        let mut out: Vec<(TransportIdentity, String)> = Vec::new();
        for (peer, address) in pairs {
            let Ok(identity) = to_transport_identity(peer) else {
                continue;
            };
            if out.len() >= MAX_PEERS_PER_BATCH {
                self.counters.over_peer_bound += 1;
                continue;
            }
            out.push((identity, address.to_string()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    /// A REAL Ed25519 identity. `PeerId::random()` mints a digest-form
    /// id the neutral grammar refuses, so a test built on it measures
    /// `to_transport_identity` rejecting everything rather than the
    /// filter -- the trap the Kademlia driver's tests already record.
    fn peer() -> PeerId {
        libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id()
    }

    /// One host announcing more peers than the batch may carry does not
    /// choose the size of the event, or of the work building it.
    ///
    /// `DISCOVERY-CONFORMANCE.md` guarantee 5 bounds emitted batches.
    /// Remove the peer bound in `on_discovered` and this fails with
    /// `MAX_PEERS_PER_BATCH + 40` candidates.
    #[test]
    fn a_flood_of_distinct_peers_stops_at_the_batch_bound() {
        let over = 40;
        let pairs: Vec<(PeerId, Multiaddr)> = (0..MAX_PEERS_PER_BATCH + over)
            .map(|i| {
                let port = 4001 + u16::try_from(i % 1000).expect("port fits");
                (
                    peer(),
                    format!("/ip4/8.8.8.8/tcp/{port}")
                        .parse()
                        .expect("a global literal the floor admits"),
                )
            })
            .collect();

        let mut state = MdnsState::new();
        let candidates = state.on_discovered(&pairs, ["/ip4/0.0.0.0/tcp/1"], 0);

        assert_eq!(
            candidates.len(),
            MAX_PEERS_PER_BATCH,
            "the batch must stop at the bound, not at whatever the announcer sent"
        );
        assert_eq!(
            state.counters().over_peer_bound,
            over,
            "and the drop is counted, or a flood is indistinguishable from a quiet domain"
        );
        assert_eq!(
            state.counters().refused_total(),
            0,
            "a bound is not an ADR-0052 class refusal and must not be reported as one"
        );
    }

    /// The bound counts PEERS, not pairs: one peer on many addresses
    /// must not spend the peer budget. Written because the obvious
    /// implementation -- checking `by_peer.len()` before the entry --
    /// charges every pair and would cap a single chatty peer at the
    /// address bound while reporting a flood.
    #[test]
    fn one_peer_on_many_addresses_does_not_spend_the_peer_budget() {
        let only = peer();
        let pairs: Vec<(PeerId, Multiaddr)> = (0..MAX_PEERS_PER_BATCH + 40)
            .map(|i| {
                let port = 4001 + u16::try_from(i % 1000).expect("port fits");
                (
                    only,
                    format!("/ip4/8.8.8.8/tcp/{port}")
                        .parse()
                        .expect("a global literal the floor admits"),
                )
            })
            .collect();

        let mut state = MdnsState::new();
        let candidates = state.on_discovered(&pairs, ["/ip4/0.0.0.0/tcp/1"], 0);

        assert_eq!(candidates.len(), 1, "one peer is one candidate");
        assert_eq!(
            state.counters().over_peer_bound,
            0,
            "no peer slot was contested, so nothing was dropped for the peer bound"
        );
    }

    /// A retraction batch is bounded too, and for the size of the
    /// emitted batch rather than on class -- `an_expiry_is_never_refused_on_class`
    /// is the sibling that pins the class half stays open.
    #[test]
    fn a_flood_of_expiries_stops_at_the_batch_bound() {
        let over = 40;
        let pairs: Vec<(PeerId, Multiaddr)> = (0..MAX_PEERS_PER_BATCH + over)
            .map(|_| {
                (
                    peer(),
                    "/ip4/127.0.0.1/tcp/4001"
                        .parse()
                        .expect("a loopback the floor would refuse on discovery"),
                )
            })
            .collect();

        let mut state = MdnsState::new();
        let expired = state.on_expired(&pairs);

        assert_eq!(expired.len(), MAX_PEERS_PER_BATCH);
        assert_eq!(state.counters().over_peer_bound, over);
    }

    fn addr(text: &str) -> Multiaddr {
        text.parse().expect("address")
    }

    #[test]
    fn settings_refuse_a_query_interval_that_outlives_the_record() {
        // A provider that forgets itself: records lapse before the next
        // query refreshes them, so a peer that never left is announced,
        // expired and re-announced on the interval.
        assert!(
            MdnsSettings {
                ttl_ms: 1_000,
                query_interval_ms: 1_000,
                enable_ipv6: false,
            }
            .validate()
            .is_err()
        );
        assert!(MdnsSettings::default().validate().is_ok());
        assert!(
            MdnsSettings {
                ttl_ms: 0,
                ..MdnsSettings::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn a_global_candidate_is_admitted_and_a_special_use_one_never_becomes_an_observation() {
        // The floor, at the learn site. The refused address does not
        // reach the candidate list at all -- not flagged, not carried.
        let mut state = MdnsState::new();
        let (a, b) = (peer(), peer());
        let no_listeners: [&str; 0] = [];
        let out = state.on_discovered(
            &[
                (a, addr("/ip4/8.8.8.8/tcp/4001")),
                (b, addr("/ip4/127.0.0.1/tcp/4001")),
            ],
            no_listeners,
            77,
        );
        assert_eq!(out.len(), 1, "only the global candidate survives");
        assert_eq!(out[0].source, "mdns");
        assert_eq!(out[0].observed_at, 77);
        assert_eq!(state.counters().admitted, 1);
        assert_eq!(
            state.counters().refused.get("special_use").copied(),
            Some(1)
        );
    }

    #[test]
    fn a_private_candidate_needs_a_private_listener_of_its_family() {
        // providers/mdns.md's rule 3 instance, which is what makes LAN
        // discovery work at all on an RFC 1918 network -- and what stops
        // a node with no LAN interface from being handed one.
        let lan = peer();
        let mut without = MdnsState::new();
        let none: [&str; 0] = [];
        assert!(
            without
                .on_discovered(&[(lan, addr("/ip4/192.168.1.7/tcp/4001"))], none, 1)
                .is_empty()
        );
        assert_eq!(
            without
                .counters()
                .refused
                .get("private_without_private_listener")
                .copied(),
            Some(1)
        );

        let mut with = MdnsState::new();
        let out = with.on_discovered(
            &[(lan, addr("/ip4/192.168.1.7/tcp/4001"))],
            ["/ip4/192.168.1.20/tcp/4001"],
            1,
        );
        assert_eq!(out.len(), 1, "the same candidate, beside a LAN listener");
        assert_eq!(with.counters().refused_total(), 0);
    }

    #[test]
    fn the_link_local_cost_the_provider_document_records() {
        // An IPv6 link-local-only LAN yields no dialable candidate, and
        // that is the floor working rather than a gap: an announcement
        // naming fe80:: from an untrusted multicast domain cannot be
        // told from one probing this host's own interfaces. The document
        // costs this out; this is the assertion behind it.
        let mut state = MdnsState::new();
        let out = state.on_discovered(
            &[(peer(), addr("/ip6/fe80::1/tcp/4001"))],
            ["/ip6/fe80::20/tcp/4001"],
            1,
        );
        assert!(out.is_empty());
        assert_eq!(
            state.counters().refused.get("special_use").copied(),
            Some(1)
        );
    }

    #[test]
    fn an_expiry_is_never_refused_on_class() {
        // A retraction for an address the floor would refuse must still
        // reach the provider: dropping it would strand whatever the
        // provider holds, since the only event that could clear it is
        // the one being dropped. The floor decides what may be DIALLED,
        // not what may be forgotten.
        let mut state = MdnsState::new();
        let gone = peer();
        let out = state.on_expired(&[
            (gone, addr("/ip4/127.0.0.1/tcp/4001")),
            (gone, addr("/ip4/8.8.8.8/tcp/4001")),
        ]);
        assert_eq!(out.len(), 2, "both retractions pass, class notwithstanding");
    }

    #[test]
    fn one_peer_announcing_twice_is_one_candidate_with_two_addresses() {
        // The crate reports pairs, not peers. Grouping here is what
        // keeps a peer on two interfaces from arriving as two candidates
        // that the pipeline then has to reconcile.
        let both = peer();
        let mut state = MdnsState::new();
        let out = state.on_discovered(
            &[
                (both, addr("/ip4/8.8.8.8/tcp/4001")),
                (both, addr("/ip4/1.1.1.1/tcp/4001")),
            ],
            [] as [&str; 0],
            5,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].addresses.len(), 2);
    }

    #[test]
    fn a_peer_past_the_address_bound_stops_at_the_bound() {
        // Remote-authored input held to the discovery contract's bounds
        // WHILE being read, so nothing oversized reaches the
        // accumulator.
        let noisy = peer();
        let mut state = MdnsState::new();
        let pairs: Vec<(PeerId, Multiaddr)> = (0..interweave_discovery_api::MAX_ADDRESSES + 5)
            .map(|i| {
                (
                    noisy,
                    addr(&format!("/ip4/8.8.{}.{}/tcp/4001", i / 200, (i % 200) + 1)),
                )
            })
            .collect();
        let out = state.on_discovered(&pairs, [] as [&str; 0], 1);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].addresses.len(),
            interweave_discovery_api::MAX_ADDRESSES
        );
    }
}
