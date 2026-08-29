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

/// Everything a provider needs to be exercised generically: the provider
/// itself, and a way to make it observe something, since each learns
/// about peers differently.
struct Subject {
    name: &'static str,
    provider: Box<dyn DiscoveryProvider>,
    /// Make the provider observe a peer, if it can be made to.
    observe: fn(&mut dyn DiscoveryProvider, &TransportIdentity, u64),
    /// Retained so a temporary directory outlives the provider using it.
    _keep: Option<tempfile::TempDir>,
}

fn cache_subject() -> Subject {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default())
        .expect("an absent file is an empty cache");
    Subject {
        name: "peer-cache",
        provider: Box::new(PeerCacheDiscovery::new(cache)),
        observe: |p, id, now| {
            // Through the hint path, which is how the cache learns.
            p.add_hint(
                PeerHint::ObservedReachable {
                    peer_id: id.clone(),
                    address: "/ip4/10.0.0.1/tcp/4001".to_owned(),
                    observed_at: now,
                },
                now,
            );
        },
        _keep: Some(dir),
    }
}

fn static_subject() -> Subject {
    Subject {
        name: "static-bootstrap",
        provider: Box::new(
            StaticBootstrapDiscovery::new(vec![
                StaticEntry::new(peer(P1), "/ip4/10.0.0.1/tcp/4001").expect("within bounds"),
            ])
            .expect("within bounds"),
        ),
        // Configured entries are observed at start; there is nothing to
        // push, and a provider that cannot be made to observe on demand is
        // not thereby exempt from the rest of the suite.
        observe: |_, _, _| {},
        _keep: None,
    }
}

/// mDNS learns only from its backend, so the generic suite treats it as a
/// provider that observes nothing on demand; a dedicated test below
/// covers its observation path, and its own crate covers normalization in
/// depth.
fn mdns_subject() -> Subject {
    Subject {
        name: "mdns",
        provider: Box::new(MdnsDiscovery::new()),
        observe: |p, id, now| {
            // Downcasting is avoided by going through the trait where
            // possible; mDNS learns only from the backend, so the suite
            // reaches its push method through a helper below.
            let _ = (p, id, now);
        },
        _keep: None,
    }
}

// --- the fourteen shared guarantees ------------------------------------
//
// Names are `DISCOVERY-CONFORMANCE.md`'s, verbatim.

fn provider_starts_cleanly(s: &mut Subject) {
    assert!(
        s.provider.drain_events(0, 8).is_empty(),
        "{}: a provider emits no events before start",
        s.name
    );
    s.provider.start(1_000).expect("starts");
}

fn provider_reports_initial_health(s: &mut Subject) {
    assert_eq!(
        s.provider.health(),
        ProviderHealth::Unavailable,
        "{}: an unstarted provider is unavailable",
        s.name
    );
    s.provider.start(1_000).expect("starts");
    assert_ne!(
        s.provider.health(),
        ProviderHealth::Unavailable,
        "{}: a started provider reports a live health",
        s.name
    );
}

fn provider_emits_normalized_candidate(s: &mut Subject) {
    s.provider.start(1_000).expect("starts");
    (s.observe)(s.provider.as_mut(), &peer(P1), 1_000);
    for event in s.provider.drain_events(1_000, 32) {
        if let DiscoveryEvent::CandidateObserved { candidate } = event {
            candidate
                .validate()
                .unwrap_or_else(|e| panic!("{}: emitted an invalid candidate: {e:?}", s.name));
            assert_eq!(
                candidate.source,
                s.provider.descriptor().name,
                "{}: provenance — the source is the provider's own name",
                s.name
            );
        }
    }
}

fn provider_handles_duplicate_observation(s: &mut Subject) {
    s.provider.start(1_000).expect("starts");
    (s.observe)(s.provider.as_mut(), &peer(P1), 1_000);
    let _ = s.provider.drain_events(1_000, 32);
    // The same observation again: correct, whatever it chooses to emit.
    (s.observe)(s.provider.as_mut(), &peer(P1), 1_100);
    let again = s.provider.drain_events(1_100, 32);
    for event in &again {
        if let DiscoveryEvent::CandidateObserved { candidate } = event {
            assert!(
                candidate.validate().is_ok(),
                "{}: a duplicate must not produce an invalid candidate",
                s.name
            );
        }
    }
}

fn provider_handles_candidate_update(s: &mut Subject) {
    s.provider.start(1_000).expect("starts");
    (s.observe)(s.provider.as_mut(), &peer(P1), 1_000);
    let _ = s.provider.drain_events(1_000, 32);
    (s.observe)(s.provider.as_mut(), &peer(P2), 2_000);
    // Whatever it emits must stay valid and correctly attributed.
    for event in s.provider.drain_events(2_000, 32) {
        if let DiscoveryEvent::CandidateObserved { candidate } = event {
            assert!(candidate.validate().is_ok(), "{}", s.name);
            assert_eq!(candidate.source, s.provider.descriptor().name, "{}", s.name);
        }
    }
}

fn provider_expires_when_semantics_support_ttl(s: &mut Subject) {
    s.provider.start(1_000).expect("starts");
    (s.observe)(s.provider.as_mut(), &peer(P1), 1_000);
    let _ = s.provider.drain_events(1_000, 32);
    // Far in the future. A provider that models expiry may retract; one
    // that does not must simply stay correct rather than panicking.
    let supports = s.provider.descriptor().supports_expiry;
    let late = s.provider.drain_events(u64::from(u32::MAX), 32);
    for event in &late {
        if let DiscoveryEvent::CandidateExpired { source, .. } = event {
            assert!(
                supports,
                "{}: a provider that declares no expiry emitted one",
                s.name
            );
            assert_eq!(source, &s.provider.descriptor().name, "{}", s.name);
        }
    }
}

fn provider_rejects_or_ignores_invalid_address_safely(s: &mut Subject) {
    s.provider.start(1_000).expect("starts");
    // An oversized address through the hint path: refused or unsupported,
    // never accepted-and-emitted, and never a panic.
    let disposition = s.provider.add_hint(
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
        s.name
    );
    for event in s.provider.drain_events(1_000, 32) {
        if let DiscoveryEvent::CandidateObserved { candidate } = event {
            assert!(candidate.validate().is_ok(), "{}", s.name);
        }
    }
}

fn provider_survives_malformed_provider_input(s: &mut Subject) {
    s.provider.start(1_000).expect("starts");
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
        let _ = s.provider.add_hint(hint, 1_000);
    }
    let _ = s.provider.drain_events(1_000, 32);
}

fn provider_respects_state_bounds(s: &mut Subject) {
    s.provider.start(1_000).expect("starts");
    (s.observe)(s.provider.as_mut(), &peer(P1), 1_000);
    (s.observe)(s.provider.as_mut(), &peer(P2), 1_000);
    assert!(
        s.provider.drain_events(1_000, 1).len() <= 1,
        "{}: the CALLER sizes the batch",
        s.name
    );
    assert!(
        s.provider.drain_events(1_000, 0).is_empty(),
        "{}: a zero bound takes nothing",
        s.name
    );
}

fn provider_shutdown_is_idempotent_and_bounded(s: &mut Subject) {
    s.provider.start(1_000).expect("starts");
    s.provider.shutdown(2_000);
    s.provider.shutdown(2_001);
    assert_eq!(
        s.provider.health(),
        ProviderHealth::Unavailable,
        "{}: a stopped provider is unavailable",
        s.name
    );
}

fn provider_event_stream_closes_after_shutdown(s: &mut Subject) {
    s.provider.start(1_000).expect("starts");
    (s.observe)(s.provider.as_mut(), &peer(P1), 1_000);
    s.provider.shutdown(2_000);
    assert!(
        s.provider.drain_events(3_000, 32).is_empty(),
        "{}: the stream terminates deterministically",
        s.name
    );
    (s.observe)(s.provider.as_mut(), &peer(P2), 3_000);
    assert!(
        s.provider.drain_events(4_000, 32).is_empty(),
        "{}: and stays closed",
        s.name
    );
}

fn provider_does_not_own_connection_policy(s: &mut Subject) {
    // STRUCTURAL. `DiscoveryEvent` has no variant meaning "connect to
    // this peer" and this crate cannot reach a Swarm, a dial, or a
    // connection type — there is nothing for a provider to call. What is
    // checkable at runtime is that everything it emits is one of the
    // three advisory shapes.
    s.provider.start(1_000).expect("starts");
    (s.observe)(s.provider.as_mut(), &peer(P1), 1_000);
    for event in s.provider.drain_events(1_000, 32) {
        match event {
            DiscoveryEvent::CandidateObserved { .. }
            | DiscoveryEvent::CandidateExpired { .. }
            | DiscoveryEvent::HealthChanged { .. } => {}
        }
    }
}

fn provider_does_not_grant_trust(s: &mut Subject) {
    // Also structural: `interweave-discovery-api` does not depend on
    // `trust-api`, so no provider can name a trust type, and none of
    // these crates depends on it either. The runtime check is that a
    // candidate carries no authorization-shaped field — which the closed
    // schema guarantees, and `validate` enforces.
    s.provider.start(1_000).expect("starts");
    (s.observe)(s.provider.as_mut(), &peer(P1), 1_000);
    for event in s.provider.drain_events(1_000, 32) {
        if let DiscoveryEvent::CandidateObserved { candidate } = event {
            assert!(candidate.validate().is_ok(), "{}", s.name);
        }
    }
}

fn provider_failure_does_not_panic(s: &mut Subject) {
    // A second start is the failure every provider can be made to have.
    s.provider.start(1_000).expect("starts");
    assert_eq!(
        s.provider.start(1_001),
        Err(ProviderError::AlreadyStarted),
        "{}: a lifecycle failure is a value, not a panic",
        s.name
    );
}

/// The suite, as a list, so it can be run over any provider — including
/// the misbehaving one below.
type Check = (&'static str, fn(&mut Subject));

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
    for make in [cache_subject, static_subject, mdns_subject] {
        for (name, check) in SUITE {
            // A fresh subject per check: the guarantees are about a
            // provider's own lifecycle, and sharing one would let an
            // earlier check's shutdown decide a later one's result.
            let mut subject = make();
            let label = subject.name;
            check(&mut subject);
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
    for event in p.drain_events(0, 32) {
        if let DiscoveryEvent::CandidateObserved { candidate } = event {
            candidate.validate().expect("valid");
            assert_eq!(candidate.source, p.descriptor().name);
        }
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

fn misbehaving_subject(violation: Violation) -> Subject {
    Subject {
        name: "misbehaving",
        provider: Box::new(Misbehaving::new(violation)),
        observe: |_, _, _| {},
        _keep: None,
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
