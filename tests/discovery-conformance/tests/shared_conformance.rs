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

    /// Make the provider observe `id`, and report whether this provider
    /// CAN be made to observe on demand.
    ///
    /// The boolean is what lets the suite demand an emission from the
    /// providers that have an input, without demanding one from a
    /// provider whose candidates all arrive at `start`.
    fn observe(&mut self, id: &TransportIdentity, now: u64) -> bool;
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
    fn observe(&mut self, id: &TransportIdentity, now: u64) -> bool {
        self.provider.add_hint(
            PeerHint::ObservedReachable {
                peer_id: id.clone(),
                address: "/ip4/10.0.0.1/tcp/4001".to_owned(),
                observed_at: now,
            },
            now,
        );
        true
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
    /// FALSE, and that is a fact about the provider rather than an
    /// exemption: its candidates arrive at `start`, so the suite demands
    /// an emission there instead of after an observation.
    fn observe(&mut self, _id: &TransportIdentity, _now: u64) -> bool {
        false
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
    fn observe(&mut self, id: &TransportIdentity, now: u64) -> bool {
        self.provider
            .push_discovered(id.as_str(), "/ip4/192.168.1.5/tcp/4001", now)
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
    // AN EMISSION IS REQUIRED, not merely validated if it happens. Every
    // assertion here used to sit inside `for event in drain_events(..)`,
    // so a provider that emitted nothing satisfied the whole check by
    // never entering the loop — which is exactly what mDNS did, because
    // its `observe` was a no-op. "Normalized candidate output" is a
    // mandatory common guarantee, and a guarantee no run can fail is not
    // one.
    s.provider().start(1_000).expect("starts");
    let name = s.name();
    let source = s.provider().descriptor().name;
    let observed = s.observe(&peer(P1), 1_000);
    let candidates = candidates_in(s.provider().drain_events(1_000, 32));

    assert!(
        !candidates.is_empty(),
        "{name}: no candidate was emitted. A provider that observes on \
         demand must emit for what it observed; one whose candidates \
         arrive at start must emit them there. Either way this guarantee \
         is about output that exists (observed-on-demand: {observed})"
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
}

fn provider_handles_duplicate_observation(s: &mut dyn Subject) {
    s.provider().start(1_000).expect("starts");
    let name = s.name();
    let observes = s.observe(&peer(P1), 1_000);
    let first = candidates_in(s.provider().drain_events(1_000, 32));
    // THE SETUP MUST HAVE WORKED. Without this the duplicate below is a
    // duplicate of nothing, and the check passes for a provider that
    // never observed anything in the first place.
    assert!(
        !first.is_empty(),
        "{name}: nothing was emitted for the first observation, so there \
         is no duplicate to handle"
    );

    // The same observation again: correct, whatever it chooses to emit.
    let _ = s.observe(&peer(P1), 1_100);
    let again = candidates_in(s.provider().drain_events(1_100, 32));
    for candidate in &again {
        assert!(
            candidate.validate().is_ok(),
            "{name}: a duplicate must not produce an invalid candidate"
        );
    }
    let _ = observes;
}

fn provider_handles_candidate_update(s: &mut dyn Subject) {
    s.provider().start(1_000).expect("starts");
    let name = s.name();
    let source = s.provider().descriptor().name;
    let observes = s.observe(&peer(P1), 1_000);
    let _ = s.provider().drain_events(1_000, 32);

    let updated = s.observe(&peer(P2), 2_000);
    let events = candidates_in(s.provider().drain_events(2_000, 32));
    // A PROVIDER THAT CAN OBSERVE ON DEMAND MUST EMIT for the second
    // peer. One whose candidates arrive at start has nothing to update,
    // and says so through `observe` returning false — which is a fact
    // about the provider, not a way out of the assertion.
    if updated {
        assert!(
            !events.is_empty(),
            "{name}: observed a second peer and emitted nothing"
        );
        assert!(
            events.iter().any(|c| c.peer_id == peer(P2)),
            "{name}: the update names the peer that was observed"
        );
    }
    for candidate in &events {
        assert!(candidate.validate().is_ok(), "{name}");
        assert_eq!(candidate.source, source, "{name}");
    }
    let _ = observes;
}

fn provider_expires_when_semantics_support_ttl(s: &mut dyn Subject) {
    s.provider().start(1_000).expect("starts");
    let name = s.name();
    let source = s.provider().descriptor().name;
    let supports = s.provider().descriptor().supports_expiry;
    let observed = s.observe(&peer(P1), 1_000);
    let _ = s.provider().drain_events(1_000, 32);

    // Far in the future. A provider that models expiry must retract what
    // it emitted; one that does not must simply stay correct.
    let late = s.provider().drain_events(u64::from(u32::MAX), 32);
    let expiries: Vec<_> = late
        .iter()
        .filter_map(|e| match e {
            DiscoveryEvent::CandidateExpired { source, .. } => Some(source.clone()),
            _ => None,
        })
        .collect();

    if supports && observed {
        // REQUIRED, not merely permitted. `supports_expiry: true` is a
        // declaration the suite can hold the provider to, and the old
        // check only validated an expiry that happened to arrive — so a
        // provider declaring expiry and never expiring anything passed.
        assert!(
            !expiries.is_empty(),
            "{name}: declares supports_expiry and retracted nothing at the \
             end of time"
        );
    }
    for src in &expiries {
        assert!(
            supports,
            "{name}: a provider that declares no expiry emitted one"
        );
        assert_eq!(src, &source, "{name}");
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
    for event in s.provider().drain_events(1_000, 32) {
        if let DiscoveryEvent::CandidateObserved { candidate } = event {
            assert!(candidate.validate().is_ok(), "{}", s.name());
        }
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
    let _ = s.observe(&peer(P1), 1_000);
    for event in s.provider().drain_events(1_000, 32) {
        if let DiscoveryEvent::CandidateObserved { candidate } = event {
            assert!(candidate.validate().is_ok(), "{}", s.name());
        }
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
    assert!(p.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0));
    let source = p.descriptor().name;
    let candidates = candidates_in(p.drain_events(0, 32));
    // ASSERTED, not iterated. This had the same shape as the checks it
    // was standing in for: every assertion inside `if let`, so an mDNS
    // provider that emitted nothing passed it too.
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
            supports_expiry: false,
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
    fn observe(&mut self, _id: &TransportIdentity, _now: u64) -> bool {
        true
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
