// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What the peer cache must do, and what it must never hold.
//!
//! The bounds are the point. A cache that quietly exceeded its caps
//! would still pass every functional test here, so each cap has a case
//! that drives past it and checks what was evicted — not merely that
//! something was.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;

use interweave_discovery_cache::{
    CacheHealth, CacheLimits, DEFAULT_TTL_MS, MAX_ADDRESSES_PER_PEER, MAX_CAPABILITIES_PER_PEER,
    PeerCache, ProtocolCapabilityObservation,
};
use interweave_transport_api::TransportIdentity;

const PEER_A: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
const PEER_B: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

fn peer(s: &str) -> TransportIdentity {
    TransportIdentity::parse(s).expect("canonical peer id")
}

fn empty(path: &Path) -> PeerCache {
    PeerCache::load(path, CacheLimits::default()).expect("load")
}

fn capability(family: &str, supported: bool, at: u64) -> ProtocolCapabilityObservation {
    ProtocolCapabilityObservation {
        protocol_family: family.to_owned(),
        wire_major: 1,
        network_hash: "abc123".to_owned(),
        role: "server".to_owned(),
        supported,
        observed_at_ms: at,
    }
}

#[test]
fn a_missing_file_is_a_normal_empty_cache_not_an_error() {
    // Absence is the expected state on a first run and after a user
    // clears it. Treating it as a failure would make "safe to delete"
    // untrue.
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = empty(&dir.path().join("peers.json"));
    assert!(cache.is_empty());
    assert_eq!(cache.health(), &CacheHealth::Healthy);
}

#[test]
fn a_corrupt_file_is_quarantined_and_the_cache_continues_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("peers.json");
    std::fs::write(&path, b"{ this is not json").expect("write garbage");

    let cache = empty(&path);
    assert!(cache.is_empty(), "a corrupt cache must not fail startup");
    let CacheHealth::Quarantined { quarantined_to, .. } = cache.health() else {
        panic!(
            "a corrupt file must be quarantined, got {:?}",
            cache.health()
        );
    };
    assert!(
        quarantined_to.exists(),
        "the bad file is kept for inspection, not deleted"
    );
    assert!(!path.exists(), "the bad file is moved out of the way");
}

#[test]
fn a_future_format_version_is_quarantined_rather_than_misread() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("peers.json");
    std::fs::write(&path, br#"{"version":99,"peers":[]}"#).expect("write");
    let cache = empty(&path);
    assert!(matches!(cache.health(), CacheHealth::Quarantined { .. }));
}

#[test]
fn a_successful_dial_round_trips_through_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("peers.json");
    let mut cache = empty(&path);
    cache.record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 1_000);
    cache.flush(1_000).expect("flush");

    let reloaded = empty(&path);
    let candidates = reloaded.candidates(2_000);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].peer_id.as_str(), PEER_A);
    assert!(candidates[0].addresses.contains("/ip4/10.0.0.1/tcp/4001"));
    assert_eq!(candidates[0].source, "peer-cache");
    assert_eq!(
        candidates[0].expires_at,
        Some(1_000 + DEFAULT_TTL_MS),
        "the TTL runs from the last success"
    );
}

#[test]
fn an_expired_record_is_ignored_on_read_without_being_written() {
    // Reading must not be a write path: a read that mutated would make
    // every lookup a disk write.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("peers.json");
    let mut cache = empty(&path);
    cache.record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 1_000);

    let expired_at = 1_000 + DEFAULT_TTL_MS;
    assert!(cache.candidates(expired_at - 1).len() == 1);
    assert!(
        cache.candidates(expired_at).is_empty(),
        "the record is ignored the moment it expires"
    );
    assert_eq!(cache.len(), 1, "but it is still present until compaction");

    assert_eq!(cache.compact(expired_at), 1);
    assert_eq!(cache.len(), 0);
}

#[test]
fn a_failure_is_recorded_without_shortening_the_ttl() {
    // A peer that is merely offline right now is exactly the peer whose
    // addresses are worth keeping until they expire on their own.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = empty(&dir.path().join("peers.json"));
    cache.record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 1_000);
    cache.record_failure(&peer(PEER_A), 2_000);

    let record = cache.peer(&peer(PEER_A), 3_000).expect("still cached");
    assert_eq!(record.last_failure_ms, Some(2_000));
    assert_eq!(record.last_success_ms, 1_000);
    assert_eq!(record.addresses.len(), 1, "a failure drops no addresses");
}

#[test]
fn the_address_cap_drops_the_least_recently_successful() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = empty(&dir.path().join("peers.json"));
    let p = peer(PEER_A);

    // Nine addresses, oldest first, into a cap of eight.
    for i in 0..=u64::try_from(MAX_ADDRESSES_PER_PEER).expect("small") {
        cache.record_success(&p, &format!("/ip4/10.0.0.{i}/tcp/4001"), 1_000 + i);
    }
    let record = cache.peer(&p, 2_000).expect("cached");
    assert_eq!(record.addresses.len(), MAX_ADDRESSES_PER_PEER);
    let kept: Vec<&str> = record
        .addresses
        .iter()
        .map(|a| a.address.as_str())
        .collect();
    assert!(
        !kept.contains(&"/ip4/10.0.0.0/tcp/4001"),
        "the least recently successful address is the one dropped"
    );
    assert!(kept.contains(&"/ip4/10.0.0.8/tcp/4001"));
}

#[test]
fn re_observing_an_address_refreshes_it_rather_than_duplicating_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = empty(&dir.path().join("peers.json"));
    let p = peer(PEER_A);
    cache.record_success(&p, "/ip4/10.0.0.1/tcp/4001", 1_000);
    cache.record_success(&p, "/ip4/10.0.0.1/tcp/4001", 9_000);

    let record = cache.peer(&p, 10_000).expect("cached");
    assert_eq!(record.addresses.len(), 1);
    assert_eq!(record.addresses[0].last_success_ms, 9_000);
}

#[test]
fn the_peer_cap_evicts_expired_records_before_live_ones() {
    // Evicting a working peer while an expired one sits in the file is
    // strictly worse, so freshness beats age.
    let dir = tempfile::tempdir().expect("tempdir");
    let limits = CacheLimits {
        max_peers: 1,
        ..CacheLimits::default()
    };
    let mut cache = PeerCache::load(&dir.path().join("peers.json"), limits).expect("load");

    // A is old enough to have expired; B is brand new but is inserted
    // second, so an insertion-ordered cap would evict B.
    cache.record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 1_000);
    let much_later = 1_000 + DEFAULT_TTL_MS + 1;
    cache.record_success(&peer(PEER_B), "/ip4/10.0.0.2/tcp/4001", much_later);

    assert_eq!(cache.len(), 1);
    assert!(
        cache.peer(&peer(PEER_B), much_later).is_some(),
        "the live peer must survive; the expired one is what goes"
    );
}

#[test]
fn a_fresh_capability_observation_supersedes_the_earlier_one() {
    // Including flipping supported from true to false, which is how a
    // peer that stopped running a Kademlia server stops being targeted.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = empty(&dir.path().join("peers.json"));
    let p = peer(PEER_A);
    cache.record_success(&p, "/ip4/10.0.0.1/tcp/4001", 1_000);

    cache.record_capability(&p, capability("interweave/kad", true, 1_000));
    cache.record_capability(&p, capability("interweave/kad", false, 2_000));

    let record = cache.peer(&p, 3_000).expect("cached");
    assert_eq!(record.capabilities.len(), 1, "superseded, not appended");
    assert!(!record.capabilities[0].supported);
    assert_eq!(record.capabilities[0].observed_at_ms, 2_000);
}

#[test]
fn a_capability_observation_without_a_record_is_dropped() {
    // Creating a record here would mint a reachability entry out of a
    // protocol fact, for a peer no successful dial ever reached.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = empty(&dir.path().join("peers.json"));
    cache.record_capability(&peer(PEER_A), capability("interweave/kad", true, 1_000));
    assert!(cache.is_empty());
}

#[test]
fn the_capability_cap_holds_across_positive_and_negative_observations() {
    // They share one budget: a peer that changes its mind must not be
    // able to double its footprint by generating one of each.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = empty(&dir.path().join("peers.json"));
    let p = peer(PEER_A);
    cache.record_success(&p, "/ip4/10.0.0.1/tcp/4001", 1_000);

    for i in 0..=u64::try_from(MAX_CAPABILITIES_PER_PEER).expect("small") {
        let supported = i % 2 == 0;
        cache.record_capability(&p, capability(&format!("family/{i}"), supported, 1_000 + i));
    }
    let record = cache.peer(&p, 2_000).expect("cached");
    assert_eq!(record.capabilities.len(), MAX_CAPABILITIES_PER_PEER);
    assert!(
        record
            .capabilities
            .iter()
            .all(|o| o.protocol_family != "family/0"),
        "the oldest observation is the one dropped"
    );
}

#[test]
fn capability_freshness_never_outlives_the_enclosing_record() {
    // A capability observed yesterday on a record that expired this
    // morning is evidence about a peer this cache has stopped vouching
    // for.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = empty(&dir.path().join("peers.json"));
    let p = peer(PEER_A);
    cache.record_success(&p, "/ip4/10.0.0.1/tcp/4001", 1_000);
    cache.record_capability(&p, capability("interweave/kad", true, 1_000));

    let expired_at = 1_000 + DEFAULT_TTL_MS;
    let record = cache.peer(&p, expired_at - 1).expect("still fresh");
    assert_eq!(
        record
            .fresh_capabilities(expired_at - 1, DEFAULT_TTL_MS)
            .len(),
        1
    );
    assert!(
        record
            .fresh_capabilities(expired_at, DEFAULT_TTL_MS)
            .is_empty(),
        "the capability expires with its record, not on its own schedule"
    );
}

#[test]
fn writes_are_debounced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("peers.json");
    let mut cache = empty(&path);

    cache.record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 0);
    assert!(
        cache.flush_if_due(0).expect("flush"),
        "the first write is due"
    );

    cache.record_success(&peer(PEER_B), "/ip4/10.0.0.2/tcp/4001", 100);
    assert!(
        !cache.flush_if_due(100).expect("flush"),
        "a burst of dials must not rewrite the file per connection"
    );
    assert!(
        cache
            .flush_if_due(interweave_discovery_cache::WRITE_DEBOUNCE_MS)
            .expect("flush"),
        "the write lands once the debounce elapses"
    );

    assert_eq!(empty(&path).len(), 2);
}

#[test]
fn a_clean_cache_is_not_rewritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = empty(&dir.path().join("peers.json"));
    assert!(!cache.write_due(1_000_000));
    assert!(!cache.flush_if_due(1_000_000).expect("flush"));
}

#[test]
fn the_persisted_file_holds_no_application_or_trust_state() {
    // The absences that make this file safe to delete. Checked against
    // the serialised bytes so an added field fails here rather than
    // being noticed in review.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("peers.json");
    let mut cache = empty(&path);
    let p = peer(PEER_A);
    cache.record_success(&p, "/ip4/10.0.0.1/tcp/4001", 1_000);
    cache.record_capability(&p, capability("interweave/kad", true, 1_000));
    cache.record_failure(&p, 1_500);
    cache.flush(2_000).expect("flush");

    let text = std::fs::read_to_string(&path).expect("read back");
    for forbidden in [
        "endpoint",
        "channel",
        "trust",
        "allowlist",
        "payload",
        "message",
        "membership",
        "presence",
        "private",
        "secret",
    ] {
        assert!(
            !text.to_ascii_lowercase().contains(forbidden),
            "the peer cache must not persist {forbidden:?}: {text}"
        );
    }
}

#[test]
fn the_write_is_atomic_and_leaves_no_temporary_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("peers.json");
    let mut cache = empty(&path);
    cache.record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 1_000);
    cache.flush(1_000).expect("flush");

    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "peers.json")
        .collect();
    assert!(leftovers.is_empty(), "unexpected files: {leftovers:?}");
}
