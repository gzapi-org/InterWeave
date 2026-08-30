// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `DISCOVERY-CONFORMANCE.md`, applied to every provider.
//!
//! The suite is written ONCE against `&mut dyn DiscoveryProvider` and run
//! for each provider, because a per-provider copy is a per-provider
//! opportunity to weaken an assertion — and the guarantees are common by
//! definition: "every provider implementation must pass a common
//! behavioral suite before it can be composed into DiscoveryManager".
//!
//! # The suite proves it can fail
//!
//! A generic suite that passes for anything proves nothing, so
//! [`Misbehaving`] breaks the rules deliberately — it emits before start,
//! ignores the caller's batch bound, and keeps emitting after shutdown —
//! and the tests at the bottom assert the suite CATCHES each violation.
//! That is this file's own mutation check.
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

use interweave_discovery_api::{
    CandidatePeer, DiscoveryEvent, DiscoveryProvider, HintDisposition, PeerHint,
    ProviderDescriptor, ProviderError, ProviderHealth, ProviderMode, ProviderScope,
};
use interweave_discovery_cache::{CacheLimits, PeerCache, PeerCacheDiscovery};
use interweave_discovery_mdns::MdnsDiscovery;
use interweave_discovery_static::{StaticBootstrapDiscovery, StaticEntry};
use interweave_transport_api::TransportIdentity;

const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

fn peer(s: &str) -> TransportIdentity {
    TransportIdentity::parse(s).expect("valid identity")
}

/// Everything a provider needs to be exercised generically.
///
/// A TRAIT rather than a struct holding `Box<dyn DiscoveryProvider>` and
/// a function pointer, because the pointer could only reach what the
/// trait object exposes — and mDNS learns exclusively through
/// `push_discovered`, which is on the concrete type. So its `observe`
/// was a no-op, and every check that asserted "whatever it emits is
/// valid" asserted nothing at all for it: the provider emitted nothing,
/// the `for` loop never ran, and all fourteen checks passed.
///
/// Each subject now owns its concrete provider and can feed it properly.
trait Subject {
    fn name(&self) -> &'static str;
    fn provider(&mut self) -> &mut dyn DiscoveryProvider;

    /// Make the provider observe `id`, and report WHAT was supplied.
    ///
    /// The return value used to be a bare `bool`, and that was the
    /// second half of the same defect as the inert adapter: knowing
    /// that an observation happened let the suite demand an emission,
    /// but not that the emission was ABOUT the observation. Every check
    /// then verified shape and provenance and nothing else — so a
    /// provider could turn an observation of P1 at one address into a
    /// valid candidate for P2 at another and pass the entire suite.
    ///
    /// Returning the input tuple is what lets every assertion be tied
    /// to it.
    fn observe(&mut self, id: &TransportIdentity, now: u64) -> Option<Supplied>;
}

/// What a subject supplied to its provider, so emissions can be checked
/// against it rather than merely validated.
#[derive(Debug, Clone)]
struct Supplied {
    peer: TransportIdentity,
    address: String,
}

impl Supplied {
    /// Assert that `candidate` is the one this input should have
    /// produced — the right peer, carrying the address supplied.
    fn assert_matches(&self, name: &str, candidate: &interweave_discovery_api::CandidatePeer) {
        assert_eq!(
            candidate.peer_id,
            self.peer,
            "{name}: emitted a candidate for {} after being given {}",
            candidate.peer_id.as_str(),
            self.peer.as_str()
        );
        assert!(
            candidate.addresses.contains(&self.address),
            "{name}: emitted addresses {:?} which do not include the supplied {}",
            candidate.addresses,
            self.address
        );
    }
}

/// Collect the candidates a drain produced, so a check can require one.
fn candidates_in(events: Vec<DiscoveryEvent>) -> Vec<interweave_discovery_api::CandidatePeer> {
    events
        .into_iter()
        .filter_map(|e| match e {
            DiscoveryEvent::CandidateObserved { candidate } => Some(*candidate),
            _ => None,
        })
        .collect()
}

/// The cache learns through the hint path.
struct CacheSubject {
    provider: PeerCacheDiscovery,
    _keep: tempfile::TempDir,
}

impl CacheSubject {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default())
            .expect("an absent file is an empty cache");
        Self {
            provider: PeerCacheDiscovery::new(cache),
            _keep: dir,
        }
    }
}

impl Subject for CacheSubject {
    fn name(&self) -> &'static str {
        "peer-cache"
    }
    fn provider(&mut self) -> &mut dyn DiscoveryProvider {
        &mut self.provider
    }
    fn observe(&mut self, id: &TransportIdentity, now: u64) -> Option<Supplied> {
        // A DISTINCT ADDRESS PER PEER, so an emission carrying the other
        // peer's address is visible rather than accidentally correct.
        let address = format!("/ip4/10.0.0.1/tcp/{}", 4001 + u16::from(id == &peer(P2)));
        self.provider.add_hint(
            PeerHint::ObservedReachable {
                peer_id: id.clone(),
                address: address.clone(),
                observed_at: now,
            },
            now,
        );
        Some(Supplied {
            peer: id.clone(),
            address,
        })
    }
}

/// Configured entries are observed at `start`; there is nothing to push.
struct StaticSubject {
    provider: StaticBootstrapDiscovery,
}

impl StaticSubject {
    fn new() -> Self {
        Self {
            provider: StaticBootstrapDiscovery::new(vec![
                StaticEntry::new(peer(P1), "/ip4/10.0.0.1/tcp/4001").expect("within bounds"),
            ])
            .expect("within bounds"),
        }
    }
}

impl Subject for StaticSubject {
    fn name(&self) -> &'static str {
        "static-bootstrap"
    }
    fn provider(&mut self) -> &mut dyn DiscoveryProvider {
        &mut self.provider
    }
    /// THROUGH `set_entries`, which is how configuration reaches this
    /// provider at runtime. This used to return false on the grounds
    /// that its candidates arrive at `start` — true of the FIRST set,
    /// and not a reason to leave the reload path unexercised. While it
    /// was inert, `provider_handles_candidate_update` skipped its
    /// assertions entirely and the duplicate check merely re-drained
    /// what `start` had emitted.
    fn observe(&mut self, id: &TransportIdentity, now: u64) -> Option<Supplied> {
        let address = format!("/ip4/10.0.0.1/tcp/{}", 4001 + u16::from(id == &peer(P2)));
        let entry = StaticEntry::new(id.clone(), &address).expect("within bounds");
        self.provider
            .set_entries(vec![entry], now)
            .expect("a single valid entry is within bounds");
        Some(Supplied {
            peer: id.clone(),
            address,
        })
    }
}

/// mDNS learns from its backend, through `push_discovered`.
struct MdnsSubject {
    provider: MdnsDiscovery,
}

impl MdnsSubject {
    fn new() -> Self {
        Self {
            provider: MdnsDiscovery::new(),
        }
    }
}

impl Subject for MdnsSubject {
    fn name(&self) -> &'static str {
        "mdns"
    }
    fn provider(&mut self) -> &mut dyn DiscoveryProvider {
        &mut self.provider
    }
    /// THE ADAPTER THAT WAS MISSING. This used to be a no-op, so the
    /// provider observed nothing and every conditional assertion in the
    /// suite was vacuous for it.
    fn observe(&mut self, id: &TransportIdentity, now: u64) -> Option<Supplied> {
        let address = format!("/ip4/192.168.1.5/tcp/{}", 4001 + u16::from(id == &peer(P2)));
        self.provider
            .push_discovered(id.as_str(), &address, now)
            .then(|| Supplied {
                peer: id.clone(),
                address,
            })
    }
}

fn provider_starts_cleanly(s: &mut dyn Subject) {
    assert!(
        s.provider().drain_events(0, 8).is_empty(),
        "{}: a provider emits no events before start",
        s.name()
    );
    s.provider().start(1_000).expect("starts");
}

fn provider_reports_initial_health(s: &mut dyn Subject) {
    assert_eq!(
        s.provider().health(),
        ProviderHealth::Unavailable,
        "{}: an unstarted provider is unavailable",
        s.name()
    );
    s.provider().start(1_000).expect("starts");
    assert_ne!(
        s.provider().health(),
        ProviderHealth::Unavailable,
        "{}: a started provider reports a live health",
        s.name()
    );
}

fn provider_emits_normalized_candidate(s: &mut dyn Subject) {
    // AN EMISSION IS REQUIRED, and it must be ABOUT the observation.
    // Two defects lived here. Every assertion sat inside `for event in
    // drain_events(..)`, so a provider emitting nothing satisfied the
    // check by never entering the loop. And what it did assert was
    // shape and provenance only — so a provider could turn an
    // observation of P1 at one address into a valid candidate for P2 at
    // another and pass.
    s.provider().start(1_000).expect("starts");
    let name = s.name();
    let source = s.provider().descriptor().name;
    let supplied = s.observe(&peer(P1), 1_000);
    let candidates = candidates_in(s.provider().drain_events(1_000, 32));

    assert!(
        !candidates.is_empty(),
        "{name}: no candidate was emitted. A provider that observes on \
         demand must emit for what it observed; one whose candidates \
         arrive at start must emit them there"
    );
    for candidate in &candidates {
        candidate
            .validate()
            .unwrap_or_else(|e| panic!("{name}: emitted an invalid candidate: {e:?}"));
        assert_eq!(
            candidate.source, source,
            "{name}: provenance — the source is the provider's own name"
        );
    }
    if let Some(supplied) = &supplied {
        let matching = candidates
            .iter()
            .find(|c| c.peer_id == supplied.peer)
            .unwrap_or_else(|| {
                panic!(
                    "{name}: given {} and emitted only {:?}",
                    supplied.peer.as_str(),
                    candidates
                        .iter()
                        .map(|c| c.peer_id.as_str())
                        .collect::<Vec<_>>()
                )
            });
        supplied.assert_matches(name, matching);
    }
}

fn provider_handles_duplicate_observation(s: &mut dyn Subject) {
    s.provider().start(1_000).expect("starts");
    let name = s.name();
    let supplied = s.observe(&peer(P1), 1_000);
    let first = candidates_in(s.provider().drain_events(1_000, 32));
    // THE SETUP MUST HAVE WORKED, and worked for the right peer —
    // otherwise the duplicate below is a duplicate of nothing, or of
    // something else.
    assert!(
        !first.is_empty(),
        "{name}: nothing was emitted for the first observation, so there \
         is no duplicate to handle"
    );
    if let Some(supplied) = &supplied {
        let matching = first
            .iter()
            .find(|c| c.peer_id == supplied.peer)
            .unwrap_or_else(|| panic!("{name}: the first observation emitted another peer"));
        supplied.assert_matches(name, matching);
    }

    // The same observation again: correct, whatever it chooses to emit.
    let repeated = s.observe(&peer(P1), 1_100);
    let again = candidates_in(s.provider().drain_events(1_100, 32));
    for candidate in &again {
        assert!(
            candidate.validate().is_ok(),
            "{name}: a duplicate must not produce an invalid candidate"
        );
        if let Some(repeated) = &repeated {
            assert_eq!(
                candidate.peer_id, repeated.peer,
                "{name}: a repeat of one peer emitted another"
            );
        }
    }
}

fn provider_handles_candidate_update(s: &mut dyn Subject) {
    s.provider().start(1_000).expect("starts");
    let name = s.name();
    let source = s.provider().descriptor().name;
    let _ = s.observe(&peer(P1), 1_000);
    let _ = s.provider().drain_events(1_000, 32);

    let updated = s.observe(&peer(P2), 2_000);
    let events = candidates_in(s.provider().drain_events(2_000, 32));
    for candidate in &events {
        assert!(candidate.validate().is_ok(), "{name}");
        assert_eq!(candidate.source, source, "{name}");
    }
    // A PROVIDER THAT CAN OBSERVE ON DEMAND MUST EMIT for the second
    // peer, at the address it was given — checking only that P2 appeared
    // accepted a candidate carrying the first peer's address.
    if let Some(updated) = &updated {
        let matching = events
            .iter()
            .find(|c| c.peer_id == updated.peer)
            .unwrap_or_else(|| {
                panic!(
                    "{name}: observed {} and emitted only {:?}",
                    updated.peer.as_str(),
                    events
                        .iter()
                        .map(|c| c.peer_id.as_str())
                        .collect::<Vec<_>>()
                )
            });
        updated.assert_matches(name, matching);
    }
}

fn provider_expires_when_semantics_support_ttl(s: &mut dyn Subject) {
    s.provider().start(1_000).expect("starts");
    let name = s.name();
    let source = s.provider().descriptor().name;
    let supports = s.provider().descriptor().supports_expiry;
    let supplied = s.observe(&peer(P1), 1_000);
    // WHAT WAS EMITTED IS RETAINED, and checked against the input first:
    // trusting a possibly fabricated emission and then matching
    // retractions to it proves only that the provider is
    // self-consistent.
    let emitted = candidates_in(s.provider().drain_events(1_000, 32));
    if let Some(supplied) = &supplied {
        let matching = emitted
            .iter()
            .find(|c| c.peer_id == supplied.peer)
            .unwrap_or_else(|| panic!("{name}: nothing was emitted for the observed peer"));
        supplied.assert_matches(name, matching);
    }

    // Far in the future. A provider that models expiry must retract what
    // it emitted; one that does not must simply stay correct.
    let late = s.provider().drain_events(u64::from(u32::MAX), 32);
    let expiries: Vec<(TransportIdentity, String, BTreeSet<String>)> = late
        .iter()
        .filter_map(|e| match e {
            DiscoveryEvent::CandidateExpired {
                peer_id,
                source,
                addresses,
            } => Some((peer_id.clone(), source.clone(), addresses.clone())),
            _ => None,
        })
        .collect();

    if supports && supplied.is_some() {
        assert!(
            !expiries.is_empty(),
            "{name}: declares supports_expiry and retracted nothing at the \
             end of time"
        );
        // EVERY emitted candidate is retracted, and every retraction for
        // it names only its own addresses — the earlier version checked
        // the FIRST same-peer retraction and stopped, so a second one
        // carrying foreign addresses went unexamined.
        for candidate in &emitted {
            let mine: Vec<_> = expiries
                .iter()
                .filter(|(peer_id, _, _)| *peer_id == candidate.peer_id)
                .collect();
            assert!(
                !mine.is_empty(),
                "{name}: emitted a candidate for {} and never retracted it; \
                 retractions were for {:?}",
                candidate.peer_id.as_str(),
                expiries
                    .iter()
                    .map(|(p, _, _)| p.as_str())
                    .collect::<Vec<_>>()
            );
            for (_, _, addresses) in mine {
                assert!(
                    addresses.iter().all(|a| candidate.addresses.contains(a)),
                    "{name}: retracted addresses {addresses:?} that are not this \
                     candidate's {:?}",
                    candidate.addresses
                );
            }
        }
    }
    for (peer_id, src, _) in &expiries {
        assert!(
            supports,
            "{name}: a provider that declares no expiry emitted one"
        );
        assert_eq!(src, &source, "{name}");
        assert!(
            emitted.is_empty() || emitted.iter().any(|c| c.peer_id == *peer_id),
            "{name}: retracted {} which it never emitted",
            peer_id.as_str()
        );
    }
}

fn provider_rejects_or_ignores_invalid_address_safely(s: &mut dyn Subject) {
    s.provider().start(1_000).expect("starts");
    // An oversized address through the hint path: refused or unsupported,
    // never accepted-and-emitted, and never a panic.
    let disposition = s.provider().add_hint(
        PeerHint::ObservedReachable {
            peer_id: peer(P2),
            address: "a".repeat(4096),
            observed_at: 1_000,
        },
        1_000,
    );
    assert_ne!(
        disposition,
        HintDisposition::Accepted,
        "{}: an out-of-bounds address must not be accepted",
        s.name()
    );
    // AND THE REFUSED ADDRESS NEVER APPEARS. "Refused or unsupported,
    // never accepted-and-emitted" is the guarantee, and only this half
    // says anything about emission — validating whatever happens to
    // arrive would pass a provider that emitted the oversized address
    // inside an otherwise-valid candidate.
    let name = s.name();
    for candidate in candidates_in(s.provider().drain_events(1_000, 32)) {
        assert!(candidate.validate().is_ok(), "{name}");
        assert!(
            !candidate.addresses.iter().any(|a| a.len() > 1_024),
            "{name}: emitted the oversized address it had refused"
        );
    }
}

fn provider_survives_malformed_provider_input(s: &mut dyn Subject) {
    s.provider().start(1_000).expect("starts");
    // Every hint class, including ones this provider does not handle.
    for hint in [
        PeerHint::ObservedReachable {
            peer_id: peer(P1),
            address: String::new(),
            observed_at: 1_000,
        },
        PeerHint::ObservedProtocol {
            peer_id: peer(P1),
            protocol_id: interweave_discovery_api::ProtocolId::parse("/x/1.0.0").expect("valid"),
            supported: true,
            observed_at: 1_000,
        },
        PeerHint::CandidateHint(Box::new(CandidatePeer {
            peer_id: peer(P2),
            addresses: BTreeSet::new(),
            source: "somebody-else".to_owned(),
            observed_at: 1_000,
            expires_at: None,
            protocol_observations: BTreeSet::new(),
        })),
    ] {
        // The assertion is that this returns rather than panicking.
        let _ = s.provider().add_hint(hint, 1_000);
    }
    let _ = s.provider().drain_events(1_000, 32);
}

fn provider_respects_state_bounds(s: &mut dyn Subject) {
    s.provider().start(1_000).expect("starts");
    let _ = s.observe(&peer(P1), 1_000);
    let _ = s.observe(&peer(P2), 1_000);
    assert!(
        s.provider().drain_events(1_000, 1).len() <= 1,
        "{}: the CALLER sizes the batch",
        s.name()
    );
    assert!(
        s.provider().drain_events(1_000, 0).is_empty(),
        "{}: a zero bound takes nothing",
        s.name()
    );
}

fn provider_shutdown_is_idempotent_and_bounded(s: &mut dyn Subject) {
    s.provider().start(1_000).expect("starts");
    s.provider().shutdown(2_000);
    s.provider().shutdown(2_001);
    assert_eq!(
        s.provider().health(),
        ProviderHealth::Unavailable,
        "{}: a stopped provider is unavailable",
        s.name()
    );
}

fn provider_event_stream_closes_after_shutdown(s: &mut dyn Subject) {
    s.provider().start(1_000).expect("starts");
    let _ = s.observe(&peer(P1), 1_000);
    s.provider().shutdown(2_000);
    assert!(
        s.provider().drain_events(3_000, 32).is_empty(),
        "{}: the stream terminates deterministically",
        s.name()
    );
    let _ = s.observe(&peer(P2), 3_000);
    assert!(
        s.provider().drain_events(4_000, 32).is_empty(),
        "{}: and stays closed",
        s.name()
    );
}

fn provider_does_not_own_connection_policy(s: &mut dyn Subject) {
    // STRUCTURAL. `DiscoveryEvent` has no variant meaning "connect to
    // this peer" and this crate cannot reach a Swarm, a dial, or a
    // connection type — there is nothing for a provider to call. What is
    // checkable at runtime is that everything it emits is one of the
    // three advisory shapes.
    s.provider().start(1_000).expect("starts");
    let _ = s.observe(&peer(P1), 1_000);
    for event in s.provider().drain_events(1_000, 32) {
        match event {
            DiscoveryEvent::CandidateObserved { .. }
            | DiscoveryEvent::CandidateExpired { .. }
            | DiscoveryEvent::HealthChanged { .. } => {}
        }
    }
}

fn provider_does_not_grant_trust(s: &mut dyn Subject) {
    // Also structural: `interweave-discovery-api` does not depend on
    // `trust-api`, so no provider can name a trust type, and none of
    // these crates depends on it either. The runtime check is that a
    // candidate carries no authorization-shaped field — which the closed
    // schema guarantees, and `validate` enforces.
    s.provider().start(1_000).expect("starts");
    let name = s.name();
    let supplied = s.observe(&peer(P1), 1_000);
    let candidates = candidates_in(s.provider().drain_events(1_000, 32));
    // A PROVIDER THAT WAS GIVEN SOMETHING MUST HAVE EMITTED SOMETHING,
    // or this check inspects an empty list and concludes nothing carried
    // authorization — the same vacuity as the candidate checks had.
    if supplied.is_some() {
        assert!(
            !candidates.is_empty(),
            "{name}: nothing to inspect, so this guarantee was not checked"
        );
    }
    for candidate in &candidates {
        assert!(candidate.validate().is_ok(), "{name}");
    }
}

fn provider_failure_does_not_panic(s: &mut dyn Subject) {
    // A second start is the failure every provider can be made to have.
    s.provider().start(1_000).expect("starts");
    assert_eq!(
        s.provider().start(1_001),
        Err(ProviderError::AlreadyStarted),
        "{}: a lifecycle failure is a value, not a panic",
        s.name()
    );
}

/// The suite, as a list, so it can be run over any provider — including
/// the misbehaving one below.
type Check = (&'static str, fn(&mut dyn Subject));

const SUITE: &[Check] = &[
    ("provider_starts_cleanly", provider_starts_cleanly),
    (
        "provider_reports_initial_health",
        provider_reports_initial_health,
    ),
    (
        "provider_emits_normalized_candidate",
        provider_emits_normalized_candidate,
    ),
    (
        "provider_handles_duplicate_observation",
        provider_handles_duplicate_observation,
    ),
    (
        "provider_handles_candidate_update",
        provider_handles_candidate_update,
    ),
    (
        "provider_expires_when_semantics_support_ttl",
        provider_expires_when_semantics_support_ttl,
    ),
    (
        "provider_rejects_or_ignores_invalid_address_safely",
        provider_rejects_or_ignores_invalid_address_safely,
    ),
    (
        "provider_survives_malformed_provider_input",
        provider_survives_malformed_provider_input,
    ),
    (
        "provider_respects_state_bounds",
        provider_respects_state_bounds,
    ),
    (
        "provider_shutdown_is_idempotent_and_bounded",
        provider_shutdown_is_idempotent_and_bounded,
    ),
    (
        "provider_event_stream_closes_after_shutdown",
        provider_event_stream_closes_after_shutdown,
    ),
    (
        "provider_does_not_own_connection_policy",
        provider_does_not_own_connection_policy,
    ),
    (
        "provider_does_not_grant_trust",
        provider_does_not_grant_trust,
    ),
    (
        "provider_failure_does_not_panic",
        provider_failure_does_not_panic,
    ),
];

#[test]
fn every_provider_passes_the_shared_suite() {
    assert_eq!(
        SUITE.len(),
        14,
        "the fourteen named in DISCOVERY-CONFORMANCE.md"
    );
    let makers: [fn() -> Box<dyn Subject>; 3] = [
        || Box::new(CacheSubject::new()),
        || Box::new(StaticSubject::new()),
        || Box::new(MdnsSubject::new()),
    ];
    for make in makers {
        for (name, check) in SUITE {
            // A fresh subject per check: the guarantees are about a
            // provider's own lifecycle, and sharing one would let an
            // earlier check's shutdown decide a later one's result.
            let mut subject = make();
            let label = subject.name();
            check(subject.as_mut());
            let _ = (name, label);
        }
    }
}

#[test]
fn the_mdns_observation_path_is_exercised_too() {
    // The generic suite treats mDNS as observing nothing on demand,
    // because only its concrete type can be fed. Its own crate covers
    // normalization in depth; this is the conformance-shaped check that
    // what it emits is valid and correctly attributed.
    let mut p = MdnsDiscovery::new();
    p.start(0).expect("starts");
    const PUSHED: &str = "/ip4/192.168.1.5/tcp/4001";
    assert!(p.push_discovered(P1, PUSHED, 0));
    let source = p.descriptor().name;
    let candidates = candidates_in(p.drain_events(0, 32));
    // ASSERTED AND BOUND TO THE PUSH. This had the same two defects as
    // the shared checks: assertions nested inside `if let`, so emitting
    // nothing passed; and no comparison against what was pushed, so the
    // ADDRESS went unchecked even once the peer did not.
    assert!(
        !candidates.is_empty(),
        "mdns: a pushed observation must produce a candidate"
    );
    for candidate in &candidates {
        candidate.validate().expect("valid");
        assert_eq!(candidate.source, source);
        assert_eq!(
            candidate.peer_id,
            peer(P1),
            "the candidate names the peer that was pushed"
        );
        assert!(
            candidate.addresses.contains(PUSHED),
            "the candidate carries the address that was pushed, got {:?}",
            candidate.addresses
        );
    }
}

// --- the suite's own mutation check ------------------------------------

/// A provider that breaks the rules on purpose.
///
/// Every violation is one the shared suite claims to catch. If the suite
/// passes for this, the suite is decoration.
struct Misbehaving {
    started: bool,
    stopped: bool,
    violation: Violation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Violation {
    /// Emits before `start`.
    EmitsBeforeStart,
    /// Ignores the caller's batch bound.
    IgnoresBatchBound,
    /// Keeps emitting after `shutdown`.
    EmitsAfterShutdown,
    /// Stamps another provider's name on its candidates.
    ForgesProvenance,
    /// Accepts a hint it cannot honour.
    AcceptsAnythingAsAHint,
    /// Reports healthy before it has started.
    HealthyBeforeStart,
    /// A second start panics instead of returning an error.
    PanicsOnSecondStart,
    /// Declares expiry, emits a candidate for one peer, and retracts a
    /// DIFFERENT one — leaving what it observed live and withdrawing a
    /// route it never announced.
    ExpiresTheWrongPeer,
    /// Emits a VALID candidate that has nothing to do with what it was
    /// given: another peer, another address. Shape and provenance are
    /// both correct, which is precisely why a suite checking only those
    /// accepted it.
    FabricatesAnUnrelatedCandidate,
}

impl Misbehaving {
    fn new(violation: Violation) -> Self {
        Self {
            started: false,
            stopped: false,
            violation,
        }
    }

    fn candidate(&self, now_ms: u64) -> DiscoveryEvent {
        let source = if self.violation == Violation::ForgesProvenance {
            "peer-cache".to_owned() // not this provider's name
        } else {
            "misbehaving".to_owned()
        };
        DiscoveryEvent::CandidateObserved {
            candidate: Box::new(CandidatePeer {
                peer_id: peer(P1),
                addresses: ["/ip4/10.0.0.1/tcp/4001".to_owned()].into_iter().collect(),
                source,
                observed_at: now_ms,
                expires_at: None,
                protocol_observations: BTreeSet::new(),
            }),
        }
    }
}

impl DiscoveryProvider for Misbehaving {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            name: "misbehaving".to_owned(),
            interface_version: "1.0".to_owned(),
            config_version: None,
            scope: ProviderScope::Local,
            mode: ProviderMode::Passive,
            supports_expiry: self.violation == Violation::ExpiresTheWrongPeer,
            supports_hints: false,
        }
    }

    fn start(&mut self, _now_ms: u64) -> Result<(), ProviderError> {
        if self.started && self.violation == Violation::PanicsOnSecondStart {
            panic!("a second start");
        }
        if self.started {
            return Err(ProviderError::AlreadyStarted);
        }
        self.started = true;
        Ok(())
    }

    fn drain_events(&mut self, now_ms: u64, max: usize) -> Vec<DiscoveryEvent> {
        match self.violation {
            Violation::EmitsBeforeStart => vec![self.candidate(now_ms)],
            Violation::IgnoresBatchBound if self.started => {
                let _ = max;
                vec![self.candidate(now_ms), self.candidate(now_ms)]
            }
            Violation::EmitsAfterShutdown if self.started => vec![self.candidate(now_ms)],
            Violation::ForgesProvenance if self.started && !self.stopped => {
                vec![self.candidate(now_ms)]
            }
            Violation::FabricatesAnUnrelatedCandidate if self.started && !self.stopped => {
                // VALID, correctly attributed, and about nothing it was
                // given: another peer at another address.
                vec![DiscoveryEvent::CandidateObserved {
                    candidate: Box::new(CandidatePeer {
                        peer_id: peer(P2),
                        addresses: ["/ip4/203.0.113.9/tcp/9".to_owned()].into_iter().collect(),
                        source: "misbehaving".to_owned(),
                        observed_at: now_ms,
                        expires_at: None,
                        protocol_observations: BTreeSet::new(),
                    }),
                }]
            }
            Violation::ExpiresTheWrongPeer if self.started && !self.stopped => {
                // Early: the candidate. Late: a retraction naming the
                // OTHER peer, which is the defect — the suite must not
                // accept a retraction it cannot tie to what was emitted.
                if now_ms < u64::from(u32::MAX) {
                    vec![self.candidate(now_ms)]
                } else {
                    vec![DiscoveryEvent::CandidateExpired {
                        peer_id: peer(P2),
                        source: "misbehaving".to_owned(),
                        addresses: BTreeSet::new(),
                    }]
                }
            }
            _ if !self.started || self.stopped => Vec::new(),
            _ => {
                let _ = max;
                Vec::new()
            }
        }
    }

    fn add_hint(&mut self, _hint: PeerHint, _now_ms: u64) -> HintDisposition {
        if self.violation == Violation::AcceptsAnythingAsAHint {
            // Declares supports_hints: false and accepts anyway — the
            // silent taking-of-ownership the contract forbids.
            return HintDisposition::Accepted;
        }
        HintDisposition::Unsupported
    }

    fn health(&self) -> ProviderHealth {
        if self.violation == Violation::HealthyBeforeStart {
            return ProviderHealth::Healthy;
        }
        if self.started && !self.stopped {
            ProviderHealth::Healthy
        } else {
            ProviderHealth::Unavailable
        }
    }

    fn shutdown(&mut self, _now_ms: u64) {
        self.stopped = true;
    }
}

/// The rule-breaking subject the suite must catch.
struct MisbehavingSubject {
    provider: Misbehaving,
}

impl Subject for MisbehavingSubject {
    fn name(&self) -> &'static str {
        "misbehaving"
    }
    fn provider(&mut self) -> &mut dyn DiscoveryProvider {
        &mut self.provider
    }
    /// TRUE, and deliberately so: a provider claiming to observe on
    /// demand and then emitting nothing is itself a violation the suite
    /// must catch. Reporting `false` here would exempt it from the
    /// emission checks — which is precisely the hole that let the real
    /// mDNS subject pass while emitting nothing.
    fn observe(&mut self, id: &TransportIdentity, _now: u64) -> Option<Supplied> {
        Some(Supplied {
            peer: id.clone(),
            address: "/ip4/10.0.0.1/tcp/4001".to_owned(),
        })
    }
}

fn misbehaving_subject(violation: Violation) -> MisbehavingSubject {
    MisbehavingSubject {
        provider: Misbehaving::new(violation),
    }
}

/// Run one named check against a misbehaving provider and report whether
/// it failed, which is what "the suite catches this" means.
fn suite_catches(violation: Violation, check_name: &str) -> bool {
    let (_, check) = SUITE
        .iter()
        .find(|(n, _)| *n == check_name)
        .expect("a check by that name");
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut subject = misbehaving_subject(violation);
        check(&mut subject);
    }))
    .is_err()
}

#[test]
fn the_suite_catches_a_provider_that_emits_before_start() {
    assert!(
        suite_catches(Violation::EmitsBeforeStart, "provider_starts_cleanly"),
        "a suite that passes for a provider emitting before start proves nothing"
    );
}

#[test]
fn the_suite_catches_a_provider_that_ignores_the_batch_bound() {
    assert!(suite_catches(
        Violation::IgnoresBatchBound,
        "provider_respects_state_bounds"
    ));
}

#[test]
fn the_suite_catches_a_provider_that_emits_after_shutdown() {
    assert!(suite_catches(
        Violation::EmitsAfterShutdown,
        "provider_event_stream_closes_after_shutdown"
    ));
}

#[test]
fn the_suite_catches_a_provider_that_fabricates_an_unrelated_candidate() {
    // The whole class the review named: every candidate assertion
    // verified shape and source and nothing else, so a provider could
    // turn an observation of P1 at one address into a valid candidate
    // for P2 at another and pass the entire suite. Each check is bound
    // to the input now, and each catches it on its own.
    for check in [
        "provider_emits_normalized_candidate",
        "provider_handles_duplicate_observation",
        "provider_handles_candidate_update",
        "provider_expires_when_semantics_support_ttl",
    ] {
        assert!(
            suite_catches(Violation::FabricatesAnUnrelatedCandidate, check),
            "{check} must refuse a candidate unrelated to what was supplied"
        );
    }
}

#[test]
fn the_suite_catches_a_provider_that_expires_the_wrong_peer() {
    // The expiry check used to reduce every retraction to its `source`,
    // so a provider that observed P1 and retracted P2 satisfied it —
    // and composed into `DiscoveryManager` that leaves the observed
    // candidate live while withdrawing a route it never announced.
    assert!(
        suite_catches(
            Violation::ExpiresTheWrongPeer,
            "provider_expires_when_semantics_support_ttl"
        ),
        "a retraction must be tied to what the provider emitted"
    );
}

#[test]
fn the_suite_catches_a_provider_that_forges_provenance() {
    assert!(suite_catches(
        Violation::ForgesProvenance,
        "provider_emits_normalized_candidate"
    ));
}

#[test]
fn the_suite_catches_a_provider_that_accepts_a_hint_it_cannot_honour() {
    assert!(suite_catches(
        Violation::AcceptsAnythingAsAHint,
        "provider_rejects_or_ignores_invalid_address_safely"
    ));
}

#[test]
fn the_suite_catches_a_provider_healthy_before_it_started() {
    assert!(suite_catches(
        Violation::HealthyBeforeStart,
        "provider_reports_initial_health"
    ));
}

#[test]
fn the_suite_catches_a_provider_that_panics_instead_of_erroring() {
    assert!(suite_catches(
        Violation::PanicsOnSecondStart,
        "provider_failure_does_not_panic"
    ));
}
