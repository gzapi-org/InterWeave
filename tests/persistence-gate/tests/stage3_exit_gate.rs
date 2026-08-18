// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Stage 3 exit gate: "persistence invariants survive process
//! restart and failure injection".
//!
//! Each store proves its own rules in its own suite. These are the
//! statements no single crate can make — a reader would otherwise have
//! to hold three packages in mind and trust that the seams between them
//! hold.
//!
//! Every case here builds a whole profile on disk, drops every handle,
//! and reopens from the paths alone. Reusing a live handle would test
//! the in-memory state, which is the thing a restart destroys.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;

use interweave_discovery_cache::{CacheLimits, PeerCache};
use interweave_human_core::retention::StorageHealth;
use interweave_human_store::{
    AppMessageId, HumanStore, InboundOrigin, NewInbound, NewOutbound, OutboundDestination,
    StoreOptions, TerminalCause,
};
use interweave_profile_config::{
    ProfilePaths, XdgRoots, create_private_dir, is_owner_only, write_atomic, write_private_atomic,
};
use interweave_transport_api::{DirectDestination, TransportIdentity};

const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
const OUTBOUND_ID: &str = "0123456789abcdef0123456789abcdef";
const INBOUND_ID: &str = "fedcba9876543210fedcba9876543210";
const CONFIG: &[u8] = b"schema_version: 2\nendpoints:\n  default_direct_endpoint: human\n";
// Test-only bytes. Not a key, not derived from one, and no private key
// exists for the PeerId above.
const FAKE_IDENTITY: &[u8] = b"test-only placeholder, not key material";

fn paths(base: &Path) -> ProfilePaths {
    ProfilePaths::resolve(
        "default",
        &XdgRoots {
            config_home: base.join("config"),
            data_home: base.join("data"),
            state_home: base.join("state"),
            cache_home: base.join("cache"),
            runtime_dir: Some(base.join("run")),
        },
    )
    .expect("resolve")
}

fn peer() -> TransportIdentity {
    TransportIdentity::parse(PEER).expect("canonical")
}

/// Build a complete profile on disk, then drop every handle.
fn write_whole_profile(p: &ProfilePaths) {
    create_private_dir(p.identity_dir()).expect("identity dir");
    write_private_atomic(&p.identity_file(), FAKE_IDENTITY).expect("identity");
    write_atomic(&p.config_file(), CONFIG).expect("config");

    let mut cache = PeerCache::load(&p.peer_cache_file(), CacheLimits::default()).expect("cache");
    cache.record_success(&peer(), "/ip4/10.0.0.1/tcp/4001", 1_000);
    cache.flush(1_000).expect("flush");

    let mut store = HumanStore::open(
        &p.state_dir().join("human.sqlite3"),
        StoreOptions::default(),
    )
    .expect("store");
    store
        .commit_pending_outbound(&NewOutbound {
            app_message_id: AppMessageId::parse(OUTBOUND_ID).expect("canonical"),
            destination: OutboundDestination::Direct(DirectDestination::to_default(peer())),
            media_type: None,
            payload: b"still sending".to_vec(),
            created_at: 1_000,
        })
        .expect("pending");
    store
        .commit_unread_inbound(&NewInbound {
            app_message_id: AppMessageId::parse(INBOUND_ID).expect("canonical"),
            origin: InboundOrigin {
                peer: peer(),
                endpoint: None,
                channel: None,
            },
            media_type: None,
            payload: b"not yet read".to_vec(),
            received_at: 2_000,
        })
        .expect("unread");
}

#[test]
fn a_whole_profile_reloads_from_its_paths_after_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    write_whole_profile(&p);

    // Everything above is dropped. What follows opens from paths alone.
    assert_eq!(std::fs::read(p.config_file()).expect("config"), CONFIG);
    assert_eq!(
        std::fs::read(p.identity_file()).expect("identity"),
        FAKE_IDENTITY
    );
    assert!(is_owner_only(&p.identity_file()).expect("mode"));

    let cache = PeerCache::load(&p.peer_cache_file(), CacheLimits::default()).expect("cache");
    assert_eq!(cache.candidates(2_000).len(), 1);

    let store = HumanStore::open(
        &p.state_dir().join("human.sqlite3"),
        StoreOptions::default(),
    )
    .expect("store");
    assert_eq!(store.pending_outbound().expect("pending").len(), 1);
    assert_eq!(store.unread_inbound().expect("unread").len(), 1);
}

#[test]
fn deleting_the_peer_cache_costs_a_cold_start_and_nothing_else() {
    // The claim that makes the cache safe to delete. It is only
    // meaningful if identity, configuration, and retained messages are
    // all still there afterwards — and those live in three packages, so
    // no single crate can state it.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    write_whole_profile(&p);

    std::fs::remove_file(p.peer_cache_file()).expect("delete the cache");
    // And the whole cache directory, the way a disk cleaner would.
    std::fs::remove_dir_all(p.cache_dir()).expect("delete the cache directory");

    let cache = PeerCache::load(&p.peer_cache_file(), CacheLimits::default()).expect("cache");
    assert!(cache.is_empty(), "the cache starts over, as designed");

    assert_eq!(
        std::fs::read(p.identity_file()).expect("identity"),
        FAKE_IDENTITY,
        "deleting the cache must not touch the identity key"
    );
    assert_eq!(std::fs::read(p.config_file()).expect("config"), CONFIG);

    let store = HumanStore::open(
        &p.state_dir().join("human.sqlite3"),
        StoreOptions::default(),
    )
    .expect("store");
    assert_eq!(
        store.pending_outbound().expect("pending").len(),
        1,
        "a message the human believes they sent must not vanish with a cache clear"
    );
    assert_eq!(store.unread_inbound().expect("unread").len(), 1);
}

#[test]
fn deleting_the_human_store_leaves_identity_and_configuration_intact() {
    // The other direction. Application retention is not part of transport
    // profile recovery, so losing it must not cost a PeerId.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    write_whole_profile(&p);

    std::fs::remove_dir_all(p.state_dir()).expect("delete state");

    assert_eq!(
        std::fs::read(p.identity_file()).expect("identity"),
        FAKE_IDENTITY
    );
    assert_eq!(std::fs::read(p.config_file()).expect("config"), CONFIG);
    let cache = PeerCache::load(&p.peer_cache_file(), CacheLimits::default()).expect("cache");
    assert_eq!(cache.candidates(2_000).len(), 1);
}

#[test]
fn a_corrupt_peer_cache_does_not_stop_a_profile_from_loading() {
    // Failure injection. The cache is the one store whose loss is
    // harmless, so it must be the one that cannot take startup down.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    write_whole_profile(&p);
    std::fs::write(p.peer_cache_file(), b"\x00\x01 not json at all").expect("corrupt it");

    let cache = PeerCache::load(&p.peer_cache_file(), CacheLimits::default()).expect("still loads");
    assert!(cache.is_empty());
    assert!(matches!(
        cache.health(),
        interweave_discovery_cache::CacheHealth::Quarantined { .. }
    ));

    let store = HumanStore::open(
        &p.state_dir().join("human.sqlite3"),
        StoreOptions::default(),
    )
    .expect("the human store is unaffected");
    assert_eq!(store.unread_inbound().expect("unread").len(), 1);
}

#[test]
fn a_full_human_store_degrades_without_touching_the_rest_of_the_profile() {
    // Failure injection at the store that matters. The client must stop
    // presenting itself as a durable receiver — and must NOT respond by
    // dropping identity, configuration, or the messages it already holds.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    create_private_dir(p.identity_dir()).expect("identity dir");
    write_private_atomic(&p.identity_file(), FAKE_IDENTITY).expect("identity");
    write_atomic(&p.config_file(), CONFIG).expect("config");

    let db = p.state_dir().join("human.sqlite3");
    std::fs::create_dir_all(p.state_dir()).expect("state dir");
    let mut store = HumanStore::open(
        &db,
        StoreOptions {
            max_pages: Some(64),
        },
    )
    .expect("store");

    let mut survivor = None;
    for i in 0..8_u32 {
        let new = NewInbound {
            app_message_id: AppMessageId::parse(format!("{i:032x}")).expect("canonical"),
            origin: InboundOrigin {
                peer: peer(),
                endpoint: None,
                channel: None,
            },
            media_type: None,
            payload: vec![0_u8; interweave_transport_api::MAX_PAYLOAD_BYTES],
            received_at: 2_000 + u64::from(i),
        };
        match store.commit_unread_inbound(&new) {
            Ok(_) => survivor = Some(i),
            Err(_) => break,
        }
    }
    assert!(survivor.is_some(), "at least one message must have landed");
    assert_eq!(store.health(), StorageHealth::Degraded);

    // Already-committed content is still there. Degrading is a refusal to
    // accept MORE, not an eviction of what was promised.
    assert!(!store.unread_inbound().expect("unread").is_empty());

    // And nothing else in the profile moved.
    assert_eq!(
        std::fs::read(p.identity_file()).expect("identity"),
        FAKE_IDENTITY
    );
    assert_eq!(std::fs::read(p.config_file()).expect("config"), CONFIG);
    assert!(is_owner_only(&p.identity_file()).expect("mode"));
}

#[test]
fn a_transport_terminal_outbound_is_gone_after_a_restart_while_the_rest_survives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    write_whole_profile(&p);
    let db = p.state_dir().join("human.sqlite3");

    {
        let mut store = HumanStore::open(&db, StoreOptions::default()).expect("store");
        let row = store.pending_outbound().expect("pending")[0].row_id;
        store
            .transport_terminal(row, TerminalCause::Accepted)
            .expect("terminal");
    }

    let store = HumanStore::open(&db, StoreOptions::default()).expect("reopen");
    assert!(store.pending_outbound().expect("pending").is_empty());
    assert_eq!(
        store.unread_inbound().expect("unread").len(),
        1,
        "the unread message is unaffected by the outbound transition"
    );
    let cache = PeerCache::load(&p.peer_cache_file(), CacheLimits::default()).expect("cache");
    assert_eq!(cache.candidates(2_000).len(), 1);
}
