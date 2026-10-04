// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The read pairs (STATE.md `read_pairs`; architect-cto's Q5 ruling, relay
//! seq 11163; plan section 18 (5)), against a real file reopened: a copy of
//! a message read here and not kept, arriving after a restart, is not
//! unread again -- and the record that makes it so holds no content and
//! stays bounded.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_human_retention_tests::{INBOUND_BODY, PEER, unread_inbound};
use interweave_human_store::{
    AppMessageId, HumanStore, InboundOrigin, NewInbound, READ_PAIR_CAP, StoreError, StoreOptions,
};
use interweave_transport_api::{EndpointId, TransportIdentity};

fn open(path: &std::path::Path) -> HumanStore {
    HumanStore::open(path, StoreOptions::default()).expect("opens")
}

fn copy_with(payload: &[u8]) -> NewInbound {
    NewInbound {
        payload: payload.to_vec(),
        ..unread_inbound()
    }
}

#[test]
fn a_copy_of_a_message_read_and_not_kept_is_not_unread_after_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    let mut store = open(&path);
    let row = store
        .commit_unread_inbound(&unread_inbound())
        .expect("commit");
    store.mark_read(row, 1_700_000_010_000).expect("read");
    drop(store);

    let mut store = open(&path);
    assert!(
        matches!(
            store.commit_unread_inbound(&unread_inbound()),
            Err(StoreError::AlreadyRead)
        ),
        "the late copy is the same message"
    );
    assert!(store.unread_inbound().expect("unread").is_empty());
    // And a copy carrying the same id with different text is the same
    // message too: the id is the sender's claim (Q5).
    assert!(matches!(
        store.commit_unread_inbound(&copy_with(b"different words")),
        Err(StoreError::AlreadyRead)
    ));
    // The facade skips a duplicate rather than failing: AlreadyRead is one.
    assert!(StoreError::AlreadyRead.is_duplicate());
}

#[test]
fn the_pair_is_the_origin_so_another_peer_or_endpoint_reusing_the_id_is_admitted() {
    let mut store = HumanStore::open_in_memory(StoreOptions::default()).expect("store");
    let row = store
        .commit_unread_inbound(&unread_inbound())
        .expect("commit");
    store.mark_read(row, 1_000).expect("read");

    let another_endpoint = NewInbound {
        origin: InboundOrigin {
            endpoint: Some(EndpointId::parse("phone").expect("endpoint")),
            ..unread_inbound().origin
        },
        ..unread_inbound()
    };
    store
        .commit_unread_inbound(&another_endpoint)
        .expect("another route of the same peer is another message");

    let other_peer =
        TransportIdentity::parse("12D3KooWQYhTNQdmr3ArTeUHRYzFg94BKyTkoWBDWez9kSCVe2Xo")
            .expect("peer");
    assert_ne!(other_peer.as_str(), PEER);
    let another_peer = NewInbound {
        origin: InboundOrigin {
            peer: other_peer,
            ..unread_inbound().origin
        },
        ..unread_inbound()
    };
    store
        .commit_unread_inbound(&another_peer)
        .expect("another peer reusing the id is another message");
    assert_eq!(store.unread_inbound().expect("unread").len(), 2);
}

#[test]
fn removing_keep_records_the_pair_too() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    let mut store = open(&path);
    let row = store
        .commit_unread_inbound(&unread_inbound())
        .expect("commit");
    let held = store.mark_read(row, 1_000).expect("read");
    let kept = store.keep(&held, 2_000).expect("keep");
    store.unkeep(kept, 3_000).expect("unkeep");
    drop(store);

    let mut store = open(&path);
    assert!(matches!(
        store.commit_unread_inbound(&unread_inbound()),
        Err(StoreError::AlreadyRead)
    ));
    let conn = rusqlite::Connection::open(&path).expect("raw");
    let at: i64 = conn
        .query_row("SELECT at FROM read_pairs", [], |r| r.get(0))
        .expect("one pair");
    assert_eq!(
        at, 3_000,
        "re-recorded at the unkeep, so it counts as the newest"
    );
}

#[test]
fn the_pairs_are_bounded_and_the_oldest_goes_first() {
    let mut store = HumanStore::open_in_memory(StoreOptions::default()).expect("store");
    let id = |n: usize| {
        AppMessageId::parse(format!("{:032x}", 0xa000_0000_u128 + n as u128)).expect("id")
    };
    let message = |n: usize| NewInbound {
        app_message_id: id(n),
        ..unread_inbound()
    };
    for n in 0..=READ_PAIR_CAP {
        let row = store.commit_unread_inbound(&message(n)).expect("commit");
        store.mark_read(row, 1_000).expect("read");
    }
    // The first pair went when the cap-plus-first was recorded; the second
    // is still held.
    store
        .commit_unread_inbound(&message(0))
        .expect("the oldest pair was evicted: its copy is admitted");
    assert!(matches!(
        store.commit_unread_inbound(&message(1)),
        Err(StoreError::AlreadyRead)
    ));
}

#[test]
fn the_pairs_hold_no_content_and_a_column_that_could_is_refused_at_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    let mut store = open(&path);
    let row = store
        .commit_unread_inbound(&unread_inbound())
        .expect("commit");
    store.mark_read(row, 1_000).expect("read");
    drop(store);

    let conn = rusqlite::Connection::open(&path).expect("raw");
    let stmt = conn.prepare("SELECT * FROM read_pairs").expect("select");
    let columns: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|c| (*c).to_owned())
        .collect();
    assert_eq!(
        columns,
        [
            "pair_id",
            "app_message_id",
            "source_peer",
            "source_endpoint",
            "channel_id",
            "at",
            // Generated from the two above, for the key: nothing new.
            "source_endpoint_key",
            "channel_key"
        ]
    );
    let body = String::from_utf8_lossy(INBOUND_BODY);
    let raw: String = conn
        .query_row(
            "SELECT app_message_id || source_peer || IFNULL(source_endpoint, '') || \
                    IFNULL(channel_id, '') || at FROM read_pairs",
            [],
            |r| r.get(0),
        )
        .expect("the pair");
    assert!(!raw.contains(body.as_ref()), "no body in a pair");
    drop(stmt);

    // A column that could hold content, added behind the store's back:
    // the shape guard refuses the file at the next open.
    conn.execute("ALTER TABLE read_pairs ADD COLUMN payload BLOB", [])
        .expect("widen");
    drop(conn);
    assert!(HumanStore::open(&path, StoreOptions::default()).is_err());
}
