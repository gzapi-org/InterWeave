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
    CacheError, CacheHealth, CacheLimits, CacheLimitsBuilder, DEFAULT_TTL_MS, InvalidCacheLimits,
    MAX_ADDRESS_BYTES, MAX_ADDRESSES_PER_PEER, MAX_CACHE_FILE_BYTES, MAX_CAPABILITIES_PER_PEER,
    MAX_LABEL_BYTES, MAX_PEERS, PeerCache, ProtocolCapabilityObservation,
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
    cache
        .record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("within the bounded format");
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
    cache
        .record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("within the bounded format");

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
    cache
        .record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("within the bounded format");
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
        cache
            .record_success(&p, &format!("/ip4/10.0.0.{i}/tcp/4001"), 1_000 + i)
            .expect("within the bounded format");
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
    cache
        .record_success(&p, "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("within the bounded format");
    cache
        .record_success(&p, "/ip4/10.0.0.1/tcp/4001", 9_000)
        .expect("within the bounded format");

    let record = cache.peer(&p, 10_000).expect("cached");
    assert_eq!(record.addresses.len(), 1);
    assert_eq!(record.addresses[0].last_success_ms, 9_000);
}

#[test]
fn the_peer_cap_evicts_expired_records_before_live_ones() {
    // Evicting a working peer while an expired one sits in the file is
    // strictly worse, so freshness beats age.
    let dir = tempfile::tempdir().expect("tempdir");
    let limits = CacheLimitsBuilder {
        max_peers: 1,
        ..Default::default()
    }
    .build()
    .expect("one peer is a narrowing");
    let mut cache = PeerCache::load(&dir.path().join("peers.json"), limits).expect("load");

    // A is old enough to have expired; B is brand new but is inserted
    // second, so an insertion-ordered cap would evict B.
    cache
        .record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("within the bounded format");
    let much_later = 1_000 + DEFAULT_TTL_MS + 1;
    cache
        .record_success(&peer(PEER_B), "/ip4/10.0.0.2/tcp/4001", much_later)
        .expect("within the bounded format");

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
    cache
        .record_success(&p, "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("within the bounded format");

    cache
        .record_capability(&p, capability("interweave/kad", true, 1_000))
        .expect("within the bounded format");
    cache
        .record_capability(&p, capability("interweave/kad", false, 2_000))
        .expect("within the bounded format");

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
    cache
        .record_capability(&peer(PEER_A), capability("interweave/kad", true, 1_000))
        .expect("within the bounded format");
    assert!(cache.is_empty());
}

#[test]
fn the_capability_cap_holds_across_positive_and_negative_observations() {
    // They share one budget: a peer that changes its mind must not be
    // able to double its footprint by generating one of each.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = empty(&dir.path().join("peers.json"));
    let p = peer(PEER_A);
    cache
        .record_success(&p, "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("within the bounded format");

    for i in 0..=u64::try_from(MAX_CAPABILITIES_PER_PEER).expect("small") {
        let supported = i % 2 == 0;
        cache
            .record_capability(&p, capability(&format!("family/{i}"), supported, 1_000 + i))
            .expect("within the bounded format");
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
    cache
        .record_success(&p, "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("within the bounded format");
    cache
        .record_capability(&p, capability("interweave/kad", true, 1_000))
        .expect("within the bounded format");

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

    cache
        .record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 0)
        .expect("within the bounded format");
    assert!(
        cache.flush_if_due(0).expect("flush"),
        "the first write is due"
    );

    cache
        .record_success(&peer(PEER_B), "/ip4/10.0.0.2/tcp/4001", 100)
        .expect("within the bounded format");
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
    cache
        .record_success(&p, "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("within the bounded format");
    cache
        .record_capability(&p, capability("interweave/kad", true, 1_000))
        .expect("within the bounded format");
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
    cache
        .record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("within the bounded format");
    cache.flush(1_000).expect("flush");

    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "peers.json")
        .collect();
    assert!(leftovers.is_empty(), "unexpected files: {leftovers:?}");
}

/// Everything a `String`/`Vec` shape cannot say about itself.
///
/// The load path checked JSON syntax and a version number and then
/// inserted whatever it found. Both say the file is roughly the right
/// shape; neither says a peer id is canonical, an address is bounded, or
/// a count is inside the limits this cache advertises — so a restored,
/// corrupted, or locally replaced file could allocate far more than the
/// runtime limits imply, on state that is explicitly disposable.
#[test]
fn a_file_outside_the_bounded_format_is_quarantined_not_loaded() {
    let dir = tempfile::tempdir().expect("temp dir");
    let limits = CacheLimits::default();

    let cases: Vec<(&str, String)> = vec![
        (
            "a non-canonical peer id",
            format!(
                r#"{{"version":1,"peers":[{}]}}"#,
                record_json("not-a-peer-id", 1, 1, &[], &[])
            ),
        ),
        (
            "an address past its byte bound",
            format!(
                r#"{{"version":1,"peers":[{}]}}"#,
                record_json(PEER_A, 1, 1, &["x".repeat(257)], &[])
            ),
        ),
        (
            "an empty address",
            format!(
                r#"{{"version":1,"peers":[{}]}}"#,
                record_json(PEER_A, 1, 1, &[String::new()], &[])
            ),
        ),
        (
            "more addresses than the cache retains",
            format!(
                r#"{{"version":1,"peers":[{}]}}"#,
                record_json(
                    PEER_A,
                    1,
                    1,
                    &(0..=MAX_ADDRESSES_PER_PEER)
                        .map(|i| format!("/ip4/10.0.0.{i}/tcp/1"))
                        .collect::<Vec<_>>(),
                    &[]
                )
            ),
        ),
        (
            "more capability observations than the cache retains",
            format!(
                r#"{{"version":1,"peers":[{}]}}"#,
                record_json(
                    PEER_A,
                    1,
                    1,
                    &[],
                    &(0..=MAX_CAPABILITIES_PER_PEER)
                        .map(|i| format!("fam{i}"))
                        .collect::<Vec<_>>()
                )
            ),
        ),
        (
            "a capability label past its byte bound",
            format!(
                r#"{{"version":1,"peers":[{}]}}"#,
                record_json(PEER_A, 1, 1, &[], &["f".repeat(129)])
            ),
        ),
        (
            "a record that last succeeded before it first did",
            format!(
                r#"{{"version":1,"peers":[{}]}}"#,
                record_json(PEER_A, 500, 100, &[], &[])
            ),
        ),
    ];

    for (what, body) in cases {
        let path = dir.path().join(format!("{}.json", what.replace(' ', "-")));
        std::fs::write(&path, &body).expect("write");
        let cache = PeerCache::load(&path, limits).expect("load reports rather than fails");
        assert!(
            matches!(cache.health(), CacheHealth::Quarantined { .. }),
            "{what} must be quarantined, health was {:?}",
            cache.health()
        );
        assert_eq!(cache.len(), 0, "{what} must not be partially loaded");
    }
}

#[test]
fn a_file_larger_than_the_format_is_never_read_into_memory() {
    // The ceiling is derived from the format's own worst case, so a file
    // above it cannot be a valid cache — and the size is checked before
    // the bytes are read, because this is exactly the state a restore or
    // a local edit can replace.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("huge.json");

    let mut body = String::from(r#"{"version":1,"peers":[]}"#);
    body.push_str(&" ".repeat(usize::try_from(MAX_CACHE_FILE_BYTES).unwrap_or(usize::MAX)));
    std::fs::write(&path, &body).expect("write");

    let cache =
        PeerCache::load(&path, CacheLimits::default()).expect("load reports rather than fails");
    assert!(
        matches!(cache.health(), CacheHealth::Quarantined { .. }),
        "an over-size file must be quarantined, health was {:?}",
        cache.health()
    );
}

#[test]
fn a_valid_file_still_loads() {
    // The bound must not have become a refusal to load anything: every
    // check above has to pass for a file this cache itself wrote.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("peers.json");

    let mut cache = PeerCache::load(&path, CacheLimits::default()).expect("empty");
    cache
        .record_success(&peer(PEER_A), "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("within the bounded format");
    cache.flush(1_000_000).expect("writes");

    let reloaded = PeerCache::load(&path, CacheLimits::default()).expect("loads");
    assert!(
        matches!(reloaded.health(), CacheHealth::Healthy),
        "a file this cache wrote must load: {:?}",
        reloaded.health()
    );
    assert_eq!(reloaded.len(), 1);
}

fn record_json(
    peer_id: &str,
    first: u64,
    last: u64,
    addresses: &[String],
    families: &[String],
) -> String {
    let addrs: Vec<String> = addresses
        .iter()
        .map(|a| format!(r#"{{"address":"{a}","last_success_ms":1}}"#))
        .collect();
    let caps: Vec<String> = families
        .iter()
        .map(|f| {
            format!(
                r#"{{"protocol_family":"{f}","wire_major":1,"network_hash":"h","role":"server","supported":true,"observed_at_ms":1}}"#
            )
        })
        .collect();
    format!(
        r#"{{"peer_id":"{peer_id}","addresses":[{}],"first_success_ms":{first},"last_success_ms":{last},"capabilities":[{}]}}"#,
        addrs.join(","),
        caps.join(",")
    )
}

#[test]
fn a_cache_full_of_legal_records_fits_under_its_own_ceiling() {
    // The ceiling was hand-derived once and undercounted a legal peer by
    // a third, so `flush` could write a perfectly legal file that the
    // next `load` quarantined — the cache deleting its own contents on
    // restart for no reason a user could see.
    //
    // Measured with the real encoder rather than recomputed here. A test
    // that repeated the arithmetic would agree with a wrong constant.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("peers.json");
    let limits = CacheLimits::default();

    let mut cache = PeerCache::load(&path, limits).expect("empty");
    let who = peer(PEER_A);

    // One peer at every documented maximum.
    // The WORST legal characters, not comfortable ones: `"` and `\`
    // each encode as two JSON bytes, so a record of them is twice the
    // size of the same record made of letters. A maximal test built from
    // `a` would measure the easy case and miss the ceiling entirely.
    let worst = |n: usize| "\"\\".repeat(n / 2);
    for i in 0..MAX_ADDRESSES_PER_PEER {
        cache
            .record_success(&who, &format!("{}{i:04}", worst(252)), 1_000)
            .expect("within the bounded format");
    }
    for i in 0..MAX_CAPABILITIES_PER_PEER {
        cache
            .record_capability(
                &who,
                ProtocolCapabilityObservation {
                    protocol_family: format!("{}{i:04}", "f".repeat(120)),
                    wire_major: u32::MAX,
                    network_hash: "h".repeat(128),
                    role: "r".repeat(128),
                    supported: true,
                    observed_at_ms: u64::MAX,
                },
            )
            .expect("within the bounded format");
    }
    cache.flush(1_000_000).expect("writes");

    let one_peer = std::fs::metadata(&path).expect("stat").len();
    let full_cache = one_peer * MAX_PEERS as u64;
    assert!(
        full_cache <= MAX_CACHE_FILE_BYTES,
        "a full cache serializes to ~{full_cache} bytes but the ceiling is \
         {MAX_CACHE_FILE_BYTES}; `flush` would write a legal file that `load` quarantines"
    );

    // And it really does come back, rather than passing the arithmetic
    // and failing the round trip.
    let reloaded = PeerCache::load(&path, limits).expect("loads");
    assert!(
        matches!(reloaded.health(), CacheHealth::Healthy),
        "a maximal record must load: {:?}",
        reloaded.health()
    );
    assert_eq!(reloaded.len(), 1);
}

#[test]
fn nothing_the_cache_accepts_can_produce_a_file_it_refuses() {
    // The load path validated every record and the write path validated
    // nothing, so a caller could record a 257-byte address, persist it
    // successfully, and have the cache quarantine its own file on the
    // next start — discarding everything it held because of a value it
    // had already agreed to store.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("peers.json");
    let limits = CacheLimits::default();
    let mut cache = PeerCache::load(&path, limits).expect("empty");
    let who = peer(PEER_A);

    // Refused on the way in, rather than on the way back.
    assert!(matches!(
        cache.record_success(&who, &"x".repeat(257), 1_000),
        Err(CacheError::OutOfBounds { got: 257, .. })
    ));
    assert!(matches!(
        cache.record_success(&who, "", 1_000),
        Err(CacheError::OutOfBounds { got: 0, .. })
    ));

    // A control character passes every LENGTH check and encodes as six
    // JSON bytes, so a cache of them serializes to three times the
    // ceiling — `flush` succeeding and the next `load` quarantining. The
    // character set is part of the size bound, not a separate opinion.
    assert!(matches!(
        cache.record_success(&who, "/ip4/10.0.0.1/tcp/\u{0}4001", 1_000),
        Err(CacheError::OutOfBounds { .. })
    ));
    assert!(matches!(
        cache.record_success(&who, "/ip4/10.0.0.1/tcp/caf\u{e9}", 1_000),
        Err(CacheError::OutOfBounds { .. })
    ));

    cache
        .record_success(&who, "/ip4/10.0.0.1/tcp/4001", 1_000)
        .expect("a legal address");
    for (family, hash, role) in [
        ("f".repeat(129), "h".to_owned(), "r".to_owned()),
        ("f".to_owned(), "h".repeat(129), "r".to_owned()),
        ("f".to_owned(), "h".to_owned(), "r".repeat(129)),
    ] {
        assert!(
            matches!(
                cache.record_capability(
                    &who,
                    ProtocolCapabilityObservation {
                        protocol_family: family,
                        wire_major: 1,
                        network_hash: hash,
                        role,
                        supported: true,
                        observed_at_ms: 1_000,
                    },
                ),
                Err(CacheError::OutOfBounds { got: 129, .. })
            ),
            "each label is bounded, not merely one of them"
        );
    }

    // What did get in round-trips, which is the whole invariant: every
    // file `flush` writes is one `load` accepts.
    cache.flush(1_000_000).expect("writes");
    let reloaded = PeerCache::load(&path, limits).expect("loads");
    assert!(
        matches!(reloaded.health(), CacheHealth::Healthy),
        "the cache must accept its own output: {:?}",
        reloaded.health()
    );
    assert_eq!(reloaded.len(), 1);
}

#[test]
fn limits_may_narrow_the_frozen_format_and_never_widen_it() {
    // THE LOOP THIS CLOSES. The runtime honoured whatever limits it was
    // handed, while `load` measured the file against
    // MAX_CACHE_FILE_BYTES, which is computed from the FROZEN constants
    // and not from the instance's limits. So a caller asking for more
    // peers than the format holds got a cache that accepted the
    // records, serialized them, flushed an ordinary file -- and
    // quarantined that same file on the next start. The cache deleted
    // its own contents on restart, with nothing to show for it but a
    // size complaint about a file it had written itself.
    for (proposed, want) in [
        (
            CacheLimitsBuilder {
                max_peers: MAX_PEERS + 1,
                ..Default::default()
            },
            InvalidCacheLimits::PeersOutOfRange,
        ),
        (
            CacheLimitsBuilder {
                max_addresses_per_peer: MAX_ADDRESSES_PER_PEER + 1,
                ..Default::default()
            },
            InvalidCacheLimits::AddressesPerPeerOutOfRange,
        ),
        (
            CacheLimitsBuilder {
                max_capabilities_per_peer: MAX_CAPABILITIES_PER_PEER + 1,
                ..Default::default()
            },
            InvalidCacheLimits::CapabilitiesPerPeerOutOfRange,
        ),
        (
            CacheLimitsBuilder {
                max_peers: 0,
                ..Default::default()
            },
            InvalidCacheLimits::PeersOutOfRange,
        ),
        (
            CacheLimitsBuilder {
                ttl_ms: 0,
                ..Default::default()
            },
            InvalidCacheLimits::ZeroTtl,
        ),
    ] {
        assert_eq!(
            proposed.build(),
            Err(want),
            "the disk format is not this type's to widen"
        );
    }

    // Narrowing is the whole point and stays legal.
    let narrowed = CacheLimitsBuilder {
        max_peers: 4,
        max_addresses_per_peer: 2,
        max_capabilities_per_peer: 1,
        ..Default::default()
    }
    .build()
    .expect("fewer than the format allows is a narrowing");
    assert_eq!(narrowed.max_peers(), 4);

    // The frozen format itself narrows nothing and must survive its own
    // validator -- otherwise `Default`, which bypasses the builder,
    // would be minting a value the builder refuses.
    assert_eq!(
        CacheLimitsBuilder::default().build(),
        Ok(CacheLimits::default())
    );
}

#[test]
fn a_full_cache_serializes_inside_what_a_load_will_read() {
    // Every other bound in the crate is on a VALUE; the one in `flush`
    // is on the result, and it is what makes "a cache never writes a
    // file it cannot read" a checked statement rather than an argued
    // one. The derivation behind MAX_CACHE_FILE_BYTES has been wrong
    // once already -- it undercounted a legal record by a third, and
    // then did not count JSON escaping.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("peers.json");
    let mut cache = PeerCache::load(&path, CacheLimits::default()).expect("load");

    // A worst case the public API can actually reach: every peer at the
    // address and capability caps, with every string at its own bound.
    let address = format!("/x/{}", "a".repeat(MAX_ADDRESS_BYTES - 3));
    let label = "b".repeat(MAX_LABEL_BYTES);
    for i in 0..MAX_PEERS {
        let p = peer(&format!(
            "Qm{}",
            format!("{i:044}").replace('0', "a")[..44].to_owned()
        ));
        for a in 0..MAX_ADDRESSES_PER_PEER {
            let mut addr = address.clone();
            addr.replace_range(3..4, &char::from(b'a' + a as u8).to_string());
            cache
                .record_success(&p, &addr, 1_000)
                .expect("within the bounded format");
        }
        for c in 0..MAX_CAPABILITIES_PER_PEER / 2 {
            // Every string at its own ceiling, and distinct, so the
            // records do not merge and the cap is genuinely filled.
            let mut family = label.clone();
            family.replace_range(0..2, &format!("{c:02}"));
            cache
                .record_capability(
                    &p,
                    ProtocolCapabilityObservation {
                        protocol_family: family,
                        wire_major: 1,
                        network_hash: label.clone(),
                        role: label.clone(),
                        supported: c % 2 == 0,
                        observed_at_ms: 1_000,
                    },
                )
                .expect("within the bounded format");
        }
    }

    cache.flush(2_000).expect("a legal cache must publish");
    let written = std::fs::metadata(&path).expect("written").len();
    assert!(
        written <= MAX_CACHE_FILE_BYTES,
        "flushed {written} bytes against a {MAX_CACHE_FILE_BYTES}-byte load ceiling"
    );

    // And it loads back, which is the property the ceiling is for.
    let reloaded = PeerCache::load(&path, CacheLimits::default()).expect("loads");
    assert_eq!(reloaded.len(), MAX_PEERS, "no record was quarantined");
}
