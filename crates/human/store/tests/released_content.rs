// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `RETENTION.md` §8 (A 2026-10-07): released content is absent from every
//! file of the store -- the database and its write-ahead log -- after a
//! clean close, after the next open following an unclean one, and, while
//! the store is open, after the release returns (the store's bound is one
//! release).
//!
//! Measured the way SPIKE-008 found the gap: a byte search of the files for
//! a long TEST-ONLY marker. Every case also keeps a second marker, which
//! the search must find, or a search that finds nothing proves nothing.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use interweave_human_store::{AppMessageId, HumanStore, InboundOrigin, NewInbound, StoreOptions};
use interweave_transport_api::TransportIdentity;

const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
/// TEST-ONLY markers: long and unique, so no other byte run matches.
const RELEASED: &[u8] = b"RETENTION8-RELEASED-CONTENT-3e5a7c9b1d2f4a6c8e0b2d4f6a8c";
const KEPT: &[u8] = b"RETENTION8-KEPT-CONTROL-CONTENT-9c7a5e3b1f0d2b4f6d8a0c";

fn inbound(id: u32, payload: &[u8]) -> NewInbound {
    NewInbound {
        app_message_id: AppMessageId::parse(format!("{id:032x}")).expect("canonical id"),
        origin: InboundOrigin {
            peer: TransportIdentity::parse(PEER).expect("canonical peer"),
            endpoint: None,
            channel: None,
        },
        media_type: None,
        payload: payload.to_vec(),
        received_at: 2_000,
    }
}

fn wal(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push("-wal");
    PathBuf::from(p)
}

/// Whether `needle` is in the database file or its WAL (a missing WAL
/// holds nothing).
fn in_files(path: &Path, needle: &[u8]) -> (bool, bool) {
    let holds = |p: &Path| {
        std::fs::read(p).is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
    };
    (holds(path), holds(&wal(path)))
}

/// A store holding the released marker as an UNREAD message that has
/// reached the database file (written, then a close checkpointed it), and
/// the kept marker as a kept message. Returns the released row's id.
fn store_with_both(path: &Path) -> interweave_human_store::RowId {
    let mut store = HumanStore::open(path, StoreOptions::default()).expect("opens");
    let kept = store
        .commit_unread_inbound(&inbound(2, KEPT))
        .expect("kept commits");
    let read = store.mark_read(kept, 3_000).expect("read");
    store.keep(&read, 3_001).expect("kept");
    let released = store
        .commit_unread_inbound(&inbound(1, RELEASED))
        .expect("released commits");
    drop(store);
    assert!(
        in_files(path, RELEASED).0,
        "the setup: the released message reached the database file"
    );
    released
}

fn state(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("state").join("human.sqlite3")
}

#[test]
fn released_content_is_absent_from_both_files_after_a_clean_close() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = state(&dir);
    let row = store_with_both(&path);
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("reopens");
    store.mark_read(row, 4_000).expect("released");
    drop(store);
    assert_eq!(
        in_files(&path, RELEASED),
        (false, false),
        "released content left"
    );
    assert!(
        in_files(&path, KEPT).0,
        "the control: the search sees kept content"
    );
}

#[test]
fn released_content_leaves_both_files_before_the_release_returns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = state(&dir);
    let row = store_with_both(&path);
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("reopens");
    store.mark_read(row, 4_000).expect("released");
    // The store is still open: the bound is one release.
    assert_eq!(
        in_files(&path, RELEASED),
        (false, false),
        "released content left"
    );
    let control = in_files(&path, KEPT);
    assert!(
        control.0 || control.1,
        "the control: the search sees kept content"
    );
    drop(store);
}

#[test]
fn an_unclean_close_by_a_build_without_secure_delete_is_scrubbed_at_the_next_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = state(&dir);
    store_with_both(&path);
    // A build before `secure_delete`, ending uncleanly: it deletes the
    // released message with no zeroing, never checkpoints, and its store
    // carries no scrubbed mark.
    let legacy = rusqlite::Connection::open(&path).expect("raw open");
    legacy
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA secure_delete=OFF;
             DELETE FROM unread_inbound;
             DELETE FROM settings WHERE key = 'released_content_scrubbed';",
        )
        .expect("legacy delete");
    let (db, log) = in_files(&path, RELEASED);
    assert!(
        db || log,
        "the setup: the legacy delete left the content behind"
    );
    // No close: the process ends without checkpointing.
    std::mem::forget(legacy);

    let store = HumanStore::open(&path, StoreOptions::default()).expect("the next open");
    assert_eq!(
        in_files(&path, RELEASED),
        (false, false),
        "released content left"
    );
    let control = in_files(&path, KEPT);
    assert!(
        control.0 || control.1,
        "the control: the search sees kept content"
    );
    drop(store);
}

#[test]
fn unkept_content_leaves_both_files_before_the_unkeep_returns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = state(&dir);
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("opens");
    let kept = store
        .commit_unread_inbound(&inbound(2, KEPT))
        .expect("control commits");
    let read = store.mark_read(kept, 3_000).expect("read");
    store.keep(&read, 3_001).expect("kept");
    let row = store
        .commit_unread_inbound(&inbound(1, RELEASED))
        .expect("commits");
    let read = store.mark_read(row, 3_100).expect("read");
    let kept_row = store.keep(&read, 3_101).expect("kept");
    drop(store);
    assert!(
        in_files(&path, RELEASED).0,
        "the setup: kept content reached the file"
    );
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("reopens");
    store.unkeep(kept_row, 4_000).expect("unkept");
    assert_eq!(
        in_files(&path, RELEASED),
        (false, false),
        "unkept content left"
    );
    let control = in_files(&path, KEPT);
    assert!(
        control.0 || control.1,
        "the control: the search sees kept content"
    );
    drop(store);
}

#[test]
fn terminal_outbound_content_leaves_both_files_before_the_call_returns() {
    use interweave_human_core::retention::TerminalCause;
    use interweave_human_store::{NewOutbound, OutboundDestination};
    use interweave_transport_api::{DirectDestination, MessageId};
    let dir = tempfile::tempdir().expect("tempdir");
    let path = state(&dir);
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("opens");
    let kept = store
        .commit_unread_inbound(&inbound(2, KEPT))
        .expect("control commits");
    let read = store.mark_read(kept, 3_000).expect("read");
    store.keep(&read, 3_001).expect("kept");
    let row = store
        .commit_pending_outbound(&NewOutbound {
            app_message_id: AppMessageId::parse(format!("{:032x}", 9)).expect("id"),
            transport_message_id: MessageId::parse_hex(&format!("{:032x}", 0x99)).expect("id"),
            destination: OutboundDestination::Direct(DirectDestination::to_default(
                TransportIdentity::parse(PEER).expect("peer"),
            )),
            media_type: None,
            payload: RELEASED.to_vec(),
            created_at: 1_000,
        })
        .expect("pending commits");
    drop(store);
    assert!(
        in_files(&path, RELEASED).0,
        "the setup: pending content reached the file"
    );
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("reopens");
    store
        .transport_terminal(row, TerminalCause::Accepted)
        .expect("terminal");
    assert_eq!(
        in_files(&path, RELEASED),
        (false, false),
        "terminal content left"
    );
    let control = in_files(&path, KEPT);
    assert!(
        control.0 || control.1,
        "the control: the search sees kept content"
    );
    drop(store);
}

/// A process ending before its first checkpoint is the normal end of an
/// Android session: the schema and every message are still only in the
/// WAL, and the database file's header still says version 0. A release then
/// commits and the process dies before the store truncates the log. The
/// next open must not take that store for a fresh one (#214's review, F1).
#[test]
fn a_never_checkpointed_store_released_and_killed_is_scrubbed_at_the_next_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = state(&dir);
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("opens");
    let kept = store
        .commit_unread_inbound(&inbound(2, KEPT))
        .expect("control commits");
    let read = store.mark_read(kept, 3_000).expect("read");
    store.keep(&read, 3_001).expect("kept");
    // The truncate after that release checkpointed; start over from a
    // never-checkpointed state: a fresh store that only commits.
    drop(store);
    let path = dir.path().join("fresh").join("human.sqlite3");
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("opens");
    let kept = store
        .commit_unread_inbound(&inbound(2, KEPT))
        .expect("control commits");
    store
        .commit_unread_inbound(&inbound(1, RELEASED))
        .expect("released commits");
    let _ = kept;
    // Killed: no close, no checkpoint.
    std::mem::forget(store);
    let header = std::fs::read(&path).expect("db");
    assert!(
        header.len() < 100 || header[60..64] == [0, 0, 0, 0],
        "the setup: the database file's header still says version 0"
    );
    // The release commits through the WAL, and the process dies before the
    // truncate (a raw connection standing in for this build's own delete).
    let release = rusqlite::Connection::open(&path).expect("raw open");
    release
        .execute_batch(
            "PRAGMA secure_delete=ON;
             DELETE FROM unread_inbound WHERE payload = CAST('RETENTION8-RELEASED-CONTENT-3e5a7c9b1d2f4a6c8e0b2d4f6a8c' AS BLOB);",
        )
        .expect("release commits");
    std::mem::forget(release);
    assert!(
        in_files(&path, RELEASED).1,
        "the setup: the released content is in the WAL"
    );

    let store = HumanStore::open(&path, StoreOptions::default()).expect("the next open");
    assert_eq!(
        in_files(&path, RELEASED),
        (false, false),
        "released content left"
    );
    let control = in_files(&path, KEPT);
    assert!(
        control.0 || control.1,
        "the control: the search sees kept content"
    );
    drop(store);
}
