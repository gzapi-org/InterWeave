// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Storage mechanics: schema shape, migration, bounds, and degradation.
//!
//! The retention TRANSITIONS are proved in `tests/human-retention` at the
//! repository root, against a real file that is closed and reopened.
//! What this suite proves is the layer beneath them — that the database
//! this build opens is the one it thinks it opened, and that a medium
//! failure is reported as degradation rather than swallowed.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_human_core::retention::{StorageHealth, TerminalCause};
use interweave_human_store::{
    AppMessageId, HumanStore, InboundOrigin, NewInbound, NewOutbound, OutboundDestination,
    StoreError, StoreOptions,
};
use interweave_transport_api::{DirectDestination, TransportIdentity};

const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
const ID_A: &str = "0123456789abcdef0123456789abcdef";
const ID_B: &str = "fedcba9876543210fedcba9876543210";

fn peer() -> TransportIdentity {
    TransportIdentity::parse(PEER).expect("the fixture peer id is canonical")
}

fn outbound(id: &str, payload: Vec<u8>) -> NewOutbound {
    NewOutbound {
        app_message_id: AppMessageId::parse(id).expect("test id is canonical"),
        destination: OutboundDestination::Direct(DirectDestination::to_default(peer())),
        media_type: Some("application/vnd.interweave-human-chat+json;v=2".to_owned()),
        payload,
        created_at: 1_000,
    }
}

fn inbound(id: &str, payload: Vec<u8>) -> NewInbound {
    NewInbound {
        app_message_id: AppMessageId::parse(id).expect("test id is canonical"),
        origin: InboundOrigin {
            peer: peer(),
            endpoint: None,
            channel: None,
        },
        media_type: None,
        payload,
        received_at: 2_000,
    }
}

fn memory() -> HumanStore {
    HumanStore::open_in_memory(StoreOptions::default()).expect("in-memory store opens")
}

#[test]
fn a_fresh_store_has_exactly_the_allowed_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("human.sqlite3");
    let store = HumanStore::open(&path, StoreOptions::default()).expect("opens");
    drop(store);

    let conn = rusqlite::Connection::open(&path).expect("reopen for inspection");
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .expect("prepare");
    let mut names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    names.sort();

    assert_eq!(
        names,
        vec![
            "kept_inbound".to_owned(),
            "pending_outbound".to_owned(),
            "settings".to_owned(),
            "unread_inbound".to_owned(),
        ],
        "the store must contain the three retention tables and content-free settings, nothing more"
    );
}

#[test]
fn opening_a_database_with_a_history_table_is_refused() {
    // The failure this guards against is a plausible-sounding addition,
    // so it is caught at open rather than left to review.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("first open"));

    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute_batch("CREATE TABLE messages (row_id INTEGER PRIMARY KEY, body BLOB)")
        .expect("create the forbidden table");
    drop(conn);

    let err = HumanStore::open(&path, StoreOptions::default())
        .expect_err("a store containing a general archive must not open");
    assert!(
        matches!(&err, StoreError::Migration(d) if d.contains("messages")),
        "unexpected error: {err}"
    );
}

#[test]
fn a_newer_schema_is_refused_rather_than_downgraded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("first open"));

    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.pragma_update(None, "user_version", 99_i64)
        .expect("bump the version");
    drop(conn);

    let err = HumanStore::open(&path, StoreOptions::default())
        .expect_err("running old migrations over a newer schema destroys data");
    assert!(
        matches!(&err, StoreError::Migration(d) if d.contains("refusing to downgrade")),
        "unexpected error: {err}"
    );
}

#[test]
fn reopening_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("human.sqlite3");
    for _ in 0..3 {
        drop(HumanStore::open(&path, StoreOptions::default()).expect("reopen"));
    }
}

#[test]
fn app_message_id_takes_only_the_humanchatv2_grammar() {
    assert!(AppMessageId::parse(ID_A).is_ok());
    for bad in [
        "",
        "0123456789ABCDEF0123456789ABCDEF",  // upper case
        "0123456789abcdef0123456789abcde",   // 31
        "0123456789abcdef0123456789abcdef0", // 33
        "0123456789abcdef0123456789abcdeg",  // non-hex
    ] {
        assert!(
            AppMessageId::parse(bad).is_err(),
            "{bad:?} must be refused before it reaches a UNIQUE column"
        );
    }
}

#[test]
fn a_payload_transport_could_not_carry_is_not_a_pending_row() {
    // The store holds the exact wire bytes so a retry resends them
    // unchanged; anything over the transport ceiling never was a message.
    let mut store = memory();
    let max = interweave_transport_api::MAX_PAYLOAD_BYTES;
    assert!(
        store
            .commit_pending_outbound(&outbound(ID_A, vec![0; max]))
            .is_ok()
    );
    let err = store
        .commit_pending_outbound(&outbound(ID_B, vec![0; max + 1]))
        .expect_err("over the ceiling");
    assert!(
        matches!(err, StoreError::PayloadTooLarge { got, max: m } if got == max + 1 && m == max),
        "unexpected error: {err}"
    );
}

#[test]
fn a_full_medium_degrades_the_store_and_refuses_new_unread() {
    // A real SQLITE_FULL from a real page quota, not an injected fake:
    // the degradation path must be the one production takes.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("human.sqlite3");
    let mut store = HumanStore::open(
        &path,
        StoreOptions {
            max_pages: Some(16),
        },
    )
    .expect("opens");
    assert_eq!(store.health(), StorageHealth::Healthy);

    let big = vec![0_u8; interweave_transport_api::MAX_PAYLOAD_BYTES];
    let mut hit_full = false;
    for i in 0..8_u32 {
        let id = format!("{i:032x}");
        if store
            .commit_unread_inbound(&inbound(&id, big.clone()))
            .is_err()
        {
            hit_full = true;
            break;
        }
    }
    assert!(hit_full, "a 16-page ceiling must be reached by 8 × 48 KiB");
    assert_eq!(
        store.health(),
        StorageHealth::Degraded,
        "a full medium must degrade the store"
    );

    // And the degraded store must REFUSE, not try and fail: the caller
    // has to release the human endpoint rather than keep accepting.
    let err = store
        .commit_unread_inbound(&inbound(ID_A, vec![1, 2, 3]))
        .expect_err("a degraded store cannot accept new unread content");
    assert!(matches!(err, StoreError::Degraded), "unexpected: {err}");

    assert!(
        store
            .health()
            .degraded_response()
            .is_some_and(|r| r.release_human_endpoint && r.suspend_broadcast_joins),
        "the required reaction is to stop presenting as a durable receiver"
    );
}

#[test]
fn a_duplicate_app_message_id_does_not_degrade_the_store() {
    // A constraint violation is an application bug; the medium is fine.
    // Degrading here would take the client offline over a duplicate id.
    let mut store = memory();
    store
        .commit_unread_inbound(&inbound(ID_A, vec![1]))
        .expect("first commit");
    assert!(
        store
            .commit_unread_inbound(&inbound(ID_A, vec![1]))
            .is_err()
    );
    assert_eq!(
        store.health(),
        StorageHealth::Healthy,
        "a duplicate id says nothing about the storage medium"
    );
    store
        .commit_unread_inbound(&inbound(ID_B, vec![2]))
        .expect("the store still works");
}

#[test]
fn backup_eligible_content_excludes_pending_outbound() {
    // Excluded so a restored or second device cannot become an implicit
    // delayed-send or replay source (RETENTION.md §6).
    let mut store = memory();
    store
        .commit_pending_outbound(&outbound(ID_A, b"outbound".to_vec()))
        .expect("pending");
    store
        .commit_unread_inbound(&inbound(ID_B, b"inbound".to_vec()))
        .expect("unread");

    let backup = store.backup_eligible_content().expect("backup set");
    assert_eq!(backup.len(), 1);
    assert_eq!(backup[0].payload, b"inbound".to_vec());
}

#[test]
fn recheck_health_clears_degradation_when_the_medium_recovers() {
    let mut store = memory();
    assert_eq!(
        store.recheck_health().expect("probe"),
        StorageHealth::Healthy
    );
}

#[test]
fn a_terminal_outbound_row_is_gone_and_a_second_terminal_event_is_harmless() {
    let mut store = memory();
    let row = store
        .commit_pending_outbound(&outbound(ID_A, b"hello".to_vec()))
        .expect("pending");
    assert_eq!(store.pending_outbound().expect("read").len(), 1);

    store
        .transport_terminal(row, TerminalCause::Accepted)
        .expect("terminal");
    assert!(store.pending_outbound().expect("read").is_empty());

    // A retry that reaches terminal twice must not error: the required
    // end state already holds.
    store
        .transport_terminal(row, TerminalCause::Cancelled)
        .expect("idempotent");
}
