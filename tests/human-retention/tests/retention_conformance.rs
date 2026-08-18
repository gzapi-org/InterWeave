// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The ADR-0044 / `RETENTION.md` §9 conformance suite.
//!
//! Every case that claims content SURVIVES uses a real file, and the
//! process that wrote it is killed with `abort()` rather than closed —
//! see `crash_writer`. A drop-and-reopen would prove the store survives
//! a polite shutdown, which is not what §7 claims.
//!
//! # Coverage of the fourteen cases, including what is NOT proved here
//!
//! | § 9 case | here |
//! |---|---|
//! | 1 outbound committed before first send attempt | yes — durability at return, observed from a second connection |
//! | 2 direct `AcceptedV2` deletes the pending copy | yes |
//! | 3 failed/no-route/timeout stays pending | yes |
//! | 4 broadcast publication deletes the pending copy | yes |
//! | 5 inbound committed unread before presentation | yes — same durability-at-return argument |
//! | 6 unread inbound survives restart | yes — across a real crash |
//! | 7 read without Keep deletes durable content | yes |
//! | 8 Keep after read makes it durable | yes |
//! | 9 sender/payload cannot set Keep | yes |
//! | 10 removing Keep deletes durable content | yes |
//! | 11 terminal outbound and read-unkept vanish across restart | yes — across a real crash |
//! | 12 backup includes only unread/kept inbound | yes |
//! | 13 Android system backup excludes the store | **no** — a packaging property (`allowBackup`), provable only with the Android manifest in Stage 17 |
//! | 14 storage full degrades rather than claiming durability | yes |
//!
//! Cases 1 and 5 are ordering claims about a client that does not exist
//! yet. What is proved here is the store's half: when the commit call
//! returns, the row is durable and visible to an INDEPENDENT connection,
//! so a client that calls transport afterwards satisfies the contract.
//! The client's half belongs to the stage that builds the client.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

use interweave_human_core::retention::StorageHealth;
use interweave_human_retention_tests::{
    INBOUND_BODY, INBOUND_ID, OUTBOUND_BODY, OUTBOUND_ID, pending_outbound, unread_inbound,
};
use interweave_human_store::{
    HumanStore, OutboundDestination, StoreError, StoreOptions, TerminalCause,
};
use interweave_transport_api::ChannelId;

/// Run `crash_writer` against `path` and require that it really died.
///
/// A writer that exited normally would invalidate every conclusion drawn
/// from what the file contains, so the exit status is checked rather than
/// assumed.
fn crash_after(path: &Path, scenario: &str) {
    let status = Command::new(env!("CARGO_BIN_EXE_crash_writer"))
        .arg(path)
        .arg(scenario)
        .status()
        .expect("crash_writer runs");
    assert!(
        !status.success(),
        "crash_writer must abort, not exit cleanly: {status:?}"
    );
}

fn reopen(path: &Path) -> HumanStore {
    HumanStore::open(path, StoreOptions::default()).expect("the store reopens after a crash")
}

fn memory() -> HumanStore {
    HumanStore::open_in_memory(StoreOptions::default()).expect("in-memory store")
}

// -------------------------------------------------------------------
// Cases 1 and 5 — durable at return
// -------------------------------------------------------------------

#[test]
fn case_1_outbound_is_durable_before_the_call_returns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("human.sqlite3");
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("opens");
    store
        .commit_pending_outbound(&pending_outbound())
        .expect("commit");

    // A SECOND connection, not this store's. If the row is visible here
    // the transaction is committed, so a client that calls transport on
    // the next line cannot lose the message to a crash in between.
    let observer = rusqlite::Connection::open(&path).expect("independent reader");
    let count: i64 = observer
        .query_row(
            "SELECT count(*) FROM pending_outbound WHERE app_message_id = ?1",
            [OUTBOUND_ID],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(count, 1, "the pending row must be committed, not buffered");
}

#[test]
fn case_5_inbound_is_durable_before_the_call_returns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("human.sqlite3");
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("opens");
    store
        .commit_unread_inbound(&unread_inbound())
        .expect("commit");

    let observer = rusqlite::Connection::open(&path).expect("independent reader");
    let count: i64 = observer
        .query_row(
            "SELECT count(*) FROM unread_inbound WHERE app_message_id = ?1",
            [INBOUND_ID],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(
        count, 1,
        "the unread row must be committed before the user is notified"
    );
}

// -------------------------------------------------------------------
// Cases 2, 3, 4 — the outbound machine
// -------------------------------------------------------------------

#[test]
fn case_2_accepted_v2_deletes_the_durable_pending_copy() {
    let mut store = memory();
    let row = store
        .commit_pending_outbound(&pending_outbound())
        .expect("commit");
    store
        .transport_terminal(row, TerminalCause::Accepted)
        .expect("terminal");
    assert!(
        store.pending_outbound().expect("read").is_empty(),
        "AcceptedV2 is the transport-terminal event for retention"
    );
}

#[test]
fn case_3_a_failed_attempt_leaves_the_message_pending() {
    // Deleting here would lose content the human believes they sent, and
    // surviving an ambiguous failure is the whole reason it is durable.
    let mut store = memory();
    let row = store
        .commit_pending_outbound(&pending_outbound())
        .expect("commit");
    store
        .record_attempt(row, 1_700_000_005_000)
        .expect("attempt");
    store
        .record_attempt(row, 1_700_000_006_000)
        .expect("attempt");

    let pending = store.pending_outbound().expect("read");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attempts, 2);
    assert_eq!(
        pending[0].payload, OUTBOUND_BODY,
        "a retry resends the stored bytes unchanged (ADR-0050)"
    );
    assert_eq!(
        pending[0].app_message_id.as_str(),
        OUTBOUND_ID,
        "a retry reuses the same app_message_id"
    );
}

#[test]
fn case_4_broadcast_publication_deletes_the_pending_copy() {
    let mut store = memory();
    let mut new = pending_outbound();
    new.destination = OutboundDestination::Broadcast(
        ChannelId::parse("interweave.test.channel").expect("canonical channel id"),
    );
    let row = store.commit_pending_outbound(&new).expect("commit");

    let reloaded = store.pending_outbound().expect("read");
    assert!(
        matches!(reloaded[0].destination, OutboundDestination::Broadcast(_)),
        "a broadcast row must not read back as a direct send to nobody"
    );

    // Terminal because broadcast has no per-recipient acknowledgement —
    // NOT because anyone received it. The type keeps the two apart so a
    // UI cannot render this as "delivered".
    store
        .transport_terminal(row, TerminalCause::Published)
        .expect("published");
    assert!(store.pending_outbound().expect("read").is_empty());
}

// -------------------------------------------------------------------
// Cases 7, 8, 9, 10 — the inbound machine
// -------------------------------------------------------------------

#[test]
fn case_7_reading_without_keep_deletes_the_durable_copy() {
    let mut store = memory();
    let row = store
        .commit_unread_inbound(&unread_inbound())
        .expect("commit");
    let held = store.mark_read(row, 1_700_000_010_000).expect("read");

    assert!(
        store.unread_inbound().expect("read").is_empty(),
        "the durable unread copy goes at read, not at some later cleanup"
    );
    assert!(store.kept_inbound().expect("read").is_empty());
    assert_eq!(
        held.payload(),
        INBOUND_BODY,
        "the content stays in memory so the conversation still renders"
    );
}

#[test]
fn case_8_keep_after_read_makes_the_message_durable_again() {
    let mut store = memory();
    let row = store
        .commit_unread_inbound(&unread_inbound())
        .expect("commit");
    let held = store.mark_read(row, 1_700_000_010_000).expect("read");
    store.keep(&held, 1_700_000_011_000).expect("keep");

    let kept = store.kept_inbound().expect("read");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].payload, INBOUND_BODY);
    assert_eq!(kept[0].read_at, Some(1_700_000_010_000));
    assert_eq!(kept[0].kept_at, Some(1_700_000_011_000));
}

#[test]
fn case_9_nothing_a_sender_controls_can_produce_a_kept_row() {
    // The payload here is the most direct attempt available: content that
    // asks to be kept. It cannot work, because `keep` accepts only a
    // `ReadEphemeral` and the ONLY thing that mints one is a local
    // `mark_read`. There is no parameter for this to reach.
    let mut store = memory();
    let mut hostile = unread_inbound();
    hostile.payload = br#"{"v":2,"kind":"text","keep":true,"retain":"forever"}"#.to_vec();
    let row = store.commit_unread_inbound(&hostile).expect("commit");

    assert!(
        store.kept_inbound().expect("read").is_empty(),
        "arriving cannot make content kept"
    );

    // And reading it does not either — Keep is a separate local act.
    let held = store.mark_read(row, 1_700_000_010_000).expect("read");
    assert!(
        store.kept_inbound().expect("read").is_empty(),
        "reading is not keeping"
    );

    // Only the receiver's own action, on a handle only the local read
    // produced, reaches the durable table.
    store
        .keep(&held, 1_700_000_011_000)
        .expect("receiver keeps");
    assert_eq!(store.kept_inbound().expect("read").len(), 1);
}

#[test]
fn case_10_removing_keep_deletes_the_durable_copy_immediately() {
    let mut store = memory();
    let row = store
        .commit_unread_inbound(&unread_inbound())
        .expect("commit");
    let held = store.mark_read(row, 1_700_000_010_000).expect("read");
    let kept = store.keep(&held, 1_700_000_011_000).expect("keep");

    store.unkeep(kept).expect("unkeep");
    assert!(
        store.kept_inbound().expect("read").is_empty(),
        "deletion is immediate, not deferred to a cleanup pass"
    );
}

// -------------------------------------------------------------------
// Cases 6 and 11 — across a real crash
// -------------------------------------------------------------------

#[test]
fn case_6_pending_outbound_and_unread_inbound_survive_a_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("human.sqlite3");
    crash_after(&path, "durable");

    let store = reopen(&path);
    let pending = store.pending_outbound().expect("read");
    assert_eq!(pending.len(), 1, "pending outbound must survive");
    assert_eq!(pending[0].payload, OUTBOUND_BODY);
    assert_eq!(pending[0].app_message_id.as_str(), OUTBOUND_ID);

    let unread = store.unread_inbound().expect("read");
    assert_eq!(unread.len(), 1, "unread inbound must survive");
    assert_eq!(unread[0].payload, INBOUND_BODY);
    assert_eq!(unread[0].app_message_id.as_str(), INBOUND_ID);
}

#[test]
fn case_11_terminal_outbound_and_read_unkept_inbound_are_gone_after_a_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("human.sqlite3");
    crash_after(&path, "ephemeral");

    let store = reopen(&path);
    assert!(
        store.pending_outbound().expect("read").is_empty(),
        "transport-terminal outbound content does not survive"
    );
    assert!(
        store.unread_inbound().expect("read").is_empty(),
        "read content is deleted at read, so there is nothing to survive"
    );
    assert!(
        store.kept_inbound().expect("read").is_empty(),
        "the crashed process never kept it, and no other route exists"
    );

    // The content is not hiding in some other table either. This is the
    // assertion that would catch an archive added behind the API.
    let observer = rusqlite::Connection::open(&path).expect("independent reader");
    for table in ["pending_outbound", "unread_inbound", "kept_inbound"] {
        let rows: i64 = observer
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .expect("count");
        assert_eq!(rows, 0, "{table} still holds content after the crash");
    }
}

#[test]
fn case_11b_a_row_id_outlives_its_row_and_cannot_resurrect_the_content() {
    // Why this matters for the restart case: a caller keeps holding a
    // `RowId` after the row is gone, and that is the value a naive design
    // would let it re-read. It cannot. The only handle that carries
    // content is `ReadEphemeral`, which lives in memory and is not
    // serializable — so after a restart a read-unkept message has neither
    // a row nor a handle, and `keep` has nothing to be called with. The
    // case disappears rather than needing a runtime refusal.
    let mut store = memory();
    let row = store
        .commit_unread_inbound(&unread_inbound())
        .expect("commit");
    let held = store.mark_read(row, 1_700_000_010_000).expect("read");
    drop(held);

    let err = store
        .mark_read(row, 1_700_000_020_000)
        .expect_err("the row was deleted at read");
    assert!(matches!(err, StoreError::NoSuchRow), "unexpected: {err}");
}

// -------------------------------------------------------------------
// Cases 12 and 14
// -------------------------------------------------------------------

#[test]
fn case_12_backup_includes_only_unread_and_kept_inbound() {
    let mut store = memory();
    store
        .commit_pending_outbound(&pending_outbound())
        .expect("pending");
    store
        .commit_unread_inbound(&unread_inbound())
        .expect("unread");

    // One unread, one kept, one pending outbound.
    let mut second = unread_inbound();
    second.app_message_id =
        interweave_human_store::AppMessageId::parse("11111111111111111111111111111111")
            .expect("canonical");
    second.payload = b"kept later".to_vec();
    let second_row = store.commit_unread_inbound(&second).expect("unread");
    let held = store
        .mark_read(second_row, 1_700_000_010_000)
        .expect("read");
    store.keep(&held, 1_700_000_011_000).expect("keep");

    let backup = store.backup_eligible_content().expect("backup set");
    let bodies: Vec<&[u8]> = backup.iter().map(|r| r.payload.as_slice()).collect();
    assert!(bodies.contains(&INBOUND_BODY), "unread inbound is eligible");
    assert!(
        bodies.contains(&&b"kept later"[..]),
        "kept inbound is eligible"
    );
    assert!(
        !bodies.contains(&OUTBOUND_BODY),
        "pending outbound is excluded so a restored device cannot become a \
         delayed-send or replay source"
    );
    assert_eq!(backup.len(), 2);
}

#[test]
fn case_14_a_full_store_degrades_rather_than_claiming_durability() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("human.sqlite3");
    let mut store = HumanStore::open(
        &path,
        StoreOptions {
            max_pages: Some(64),
        },
    )
    .expect("opens");

    let mut committed = 0_u32;
    for i in 0..8_u32 {
        let mut new = unread_inbound();
        new.app_message_id =
            interweave_human_store::AppMessageId::parse(format!("{i:032x}")).expect("canonical");
        new.payload = vec![0_u8; interweave_transport_api::MAX_PAYLOAD_BYTES];
        if store.commit_unread_inbound(&new).is_err() {
            break;
        }
        committed += 1;
    }
    assert!(committed < 8, "the page ceiling must be reached");
    // A ceiling so low that nothing fits would exercise a store that was
    // never usable rather than one that filled up — and `committed < 8`
    // alone is satisfied by zero.
    assert!(
        committed > 0,
        "the ceiling must admit at least one message first"
    );
    assert_eq!(store.health(), StorageHealth::Degraded);

    // The required reaction: stop presenting as a durable receiver.
    let response = store
        .health()
        .degraded_response()
        .expect("a degraded store owes a reaction");
    assert!(response.release_human_endpoint);
    assert!(response.suspend_broadcast_joins);
    assert!(response.surface_to_user);

    // And it must REFUSE rather than silently accept and lose.
    let err = store
        .commit_unread_inbound(&unread_inbound())
        .expect_err("a degraded store cannot accept new human delivery");
    assert!(matches!(err, StoreError::Degraded), "unexpected: {err}");
}
