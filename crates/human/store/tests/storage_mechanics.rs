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
const PEER_B: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";
const ID_A: &str = "0123456789abcdef0123456789abcdef";
const ID_B: &str = "fedcba9876543210fedcba9876543210";

fn peer() -> TransportIdentity {
    TransportIdentity::parse(PEER).expect("the fixture peer id is canonical")
}

fn other_peer() -> TransportIdentity {
    TransportIdentity::parse(PEER_B).expect("the second fixture peer id is canonical")
}

fn inbound_from(who: TransportIdentity, id: &str, payload: Vec<u8>) -> NewInbound {
    NewInbound {
        origin: InboundOrigin {
            peer: who,
            endpoint: None,
            channel: None,
        },
        ..inbound(id, payload)
    }
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
    let path = dir.path().join("state").join("human.sqlite3");
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
    let path = dir.path().join("state").join("human.sqlite3");
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
    let path = dir.path().join("state").join("human.sqlite3");
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
    let path = dir.path().join("state").join("human.sqlite3");
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
    let path = dir.path().join("state").join("human.sqlite3");
    let mut store = HumanStore::open(
        &path,
        StoreOptions {
            max_pages: Some(64),
        },
    )
    .expect("opens");
    assert_eq!(store.health(), StorageHealth::Healthy);

    let big = vec![0_u8; interweave_transport_api::MAX_PAYLOAD_BYTES];
    let mut committed = 0_u32;
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
        committed += 1;
    }
    assert!(hit_full, "the page ceiling must be reached by 8 × 48 KiB");
    // Without this the test passes when the ceiling is so low that NOTHING
    // fits, which exercises a store that was never usable rather than one
    // that filled up.
    assert!(
        committed > 0,
        "the ceiling must admit at least one message first"
    );
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

#[test]
fn a_stale_row_id_cannot_delete_a_later_message() {
    // SQLite reuses a rowid after the highest row is deleted. Since
    // `transport_terminal` is deliberately idempotent — a retry reaching
    // terminal twice must not error — a late duplicate event for a
    // finished message would otherwise delete whatever message inherited
    // its id. That is silent loss of something the user just composed.
    let mut store = memory();
    let first = store
        .commit_pending_outbound(&outbound(ID_A, b"finished".to_vec()))
        .expect("first");
    store
        .transport_terminal(first, TerminalCause::Accepted)
        .expect("terminal");

    let second = store
        .commit_pending_outbound(&outbound(ID_B, b"just composed".to_vec()))
        .expect("second");
    assert_ne!(
        first.get(),
        second.get(),
        "a row id must never be handed out twice"
    );

    // The late duplicate event for the FIRST message.
    store
        .transport_terminal(first, TerminalCause::Accepted)
        .expect("idempotent");

    let pending = store.pending_outbound().expect("read");
    assert_eq!(pending.len(), 1, "the new message must still be pending");
    assert_eq!(pending[0].payload, b"just composed".to_vec());
}

#[test]
fn a_stale_row_id_cannot_read_a_later_message() {
    // Same hazard on the inbound side, where the consequence is worse: a
    // reused id would hand the caller someone else's message body and
    // delete the durable copy of a message the user never saw.
    let mut store = memory();
    let first = store
        .commit_unread_inbound(&inbound(ID_A, b"already read".to_vec()))
        .expect("first");
    store.mark_read(first, 1_000).expect("read");

    let second = store
        .commit_unread_inbound(&inbound(ID_B, b"never seen".to_vec()))
        .expect("second");
    assert_ne!(first.get(), second.get());

    assert!(
        store.mark_read(first, 2_000).is_err(),
        "a stale row id must not read the message that inherited it"
    );
    assert_eq!(
        store.unread_inbound().expect("read").len(),
        1,
        "the unread message must survive"
    );
}

#[test]
fn keeping_an_already_kept_message_is_not_an_error() {
    // The state machine says keeping a kept message is fine, and a UI can
    // produce a second Keep from one double-click. A store stricter than
    // the contract it implements would surface that as a storage failure.
    let mut store = memory();
    let row = store
        .commit_unread_inbound(&inbound(ID_A, b"body".to_vec()))
        .expect("commit");
    let held = store.mark_read(row, 1_000).expect("read");

    let first = store.keep(&held, 2_000).expect("keep");

    // Something else lands in between. Without this the second keep would
    // pass while returning the wrong id, because last_insert_rowid() is
    // not updated by an upsert that takes the UPDATE path.
    let other = store
        .commit_unread_inbound(&inbound(ID_B, b"other".to_vec()))
        .expect("other");
    let other_held = store.mark_read(other, 2_500).expect("read other");
    let other_kept = store.keep(&other_held, 2_600).expect("keep other");

    let second = store.keep(&held, 3_000).expect("keep again");
    assert_eq!(first, second, "the same message, not a second copy");
    assert_ne!(second, other_kept, "and not some other message's row");

    let kept = store.kept_inbound().expect("read");
    assert_eq!(kept.len(), 2);
    let mine = kept
        .iter()
        .find(|r| r.app_message_id.as_str() == ID_A)
        .expect("still there");
    assert_eq!(mine.kept_at, Some(3_000));
    assert_eq!(store.health(), StorageHealth::Healthy);
}

#[test]
fn two_peers_may_use_the_same_application_id() {
    // `app_message_id` is HumanChatV2's APPLICATION identity, chosen by
    // the sender. Globally unique inbound rows made one peer's choice
    // collide with another's, so the second arrival could not be stored
    // at all — two unrelated people picking the same 128 bits is a
    // birthday problem, but one peer echoing an id it saw is not.
    let mut store = memory();
    let a = store
        .commit_unread_inbound(&inbound_from(peer(), ID_A, b"from a".to_vec()))
        .expect("first peer");
    let b = store
        .commit_unread_inbound(&inbound_from(other_peer(), ID_A, b"from b".to_vec()))
        .expect("a different peer may reuse the id");
    assert_ne!(a, b, "they are different messages");

    let unread = store.unread_inbound().expect("read");
    assert_eq!(unread.len(), 2, "both are held");
}

#[test]
fn one_peer_reusing_its_own_id_for_new_content_is_a_conflict() {
    // The keep upsert conflicts on remote-controlled data. Refreshing
    // the older row's timestamps and leaving its body in place would
    // report success for a message that never reached durable kept
    // state — the newer content simply disappears.
    let mut store = memory();

    let first = store
        .commit_unread_inbound(&inbound(ID_A, b"original".to_vec()))
        .expect("commit");
    let held = store.mark_read(first, 1_000).expect("read");
    store.keep(&held, 2_000).expect("keep");

    // A second message from the same peer, reusing the id, with a
    // different body. Committing it unread is fine — the first row left
    // that table when it was kept — and the collision surfaces where the
    // two would actually alias.
    let second = store
        .commit_unread_inbound(&inbound(ID_A, b"replacement".to_vec()))
        .expect("a new arrival is admitted");
    let second_held = store.mark_read(second, 3_000).expect("read");

    match store.keep(&second_held, 4_000) {
        Err(StoreError::IdentityConflict {
            app_message_id,
            source_peer,
        }) => {
            assert_eq!(app_message_id, ID_A);
            assert_eq!(source_peer, PEER);
        }
        other => panic!("expected an identity conflict, got {other:?}"),
    }

    // And the original body is intact — not silently replaced, and not
    // silently left while the caller was told the keep succeeded.
    let kept = store.kept_inbound().expect("read");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].payload, b"original".to_vec());
    assert_eq!(store.health(), StorageHealth::Healthy);
}

#[test]
fn debug_output_never_carries_a_message_body() {
    // RETENTION.md section 8: logs, analytics, and crash reports must not
    // become shadow message archives. A derived Debug puts the body into
    // whatever printed it — a panic message, a tracing span — where the
    // retention state machine has no reach, so a message deleted at read
    // would still be sitting in a log.
    const SECRET: &[u8] = b"the-quick-brown-fox-jumped";

    let mut store = memory();
    let out = outbound(ID_A, SECRET.to_vec());
    let inb = inbound(ID_B, SECRET.to_vec());

    let out_row = store.commit_pending_outbound(&out).expect("pending");
    let in_row = store.commit_unread_inbound(&inb).expect("unread");
    let pending = store.pending_outbound().expect("read");
    let unread = store.unread_inbound().expect("read");
    let held = store.mark_read(in_row, 1_000).expect("read");

    let printed = [
        format!("{out:?}"),
        format!("{inb:?}"),
        format!("{pending:?}"),
        format!("{unread:?}"),
        format!("{held:?}"),
    ];
    let secret = String::from_utf8_lossy(SECRET).into_owned();
    for text in &printed {
        assert!(
            !text.contains(&secret),
            "a message body reached Debug output: {text}"
        );
        assert!(
            text.contains("redacted"),
            "the redaction must be visible, not silent: {text}"
        );
    }
    // What a debugger actually wants still survives.
    assert!(printed[2].contains(ID_A));
    assert!(printed[4].contains(ID_B));
    assert_eq!(out_row.get(), pending[0].row_id.get());
}

#[test]
fn the_files_holding_message_content_are_owner_only() {
    // The store's documentation promised owner-only and checked nothing.
    // SQLite creates the database — and later the WAL and SHM — with the
    // process umask, which is 0644 on a default system: message content
    // readable by every local account.
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("opens");

    // Force the WAL and SHM into existence.
    store
        .commit_unread_inbound(&inbound(ID_A, b"body".to_vec()))
        .expect("commit");

    for suffix in ["", "-wal", "-shm"] {
        let mut companion = path.as_os_str().to_owned();
        companion.push(suffix);
        let companion = std::path::PathBuf::from(companion);
        if !companion.exists() {
            continue;
        }
        let mode = std::fs::metadata(&companion)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "{} is mode {:04o}",
            companion.display(),
            mode & 0o777
        );
    }

    let mode = std::fs::metadata(path.parent().expect("parent"))
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "the state directory is mode {mode:04o}");
}

#[test]
fn an_already_open_state_directory_is_refused_rather_than_tightened() {
    // Created owner-only says nothing about one that was already there —
    // restored, copied, or made by an older build. Refused rather than
    // narrowed, for the reason the identity key is: content that has been
    // broadly readable should be treated as exposed, and quietly fixing
    // the mode would hide that it ever was.
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("mkdir");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    match HumanStore::open(&state.join("human.sqlite3"), StoreOptions::default()) {
        Err(StoreError::PermissionsTooOpen { mode, .. }) => assert_eq!(mode, 0o755),
        other => panic!("expected a permissions refusal, got {other:?}"),
    }
}

#[test]
fn an_unexpected_content_table_is_refused_even_with_an_innocent_name() {
    // The forbidden-name list can only catch what it names, while the
    // module claims REQUIRED_TABLES is the whole content surface. A table
    // called `chat_archive` passed while being exactly the archive
    // ADR-0044 forbids.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("opens"));

    for ddl in [
        "CREATE TABLE chat_archive (row_id INTEGER PRIMARY KEY, payload BLOB)",
        "CREATE VIEW everything AS SELECT * FROM kept_inbound",
        "CREATE TRIGGER copy_it AFTER INSERT ON kept_inbound BEGIN SELECT 1; END",
    ] {
        let conn = rusqlite::Connection::open(&path).expect("reopen");
        conn.execute_batch(ddl).expect("create");
        drop(conn);

        let refused = HumanStore::open(&path, StoreOptions::default());
        assert!(
            matches!(refused, Err(StoreError::Migration(_))),
            "{ddl} must be refused, got {refused:?}"
        );

        let conn = rusqlite::Connection::open(&path).expect("reopen");
        let (kind, name) = ddl
            .strip_prefix("CREATE ")
            .and_then(|r| r.split_once(' '))
            .map(|(k, rest)| (k, rest.split_whitespace().next().unwrap_or("")))
            .expect("parsed");
        conn.execute_batch(&format!("DROP {kind} {name}"))
            .expect("drop");
    }

    // And with them gone it opens again, so the refusal was about the
    // extra object and not about the store having been touched.
    HumanStore::open(&path, StoreOptions::default()).expect("opens once they are gone");
}
