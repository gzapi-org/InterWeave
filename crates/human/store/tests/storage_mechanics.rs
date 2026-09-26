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

use std::os::unix::fs::PermissionsExt;

use interweave_human_core::retention::{StorageHealth, TerminalCause};
use interweave_human_store::{
    AppMessageId, HumanStore, InboundOrigin, NewInbound, NewOutbound, OutboundDestination,
    PageLimits, PageLimitsError, StoreError, StoreOptions,
};
use interweave_transport_api::{
    ChannelId, DirectDestination, EndpointId, MediaType, TransportIdentity,
};

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

fn inbound_via(endpoint: &str, id: &str, payload: Vec<u8>) -> NewInbound {
    NewInbound {
        origin: InboundOrigin {
            peer: peer(),
            endpoint: Some(EndpointId::parse(endpoint).expect("test endpoint is canonical")),
            channel: None,
        },
        ..inbound(id, payload)
    }
}

fn inbound_on(channel: &str, id: &str, payload: Vec<u8>) -> NewInbound {
    NewInbound {
        origin: InboundOrigin {
            peer: peer(),
            endpoint: None,
            channel: Some(ChannelId::parse(channel).expect("test channel is canonical")),
        },
        ..inbound(id, payload)
    }
}

fn outbound(id: &str, payload: Vec<u8>) -> NewOutbound {
    NewOutbound {
        app_message_id: AppMessageId::parse(id).expect("test id is canonical"),
        destination: OutboundDestination::Direct(DirectDestination::to_default(peer())),
        media_type: Some(
            MediaType::parse("application/vnd.interweave-human-chat+json;v=2")
                .expect("a valid test media type"),
        ),
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
fn a_timestamp_the_store_cannot_represent_is_refused_not_saturated() {
    // Every public timestamp crossed into SQL as
    // `i64::try_from(v).unwrap_or(i64::MAX)`, so `i64::MAX`, `i64::MAX + 1`
    // and `u64::MAX` all stored the same value -- distinct accepted inputs
    // collapsing into one, which is a public invariant broken quietly
    // rather than a limit enforced. No clock reaches it, so a caller who
    // does is a bug or a hostile input and is better told. Review finding.
    let mut store = memory();
    let mut new = inbound("00000000000000000000000000000001", vec![1]);
    new.received_at = u64::MAX;
    match store.commit_unread_inbound(&new) {
        Err(StoreError::TimestampOutOfRange { field, got }) => {
            assert_eq!(field, "received_at");
            assert_eq!(got, u64::MAX);
        }
        other => panic!("an unrepresentable timestamp must be refused: {other:?}"),
    }
    assert!(
        store.unread_inbound().expect("read").is_empty(),
        "and nothing is stored under a saturated value"
    );

    // The largest value that IS representable still works, so the refusal
    // is a ceiling and not an off-by-one.
    let mut edge = inbound("00000000000000000000000000000002", vec![2]);
    edge.received_at = u64::try_from(i64::MAX).expect("i64::MAX fits in u64");
    store
        .commit_unread_inbound(&edge)
        .expect("the largest representable timestamp is accepted");
    assert_eq!(store.unread_inbound().expect("read").len(), 1);
}

#[test]
fn a_zero_record_ceiling_is_refused_rather_than_ending_the_enumeration() {
    // It did not page, it TERMINATED. The query fetches `max_records + 1`
    // to tell a full page from a finished one, so a zero ceiling fetched
    // one row; the first row of a page is emitted unconditionally, so it
    // went out; no second row was ever seen, so the page reported no
    // continuation. A backup walker reads that as "this table is done"
    // and moves on -- emitting the first unread record and silently
    // skipping every record after it. Durable-data omission with no
    // error anywhere, which is why the constructor refuses it rather
    // than the use sites clamping. Review finding.
    assert_eq!(
        PageLimits::new(0, 1024 * 1024),
        Err(PageLimitsError::ZeroRecords),
        "a zero record ceiling cannot page"
    );
    assert_eq!(
        PageLimits::new(256, 0),
        Err(PageLimitsError::ZeroBytes),
        "nor can a zero byte ceiling"
    );
    assert!(PageLimits::new(1, 1).is_ok(), "one of each is a page");

    // And the smallest legal ceiling really does walk every record
    // rather than stopping after the first -- which is the behaviour the
    // refused value only looked like.
    let mut store = memory();
    for i in 0..5u8 {
        store
            .commit_unread_inbound(&inbound(&format!("{i:032x}"), vec![i]))
            .expect("record");
    }
    let one = PageLimits::new(1, 1024 * 1024).expect("a paging budget");
    let mut seen = 0usize;
    let mut cursor = None;
    for _ in 0..32 {
        let page = store.unread_inbound_page(cursor, one).expect("page");
        seen += page.items.len();
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen, 5, "every record is reached one page at a time");
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

/// Review R5 on fa3eab8: `max_page_count` ATTEMPTS the change and
/// answers with the ceiling it set, so a successful pragma was taken as
/// the quota while SQLite enforced another. A ceiling it IGNORES -- zero,
/// which would leave no quota -- is refused at open. A ceiling below the
/// database's size is raised to that size -- looser than asked -- and
/// refusing it would leave no way back under it (#117's blind review F6):
/// that store opens degraded, its content readable, and stays degraded
/// until its content fits the quota asked for (its re-review, F2). THE
/// CONTROL is a ceiling above the database's size, which opens healthy.
#[test]
fn a_quota_sqlite_would_not_enforce_is_refused_and_a_tighter_one_opens_degraded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");

    let zero = HumanStore::open(&path, StoreOptions { max_pages: Some(0) });
    assert!(
        matches!(zero, Err(StoreError::QuotaNotApplied { requested: 0, .. })),
        "a zero quota is refused, not silently no quota: {zero:?}"
    );

    let pages = |path: &std::path::Path| {
        rusqlite::Connection::open(path)
            .expect("reopen")
            .pragma_query_value(None, "page_count", |row| row.get::<_, u32>(0))
            .expect("page_count")
    };
    drop(HumanStore::open(&path, StoreOptions::default()).expect("opens"));
    let empty = pages(&path);
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("opens");
    let big = vec![0_u8; interweave_transport_api::MAX_PAYLOAD_BYTES];
    for n in 1..=2 {
        store
            .commit_unread_inbound(&inbound(&format!("{n:032x}"), big.clone()))
            .expect("committed");
    }
    drop(store);
    let quota = empty + 2;
    assert!(
        pages(&path) > quota,
        "the database is above the quota about to be asked for"
    );

    let mut tight = HumanStore::open(
        &path,
        StoreOptions {
            max_pages: Some(quota),
        },
    )
    .expect("a ceiling below the database's size still opens");
    assert_eq!(
        tight.health(),
        StorageHealth::Degraded,
        "and says nothing new fits"
    );
    let unread = tight.unread_inbound().expect("readable");
    assert_eq!(unread.len(), 2, "its unread content can still be read");

    // ONE ROW RELEASED: its pages are free, so under the looser ceiling
    // the probe fits -- and an earlier version then reported Healthy
    // (#117's blind re-review, F2). The other row keeps the content above
    // the quota asked for, so the store stays degraded.
    tight.mark_read(unread[0].row_id, 1).expect("released");
    assert_eq!(
        tight
            .recheck_health()
            .expect("the probe fits in the freed pages"),
        StorageHealth::Degraded,
        "a successful probe is not health while the quota asked for is not in force"
    );
    // BOTH RELEASED: the file is compacted and the quota asked for is the
    // one in force.
    tight.mark_read(unread[1].row_id, 2).expect("released");
    assert_eq!(
        tight.recheck_health().expect("probed"),
        StorageHealth::Healthy
    );
    drop(tight);
    let ceiling = rusqlite::Connection::open(&path)
        .expect("reopen")
        .pragma_query_value(None, "page_count", |row| row.get::<_, u32>(0))
        .expect("page_count");
    assert!(
        ceiling <= quota,
        "compacted under the quota: {ceiling} pages"
    );

    let size = pages(&path);
    let fits = HumanStore::open(
        &path,
        StoreOptions {
            max_pages: Some(size + 64),
        },
    )
    .expect("a ceiling above the database's size is applied");
    assert_eq!(fits.health(), StorageHealth::Healthy);
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
    // THIS TEST USED TO DEGRADE NOTHING. It opened a fresh in-memory
    // store, which is Healthy already, and asserted it was Healthy —
    // proving that a healthy store stays healthy, while its name claims
    // it proves recovery. Deleting the assignment in `recheck_health`
    // outright would have passed it.
    //
    // So the store is really filled, really degraded, and really drained
    // before the probe is asked anything.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    let mut store = HumanStore::open(
        &path,
        StoreOptions {
            max_pages: Some(64),
        },
    )
    .expect("opens");

    let big = vec![0_u8; interweave_transport_api::MAX_PAYLOAD_BYTES];
    let mut rows = Vec::new();
    for i in 0..8_u32 {
        let id = format!("{i:032x}");
        match store.commit_unread_inbound(&inbound(&id, big.clone())) {
            Ok(row) => rows.push(row),
            Err(_) => break,
        }
    }
    assert_eq!(
        store.health(),
        StorageHealth::Degraded,
        "the medium must actually be full before recovery means anything"
    );
    assert!(!rows.is_empty(), "at least one message fit");

    // A FAILED INSERT LEAVES NO ROW. The statement that hit the quota
    // must not have half-written one: the table holds exactly what was
    // acknowledged, or a caller told "not stored" would find it stored.
    assert_eq!(
        store.unread_inbound().expect("read").len(),
        rows.len(),
        "the refused commit must have left nothing behind"
    );

    // The receiver reads its backlog, which is what frees the pages.
    // `mark_read` is deliberately not gated on degradation — a store that
    // could not be drained could never recover.
    for row in rows {
        store
            .mark_read(row, 1_000)
            .expect("reading is still possible");
    }

    assert_eq!(
        store.recheck_health().expect("probe"),
        StorageHealth::Healthy,
        "a drained medium must clear the degradation"
    );

    // And the store is usable again, not merely relabelled.
    store
        .commit_unread_inbound(&inbound(ID_A, b"after recovery".to_vec()))
        .expect("a recovered store accepts content again");
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
fn a_new_column_inside_a_permitted_table_is_a_retention_violation() {
    // The name allowlist catches the clumsy version and misses the one
    // that fits inside a permitted name:
    //
    //     ALTER TABLE unread_inbound ADD COLUMN archive_payload BLOB;
    //
    // is a second durable content surface, in the table whose whole
    // contract is that its body disappears when the message is read.
    // Under ADR-0044 an unknown column is not forward compatibility --
    // it is somewhere a body can be kept.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("opens"));

    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute_batch("ALTER TABLE unread_inbound ADD COLUMN archive_payload BLOB")
        .expect("the DDL itself is legal SQLite");
    drop(conn);

    let refused = HumanStore::open(&path, StoreOptions::default());
    assert!(
        matches!(refused, Err(StoreError::Migration(_))),
        "an added column must be refused, got {refused:?}"
    );

    // Dropping it restores the shape, so the refusal was about the
    // column and not about the database having been reopened.
    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute_batch("ALTER TABLE unread_inbound DROP COLUMN archive_payload")
        .expect("drop");
    drop(conn);
    HumanStore::open(&path, StoreOptions::default()).expect("opens once it is gone");
}

#[test]
fn the_unique_key_and_the_autoincrement_are_part_of_the_verified_shape() {
    // Both are invisible to a `SELECT type, name FROM sqlite_master`
    // check and both are load-bearing. The peer- AND endpoint-scoped
    // UNIQUE is what stops one peer colliding with another's message id
    // (migration 2) and one peer's two endpoints colliding with each
    // other (migration 3);
    // AUTOINCREMENT is what stops a deleted row's id being handed to
    // the next insert, which in a store that deletes constantly means
    // handing a caller someone else's body.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("opens"));

    // Rebuild `unread_inbound` with every column identical, the UNIQUE
    // widened back to the id alone, and AUTOINCREMENT dropped. A
    // column-only check would pass this.
    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute_batch(
        "
        PRAGMA writable_schema = OFF;
        ALTER TABLE unread_inbound RENAME TO unread_inbound_old;
        CREATE TABLE unread_inbound (
            row_id          INTEGER PRIMARY KEY,
            app_message_id  TEXT    NOT NULL UNIQUE,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL
        );
        DROP TABLE unread_inbound_old;
        ",
    )
    .expect("the rebuild is legal SQLite");
    drop(conn);

    let refused = HumanStore::open(&path, StoreOptions::default());
    assert!(
        matches!(refused, Err(StoreError::Migration(_))),
        "a widened unique key with no autoincrement must be refused, got {refused:?}"
    );
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

#[test]
fn bulk_reads_are_paged_with_record_and_byte_ceilings() {
    // The unpaged accessors materialize every matching payload, which
    // turns a bounded per-message design into an unbounded one-call
    // allocation. They stay, for the small case, but they refuse a
    // second page rather than growing quietly.
    let mut store = memory();
    for i in 0..12_u32 {
        let id = format!("{i:032x}");
        store
            .commit_unread_inbound(&NewInbound {
                received_at: 2_000 + u64::from(i),
                ..inbound(&id, vec![b'x'; 1024])
            })
            .expect("commit");
    }

    // A record ceiling.
    let limits = PageLimits::new(5, 1024 * 1024).expect("a paging budget");
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = store.unread_inbound_page(cursor, limits).expect("page");
        assert!(page.items.len() <= 5, "a page must respect max_records");
        assert!(!page.items.is_empty(), "a page with a cursor holds rows");
        seen.extend(
            page.items
                .iter()
                .map(|r| r.app_message_id.as_str().to_owned()),
        );
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 12, "every row is visited exactly once");
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 12, "and none is visited twice");

    // A byte ceiling, small enough that it binds before the record one.
    let tight = PageLimits::new(100, 2048).expect("a paging budget");
    let page = store.unread_inbound_page(None, tight).expect("page");
    assert!(
        page.items.len() <= 3,
        "the byte budget must bind: got {} rows",
        page.items.len()
    );
    assert!(page.next.is_some(), "and there is more to come");

    // A single payload over the whole budget still makes progress: the
    // first row of a page is always emitted, or the walk stalls forever.
    let stingy = PageLimits::new(100, 1).expect("a paging budget");
    let page = store.unread_inbound_page(None, stingy).expect("page");
    assert_eq!(page.items.len(), 1, "always at least one row");

    // And the convenience accessor says so rather than allocating.
    let refused = store.unread_inbound();
    assert!(
        refused.is_ok(),
        "twelve small rows are still the small case"
    );
}

#[test]
fn a_backup_walk_covers_both_tables_exactly_once() {
    // Unread and kept have independent row-id spaces, so a cursor that
    // did not name its table would let a resumed backup duplicate or skip
    // — and reporting the walk finished when unread runs out silently
    // loses the kept half.
    let mut store = memory();
    let mut expected = Vec::new();

    for i in 0..6_u32 {
        let id = format!("{i:032x}");
        expected.push(id.clone());
        let row = store
            .commit_unread_inbound(&NewInbound {
                received_at: 2_000 + u64::from(i),
                ..inbound(&id, b"body".to_vec())
            })
            .expect("commit");
        // Half of them get read and kept, so both tables are populated.
        if i % 2 == 0 {
            let held = store.mark_read(row, 3_000 + u64::from(i)).expect("read");
            store.keep(&held, 4_000 + u64::from(i)).expect("keep");
        }
    }

    let limits = PageLimits::new(2, 1024 * 1024).expect("a paging budget");
    let mut seen = Vec::new();
    let mut cursor = None;
    for _ in 0..64 {
        let page = store.backup_eligible_page(cursor, limits).expect("page");
        seen.extend(
            page.items
                .iter()
                .map(|r| r.app_message_id.as_str().to_owned()),
        );
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    seen.sort();
    expected.sort();
    assert_eq!(seen, expected, "every eligible message, exactly once");
}

#[test]
fn a_negative_stored_timestamp_is_corruption_and_not_a_zero() {
    // `unwrap_or(0)` looked like a harmless normalization. It is not:
    // SQL keeps ordering by the RAW value, so a negative `created_at`
    // sorts first and the cursor built from it carries zero -- and the
    // next page asks for rows after zero, walking straight past every
    // other malformed row. One corrupt value silently truncated the
    // result set, and the caller was handed a short list with no error.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("opens");
    store
        .commit_pending_outbound(&outbound(ID_A, b"body".to_vec()))
        .expect("queued");
    store
        .commit_pending_outbound(&outbound(ID_B, b"body".to_vec()))
        .expect("queued");
    drop(store);

    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute(
        "UPDATE pending_outbound SET created_at = -1 WHERE app_message_id = ?1",
        [ID_A],
    )
    .expect("corrupt one row");
    drop(conn);

    let store = HumanStore::open(&path, StoreOptions::default()).expect("reopens");
    match store.pending_outbound() {
        Err(StoreError::Corrupt(what)) => {
            assert!(what.contains("created_at"), "must name the field: {what}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }

    // And a negative counter, on the same footing.
    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute(
        "UPDATE pending_outbound SET created_at = 1000, attempts = -3 WHERE app_message_id = ?1",
        [ID_A],
    )
    .expect("corrupt the counter");
    drop(conn);
    let store = HumanStore::open(&path, StoreOptions::default()).expect("reopens");
    match store.pending_outbound() {
        Err(StoreError::Corrupt(what)) => assert!(what.contains("attempts"), "{what}"),
        other => panic!("expected Corrupt, got {other:?}"),
    }

    // Repaired, it reads normally -- so the refusals were about the
    // values and not about the database having been reopened.
    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute_batch("UPDATE pending_outbound SET attempts = 0")
        .expect("repair");
    drop(conn);
    let store = HumanStore::open(&path, StoreOptions::default()).expect("reopens");
    assert_eq!(store.pending_outbound().expect("reads").len(), 2);
}

#[test]
fn two_endpoints_on_one_peer_may_use_the_same_application_id() {
    // `ENDPOINTS.md`: "Including `source_endpoint` prevents a message ID
    // collision between two endpoints on the same authenticated peer from
    // suppressing an independent delivery."
    //
    // The store was one scope level short of that. `migration_2` stopped
    // two PEERS colliding; a peer's own `human` and `automation` endpoints
    // still aliased, so the second message never reached durable state
    // while the caller was told it had — the same harm, one level down.
    let mut store = memory();

    let human = store
        .commit_unread_inbound(&inbound_via("human", ID_A, b"from human".to_vec()))
        .expect("the first endpoint commits");
    let automation = store
        .commit_unread_inbound(&inbound_via(
            "automation",
            ID_A,
            b"from automation".to_vec(),
        ))
        .expect("a different endpoint on the same peer may reuse the id");
    assert_ne!(human, automation, "they are independent deliveries");

    let unread = store.unread_inbound().expect("read");
    assert_eq!(unread.len(), 2, "both are held");
    let mut bodies: Vec<&[u8]> = unread.iter().map(|r| r.payload.as_slice()).collect();
    bodies.sort_unstable();
    assert_eq!(
        bodies,
        vec![b"from automation".as_slice(), b"from human".as_slice()],
        "neither body was replaced by the other"
    );
}

#[test]
fn one_endpoint_reusing_its_own_id_for_new_content_is_still_a_conflict() {
    // Widening a uniqueness key is exactly how a dedup guard gets removed
    // by accident. Scoping to the endpoint must not turn a single
    // endpoint's id reuse into two rows: within one endpoint the conflict
    // that `one_peer_reusing_its_own_id_for_new_content_is_a_conflict`
    // proves for the NULL-endpoint case must still fire.
    let mut store = memory();

    let first = store
        .commit_unread_inbound(&inbound_via("human", ID_A, b"original".to_vec()))
        .expect("commit");
    let held = store.mark_read(first, 1_000).expect("read");
    store.keep(&held, 2_000).expect("keep");

    let second = store
        .commit_unread_inbound(&inbound_via("human", ID_A, b"replacement".to_vec()))
        .expect("a new arrival is admitted");
    let second_held = store.mark_read(second, 3_000).expect("read");

    assert!(
        matches!(
            store.keep(&second_held, 4_000),
            Err(StoreError::IdentityConflict { .. })
        ),
        "the same endpoint reusing its id for new content still conflicts"
    );

    let kept = store.kept_inbound().expect("read");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].payload, b"original".to_vec());
}

#[test]
fn an_absent_source_endpoint_still_dedups() {
    // THE TRAP IN THE OBVIOUS FIX. `source_endpoint` is nullable, and
    // SQLite treats NULLs in a UNIQUE key as DISTINCT — so spelling the
    // key `UNIQUE(source_peer, source_endpoint, app_message_id)` silently
    // removes dedup for every row that has no asserted endpoint, which is
    // every row the rest of this suite creates.
    //
    // `source_endpoint_key` collapses NULL to the empty string, which is
    // not a legal EndpointId and so cannot alias a real one. Break that
    // and this fails while the fix above still passes.
    let mut store = memory();

    let first = store
        .commit_unread_inbound(&inbound(ID_A, b"original".to_vec()))
        .expect("commit");
    let held = store.mark_read(first, 1_000).expect("read");
    store.keep(&held, 2_000).expect("keep");

    let second = store
        .commit_unread_inbound(&inbound(ID_A, b"replacement".to_vec()))
        .expect("admitted");
    let second_held = store.mark_read(second, 3_000).expect("read");

    assert!(
        matches!(
            store.keep(&second_held, 4_000),
            Err(StoreError::IdentityConflict { .. })
        ),
        "a NULL endpoint must not read as a distinct key on every insert"
    );
    assert_eq!(store.kept_inbound().expect("read").len(), 1);
}

#[test]
fn the_same_publisher_and_id_on_two_channels_are_two_records_and_both_can_be_kept() {
    // A broadcast carries no source endpoint, so before `migration_4`
    // its scope was the publisher and the application id alone: one
    // envelope on two channels was one row, and the second delivery was
    // refused as a duplicate. The transport keeps the two apart by
    // publisher, channel and message id; so does the store now.
    let mut store = memory();
    let on_a = store
        .commit_unread_inbound(&inbound_on("channel-a", ID_A, b"on a".to_vec()))
        .expect("the first channel's delivery");
    let on_b = store
        .commit_unread_inbound(&inbound_on("channel-b", ID_A, b"on b".to_vec()))
        .expect("the second channel's delivery is a record of its own");
    assert_ne!(on_a, on_b);
    let unread = store.unread_inbound().expect("read");
    assert_eq!(unread.len(), 2, "both channels' records are held");
    // THE CONTROL: the same envelope on the same channel again is the
    // duplicate it always was.
    assert!(
        store
            .commit_unread_inbound(&inbound_on("channel-a", ID_A, b"on a".to_vec()))
            .is_err(),
        "a duplicate within one channel is still refused"
    );
    // A direct delivery -- no channel -- is a scope of its own beside
    // them, not the empty channel colliding with either.
    store
        .commit_unread_inbound(&inbound(ID_A, b"direct".to_vec()))
        .expect("a direct delivery with the same id is a third record");
    assert_eq!(store.unread_inbound().expect("read").len(), 3);

    // Keeping both keeps each with its own channel.
    let held_a = store.mark_read(on_a, 3_000).expect("read a");
    let held_b = store.mark_read(on_b, 3_000).expect("read b");
    let kept_a = store.keep(&held_a, 4_000).expect("keep a");
    let kept_b = store.keep(&held_b, 4_000).expect("keep b");
    assert_ne!(kept_a, kept_b);
    let kept = store.kept_inbound().expect("read kept");
    let channels: Vec<Option<String>> = kept
        .iter()
        .map(|m| m.origin.channel.as_ref().map(|c| c.as_str().to_owned()))
        .collect();
    assert_eq!(
        channels,
        vec![Some("channel-a".to_owned()), Some("channel-b".to_owned())],
        "each kept record carries the channel it arrived on"
    );
    assert_eq!(kept[0].payload, b"on a".to_vec());
    assert_eq!(kept[1].payload, b"on b".to_vec());
}

/// A v3 database, written by hand exactly as that build left it, with
/// the given rows in `unread_inbound`.
fn v3_database(path: &std::path::Path, unread_rows: &[(i64, &str)]) {
    let conn = rusqlite::Connection::open(path).expect("create");
    conn.execute_batch(
        "
        CREATE TABLE pending_outbound (
            row_id                INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id        TEXT    NOT NULL UNIQUE,
            destination_peer      TEXT    NOT NULL,
            destination_endpoint  TEXT,
            channel_id            TEXT,
            media_type            TEXT,
            payload               BLOB    NOT NULL,
            created_at            INTEGER NOT NULL,
            last_attempt_at       INTEGER,
            attempts              INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE unread_inbound (
            row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id  TEXT    NOT NULL,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL,
            source_endpoint_key TEXT GENERATED ALWAYS AS (IFNULL(source_endpoint, '')) VIRTUAL,
            UNIQUE(source_peer, source_endpoint_key, app_message_id)
        );
        CREATE TABLE kept_inbound (
            row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id  TEXT    NOT NULL,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL,
            read_at         INTEGER NOT NULL,
            kept_at         INTEGER NOT NULL,
            source_endpoint_key TEXT GENERATED ALWAYS AS (IFNULL(source_endpoint, '')) VIRTUAL,
            UNIQUE(source_peer, source_endpoint_key, app_message_id)
        );
        CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        PRAGMA user_version = 3;
        ",
    )
    .expect("the v3 schema is legal SQLite");
    for (row_id, id) in unread_rows {
        conn.execute(
            "INSERT INTO unread_inbound
                (row_id, app_message_id, source_peer, source_endpoint, channel_id, payload, received_at)
             VALUES (?1, ?2, ?3, 'human', 'channel-a', ?4, 2000)",
            rusqlite::params![row_id, id, PEER, b"v3 row".to_vec()],
        )
        .expect("seed a v3 row");
    }
    drop(conn);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("tighten the fixture database");
}

#[test]
fn a_v3_database_migrates_to_the_channel_scoped_key_without_losing_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .expect("tighten the fixture state directory");
    let path = state.join("human.sqlite3");
    v3_database(&path, &[(1, ID_A)]);

    let mut store = HumanStore::open(&path, StoreOptions::default())
        .expect("a v3 database migrates rather than being refused");
    let unread = store.unread_inbound().expect("read");
    assert_eq!(unread.len(), 1, "the v3 row survived the rebuild");
    assert_eq!(unread[0].payload, b"v3 row".to_vec());
    assert_eq!(
        unread[0].origin.channel.as_ref().map(|c| c.as_str()),
        Some("channel-a")
    );
    // And the widened key is actually in force afterwards: the same
    // publisher, endpoint and id on another channel is a second record.
    store
        .commit_unread_inbound(&NewInbound {
            origin: InboundOrigin {
                peer: peer(),
                endpoint: Some(EndpointId::parse("human").expect("canonical")),
                channel: Some(ChannelId::parse("channel-b").expect("canonical")),
            },
            ..inbound(ID_A, b"on b".to_vec())
        })
        .expect("the migrated table is scoped by channel");
    assert_eq!(store.unread_inbound().expect("read").len(), 2);
}

#[test]
fn a_rebuild_keeps_the_row_id_high_water_mark() {
    // A rebuild that copies the rows and their ids but not the table's
    // `sqlite_sequence` entry hands the id of a message deleted before
    // the migration to a message stored after it -- which AUTOINCREMENT
    // exists here to prevent. Rows 1..3 allocated, 2 and 3 deleted
    // before the boundary: the next id after it must be 4.
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .expect("tighten the fixture state directory");
    let path = state.join("human.sqlite3");
    v3_database(
        &path,
        &[
            (1, ID_A),
            (2, ID_B),
            (3, "00000000000000000000000000000003"),
        ],
    );
    {
        let conn = rusqlite::Connection::open(&path).expect("reopen");
        conn.execute("DELETE FROM unread_inbound WHERE row_id IN (2, 3)", [])
            .expect("delete the newest rows");
        let seq: i64 = conn
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'unread_inbound'",
                [],
                |r| r.get(0),
            )
            .expect("the high-water mark exists");
        assert_eq!(seq, 3, "the fixture allocated three ids");
    }

    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("migrates");
    let next = store
        .commit_unread_inbound(&inbound_on(
            "channel-b",
            "00000000000000000000000000000004",
            b"after".to_vec(),
        ))
        .expect("commits");
    assert_eq!(
        next.get(),
        4,
        "an id allocated before the migration is never allocated again after it"
    );

    // An emptied table keeps its mark too: nothing survives the copy, and
    // the next id is still past everything ever allocated.
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .expect("tighten the fixture state directory");
    let path = state.join("human.sqlite3");
    v3_database(&path, &[(1, ID_A), (2, ID_B)]);
    {
        let conn = rusqlite::Connection::open(&path).expect("reopen");
        conn.execute("DELETE FROM unread_inbound", [])
            .expect("empty the table");
    }
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("migrates");
    let next = store
        .commit_unread_inbound(&inbound(ID_A, b"after".to_vec()))
        .expect("commits");
    assert_eq!(next.get(), 3, "an emptied table's mark is carried as well");
}

#[test]
fn a_v1_database_keeps_the_row_id_high_water_mark_through_every_rebuild() {
    // The v2 fixture below starts past migration_2, so only this one
    // runs all three rebuilds. Rows 1..3 allocated, 2 and 3 deleted, and
    // the next id after the upgrade is 4 -- deleting the carry from any
    // of the three migrations makes it 2.
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .expect("tighten the fixture state directory");
    let path = state.join("human.sqlite3");
    let conn = rusqlite::Connection::open(&path).expect("create");
    conn.execute_batch(
        "
        CREATE TABLE pending_outbound (
            row_id                INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id        TEXT    NOT NULL UNIQUE,
            destination_peer      TEXT    NOT NULL,
            destination_endpoint  TEXT,
            channel_id            TEXT,
            media_type            TEXT,
            payload               BLOB    NOT NULL,
            created_at            INTEGER NOT NULL,
            last_attempt_at       INTEGER,
            attempts              INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE unread_inbound (
            row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id  TEXT    NOT NULL UNIQUE,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL
        );
        CREATE TABLE kept_inbound (
            row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id  TEXT    NOT NULL UNIQUE,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL,
            read_at         INTEGER NOT NULL,
            kept_at         INTEGER NOT NULL
        );
        CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        PRAGMA user_version = 1;
        ",
    )
    .expect("the v1 schema is legal SQLite");
    conn.execute(
        "INSERT INTO unread_inbound (app_message_id, source_peer, payload, received_at)
            VALUES ('00000000000000000000000000000001', ?1, x'00', 1),
                   ('00000000000000000000000000000002', ?1, x'00', 1),
                   ('00000000000000000000000000000003', ?1, x'00', 1)",
        [PEER],
    )
    .expect("seed three v1 rows");
    conn.execute("DELETE FROM unread_inbound WHERE row_id IN (2, 3)", [])
        .expect("delete the newest two");
    drop(conn);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("tighten the fixture database");

    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("migrates");
    assert_eq!(store.unread_inbound().expect("read").len(), 1);
    let next = store
        .commit_unread_inbound(&inbound(ID_A, b"after".to_vec()))
        .expect("commits");
    assert_eq!(next.get(), 4, "the mark survived migrations 2, 3 and 4");
}

#[test]
fn a_v2_database_migrates_to_the_endpoint_scoped_key_without_losing_rows() {
    // The rebuild in `migration_3` drops and recreates both inbound
    // tables. A migration that widened the key but lost the content would
    // pass every assertion above, because they all start from an empty
    // store.
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    // The store refuses a state directory anyone else can read, so the
    // fixture has to be as tight as the one `open` would have created.
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .expect("tighten the fixture state directory");
    let path = state.join("human.sqlite3");

    // A v2 database, written by hand exactly as that build left it.
    let conn = rusqlite::Connection::open(&path).expect("create");
    conn.execute_batch(
        "
        CREATE TABLE pending_outbound (
            row_id                INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id        TEXT    NOT NULL UNIQUE,
            destination_peer      TEXT    NOT NULL,
            destination_endpoint  TEXT,
            channel_id            TEXT,
            media_type            TEXT,
            payload               BLOB    NOT NULL,
            created_at            INTEGER NOT NULL,
            last_attempt_at       INTEGER,
            attempts              INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE unread_inbound (
            row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id  TEXT    NOT NULL,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL,
            UNIQUE(source_peer, app_message_id)
        );
        CREATE TABLE kept_inbound (
            row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id  TEXT    NOT NULL,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL,
            read_at         INTEGER NOT NULL,
            kept_at         INTEGER NOT NULL,
            UNIQUE(source_peer, app_message_id)
        );
        CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        PRAGMA user_version = 2;
        ",
    )
    .expect("the v2 schema is legal SQLite");
    conn.execute(
        "INSERT INTO unread_inbound
            (app_message_id, source_peer, source_endpoint, payload, received_at)
         VALUES (?1, ?2, 'human', ?3, 2000)",
        rusqlite::params![ID_A, PEER, b"carried across".to_vec()],
    )
    .expect("seed a v2 row");
    // Two more ids allocated and deleted, so the high-water mark (3) is
    // above the surviving row (1) through BOTH rebuilds that follow.
    conn.execute_batch(
        "INSERT INTO unread_inbound (app_message_id, source_peer, payload, received_at)
             VALUES ('00000000000000000000000000000002', 'x', x'00', 1),
                    ('00000000000000000000000000000003', 'x', x'00', 1);
         DELETE FROM unread_inbound WHERE row_id IN (2, 3);",
    )
    .expect("allocate and delete");
    drop(conn);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("tighten the fixture database");

    let mut store = HumanStore::open(&path, StoreOptions::default())
        .expect("a v2 database migrates rather than being refused");

    let unread = store.unread_inbound().expect("read");
    assert_eq!(unread.len(), 1, "the v2 row survived the rebuild");
    assert_eq!(unread[0].payload, b"carried across".to_vec());

    // And the widened key is actually in force afterwards.
    let next = store
        .commit_unread_inbound(&inbound_via("automation", ID_A, b"new endpoint".to_vec()))
        .expect("the migrated table is scoped by endpoint");
    assert_eq!(store.unread_inbound().expect("read").len(), 2);
    assert_eq!(
        next.get(),
        4,
        "the high-water mark survived migration_2, migration_3 and migration_4"
    );
}

#[test]
fn a_generated_key_with_the_right_name_and_a_different_expression_is_refused() {
    // THE NAME IS NOT THE CONTRACT, THE EXPRESSION IS. `table_info` does
    // not report a generated column at all and `index_info` reports only
    // its name, so a `source_endpoint_key` redefined as the constant ''
    // presents an identical column list AND an identical unique key —
    // while collapsing every endpoint back into one, which is precisely
    // the suppression migration_3 exists to stop.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("opens"));

    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute_batch(
        "
        ALTER TABLE unread_inbound RENAME TO unread_inbound_old;
        CREATE TABLE unread_inbound (
            row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id  TEXT    NOT NULL,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL,
            source_endpoint_key TEXT GENERATED ALWAYS AS ('') VIRTUAL,
            channel_key         TEXT GENERATED ALWAYS AS (IFNULL(channel_id, '')) VIRTUAL,
            UNIQUE(source_peer, source_endpoint_key, channel_key, app_message_id)
        );
        DROP TABLE unread_inbound_old;
        ",
    )
    .expect("the rebuild is legal SQLite");
    drop(conn);

    let refused = HumanStore::open(&path, StoreOptions::default());
    assert!(
        matches!(&refused, Err(StoreError::Migration(m)) if m.contains("disagrees with")),
        "a generated column with a different expression must be refused BY THE EXPRESSION \
         CHECK, not by an earlier one, got {refused:?}"
    );
}

#[test]
fn a_stored_generated_column_is_not_the_virtual_one_this_build_wrote() {
    // STORED rather than VIRTUAL is a different schema: it occupies a
    // real value per row, which is a content surface ADR-0044 did not
    // budget for. `table_xinfo` reports 3 for it and 2 for VIRTUAL, and
    // the declaration text differs, so it is caught twice over.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("opens"));

    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute_batch(
        "
        ALTER TABLE kept_inbound RENAME TO kept_inbound_old;
        CREATE TABLE kept_inbound (
            row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id  TEXT    NOT NULL,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL,
            read_at         INTEGER NOT NULL,
            kept_at         INTEGER NOT NULL,
            source_endpoint_key TEXT GENERATED ALWAYS AS (IFNULL(source_endpoint, '')) STORED,
            channel_key         TEXT GENERATED ALWAYS AS (IFNULL(channel_id, '')) VIRTUAL,
            UNIQUE(source_peer, source_endpoint_key, channel_key, app_message_id)
        );
        DROP TABLE kept_inbound_old;
        ",
    )
    .expect("the rebuild is legal SQLite");
    drop(conn);

    let refused = HumanStore::open(&path, StoreOptions::default());
    assert!(
        matches!(&refused, Err(StoreError::Migration(m)) if m.contains("has generated columns")),
        "a STORED generated column is not what this build wrote, and it is the hidden-column \
         check that says so: {refused:?}"
    );
}

#[test]
fn an_extra_generated_column_is_a_retention_violation_table_info_cannot_see() {
    // A generated column is INVISIBLE to `PRAGMA table_info`, so the
    // column check that refuses `ADD COLUMN archive_payload BLOB` cannot
    // see this one at all — and a virtual column reading `payload` is a
    // second durable view of the body in the table whose whole contract
    // is that the body disappears when the message is read.
    //
    // `table_xinfo` is what makes it visible, which is why the hidden set
    // is enumerated rather than assumed empty.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("opens"));

    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute_batch(
        "ALTER TABLE unread_inbound
             ADD COLUMN archive_payload BLOB GENERATED ALWAYS AS (payload) VIRTUAL",
    )
    .expect("SQLite permits adding a virtual generated column");
    drop(conn);

    let refused = HumanStore::open(&path, StoreOptions::default());
    assert!(
        matches!(&refused, Err(StoreError::Migration(m)) if m.contains("has generated columns")),
        "a second view of the body must be refused however it is spelled, by the hidden-column \
         check: {refused:?}"
    );
}

#[test]
fn a_decoy_comment_carrying_the_expected_declaration_does_not_excuse_a_constant() {
    // SQLite PRESERVES COMMENTS in `sqlite_master.sql`, so a column
    // generated from a constant can carry the expected declaration
    // verbatim inside `/* ... */`. The name matches, the hidden set
    // matches, the unique key matches (the fixture writes the current
    // four-column key, or an earlier check would refuse it for the
    // wrong reason), and any substring test over the
    // stored SQL finds the text it was looking for -- inside the comment
    // -- while endpoints collapse back into one.
    //
    // This is why the expression is EVALUATED rather than read.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("opens"));

    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute_batch(
        "
        ALTER TABLE unread_inbound RENAME TO unread_inbound_old;
        CREATE TABLE unread_inbound (
            row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id  TEXT    NOT NULL,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL,
            source_endpoint_key TEXT GENERATED ALWAYS AS ('') VIRTUAL
                /* source_endpoint_key TEXT GENERATED ALWAYS AS (IFNULL(source_endpoint, '')) VIRTUAL */,
            channel_key         TEXT GENERATED ALWAYS AS (IFNULL(channel_id, '')) VIRTUAL,
            UNIQUE(source_peer, source_endpoint_key, channel_key, app_message_id)
        );
        DROP TABLE unread_inbound_old;
        ",
    )
    .expect("the rebuild is legal SQLite");

    // The decoy really is in the stored SQL: this test is only meaningful
    // if a substring check WOULD have been fooled by it.
    let stored: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'unread_inbound'",
            [],
            |r| r.get(0),
        )
        .expect("read the stored ddl");
    assert!(
        stored.contains("GENERATED ALWAYS AS (IFNULL(source_endpoint, ''))"),
        "the decoy must be present, or this test proves nothing"
    );
    drop(conn);

    let refused = HumanStore::open(&path, StoreOptions::default());
    assert!(
        matches!(&refused, Err(StoreError::Migration(m)) if m.contains("disagrees with")),
        "a constant expression must be refused however it is decorated, by the expression \
         check: {refused:?}"
    );
}

#[test]
fn a_channel_key_expression_that_collapses_channels_is_refused() {
    // The channel key is verified by the same evaluator, against ITS
    // grammar: a case fold, a truncation at 64 and a filter of `:` are
    // each the identity on every endpoint probe and a collapse on
    // channels, so a probe set written for endpoints would accept all
    // three. The endpoint key is left intact so the refusal is the
    // channel key's.
    for tampered in [
        "''",
        "IFNULL(lower(channel_id), '')",
        "IFNULL(substr(channel_id, 1, 64), '')",
        "IFNULL(replace(channel_id, ':', ''), '')",
        "IFNULL(replace(channel_id, '/', ''), '')",
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state").join("human.sqlite3");
        drop(HumanStore::open(&path, StoreOptions::default()).expect("opens"));
        let conn = rusqlite::Connection::open(&path).expect("reopen");
        conn.execute_batch(&format!(
            "
            ALTER TABLE unread_inbound RENAME TO unread_inbound_old;
            CREATE TABLE unread_inbound (
                row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
                app_message_id  TEXT    NOT NULL,
                source_peer     TEXT    NOT NULL,
                source_endpoint TEXT,
                channel_id      TEXT,
                media_type      TEXT,
                payload         BLOB    NOT NULL,
                received_at     INTEGER NOT NULL,
                source_endpoint_key TEXT GENERATED ALWAYS AS (IFNULL(source_endpoint, '')) VIRTUAL,
                channel_key         TEXT GENERATED ALWAYS AS ({tampered}) VIRTUAL,
                UNIQUE(source_peer, source_endpoint_key, channel_key, app_message_id)
            );
            DROP TABLE unread_inbound_old;
            "
        ))
        .expect("the rebuild is legal SQLite");
        drop(conn);
        let refused = HumanStore::open(&path, StoreOptions::default());
        assert!(
            matches!(&refused, Err(StoreError::Migration(m))
                if m.contains("disagrees with") && m.contains("`channel_key`")),
            "channel_key AS ({tampered}) must be refused by the expression check, naming the \
             channel key: {refused:?}"
        );
    }
}

#[test]
fn a_truncating_generated_key_is_refused_even_though_it_matches_short_probes() {
    // THE SAMPLE HAS TO REACH THE GRAMMAR'S EDGE. An earlier version of
    // this guard probed one 14-character endpoint and NULL, so
    // `IFNULL(substr(source_endpoint, 1, 14), '')` reproduced both values
    // exactly and passed — while every id longer than 14 characters was
    // truncated into a collision with anything sharing its prefix, which
    // is the suppression migration_3 exists to stop.
    //
    // The probe set now reaches 64 characters, the longest legal
    // EndpointId, so no truncation can hide inside it.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("opens"));

    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute_batch(
        "
        ALTER TABLE unread_inbound RENAME TO unread_inbound_old;
        CREATE TABLE unread_inbound (
            row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id  TEXT    NOT NULL,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL,
            source_endpoint_key TEXT
                GENERATED ALWAYS AS (IFNULL(substr(source_endpoint, 1, 14), '')) VIRTUAL,
            channel_key         TEXT GENERATED ALWAYS AS (IFNULL(channel_id, '')) VIRTUAL,
            UNIQUE(source_peer, source_endpoint_key, channel_key, app_message_id)
        );
        DROP TABLE unread_inbound_old;
        ",
    )
    .expect("the rebuild is legal SQLite");
    drop(conn);

    let refused = HumanStore::open(&path, StoreOptions::default());
    assert!(
        matches!(&refused, Err(StoreError::Migration(m)) if m.contains("disagrees with")),
        "a truncating expression must be refused by the expression check, got {refused:?}"
    );
}

#[test]
fn a_generated_key_that_drops_a_legal_character_class_is_refused() {
    // EndpointId admits `.`, `-`, `_` and digits. An expression that
    // filters one of them agrees with every alphabetic probe and then
    // collides `a.b` with `ab` — so the probe set carries one id per
    // legal character class rather than a handful of plausible names.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("opens"));

    let conn = rusqlite::Connection::open(&path).expect("reopen");
    conn.execute_batch(
        "
        ALTER TABLE kept_inbound RENAME TO kept_inbound_old;
        CREATE TABLE kept_inbound (
            row_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            app_message_id  TEXT    NOT NULL,
            source_peer     TEXT    NOT NULL,
            source_endpoint TEXT,
            channel_id      TEXT,
            media_type      TEXT,
            payload         BLOB    NOT NULL,
            received_at     INTEGER NOT NULL,
            read_at         INTEGER NOT NULL,
            kept_at         INTEGER NOT NULL,
            source_endpoint_key TEXT
                GENERATED ALWAYS AS (IFNULL(replace(source_endpoint, '.', ''), '')) VIRTUAL,
            channel_key         TEXT GENERATED ALWAYS AS (IFNULL(channel_id, '')) VIRTUAL,
            UNIQUE(source_peer, source_endpoint_key, channel_key, app_message_id)
        );
        DROP TABLE kept_inbound_old;
        ",
    )
    .expect("the rebuild is legal SQLite");
    drop(conn);

    let refused = HumanStore::open(&path, StoreOptions::default());
    assert!(
        matches!(&refused, Err(StoreError::Migration(m)) if m.contains("disagrees with")),
        "an expression that drops a legal character must be refused by the expression check: \
         {refused:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_bare_relative_path_still_checks_its_directory() {
    // `Path::new("messages.db").parent()` is `Some("")`, and filtering
    // the empty string out skipped every parent check — in a shared
    // working directory, the case that most needed them. The check now
    // resolves an empty parent to ".", so a bare name is judged exactly
    // as an explicit path into the same directory would be.
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
        .expect("world-traversable on purpose");

    let explicit = HumanStore::open(&dir.path().join("messages.db"), StoreOptions::default());
    assert!(
        matches!(explicit, Err(StoreError::PermissionsTooOpen { .. })),
        "an explicit path into a world-traversable directory is refused: {explicit:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_database_path_is_refused_not_followed() {
    // `metadata` follows links, so the mode check answered about the
    // TARGET. Another local account able to pre-create the path could
    // redirect this process into opening a database elsewhere, using
    // its own authority.
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    // Owner-only, so the parent check passes and the symlink check is
    // what this test actually reaches.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
        .expect("private parent");
    let elsewhere = dir.path().join("elsewhere.db");
    std::fs::write(&elsewhere, b"").expect("target exists");

    let link = dir.path().join("messages.db");
    std::os::unix::fs::symlink(&elsewhere, &link).expect("link created");

    let opened = HumanStore::open(&link, StoreOptions::default());
    assert!(
        matches!(opened, Err(StoreError::NotAFile { .. })),
        "a symlinked database path is refused: {opened:?}"
    );
}

#[test]
fn a_nonsense_read_timestamp_is_refused_before_the_unread_row_is_destroyed() {
    // THE ONE PLACE REFUSING COULD LOSE DATA. `mark_read` deletes the
    // durable unread row and hands back a `ReadEphemeral` carrying the
    // caller's `at_ms`. With no check here, `keep` then refused on
    // `read_at` -- and `ReadEphemeral`'s fields are crate-private, so the
    // caller could neither repair the value nor get the row back. The
    // message became permanently unkeepable, which is the retention
    // contract violated by a fix meant to tighten it.
    let mut store = memory();
    let new = inbound("00000000000000000000000000000003", vec![3]);
    store.commit_unread_inbound(&new).expect("committed");
    let row = store.unread_inbound().expect("read")[0].row_id;

    match store.mark_read(row, u64::MAX) {
        Err(StoreError::TimestampOutOfRange { field, got }) => {
            assert_eq!(field, "read_at");
            assert_eq!(got, u64::MAX);
        }
        other => panic!("an unrepresentable read time must be refused: {other:?}"),
    }

    // THE POINT: the row survived the refusal, so the caller can retry
    // with a sane clock and the message is still keepable.
    assert_eq!(
        store.unread_inbound().expect("read").len(),
        1,
        "the durable unread copy must survive a refused read"
    );
    let held = store
        .mark_read(row, 1_000)
        .expect("a sane read time is accepted");
    store
        .keep(&held, 2_000)
        .expect("and the message is keepable");
}

#[test]
fn the_three_remaining_reachable_timestamp_sites_refuse_rather_than_saturating() {
    // The refusal was tested at ONE of its sites, so reverting the others
    // left the whole suite green. A review found that.
    //
    // EIGHT CALL SITES, FIVE OF THEM REACHABLE from outside the crate, and
    // THIS TEST COVERS THREE -- `created_at`, `record_attempt`'s `at_ms`,
    // and `keep`'s `at_ms`. The other two reachable sites are pinned by the
    // two tests NAMED below rather than located, because an earlier version
    // said "directly above" and only one of them is:
    // `a_timestamp_the_store_cannot_represent_is_refused_not_saturated`
    // for `commit_unread_inbound`'s `received_at`, near the TOP of this
    // file, and
    // `a_nonsense_read_timestamp_is_refused_before_the_unread_row_is_destroyed`
    // for `mark_read`'s `read_at`, which is the one directly above.
    //
    // The remaining three take a value the store itself produced: `keep`'s
    // `held.received_at` and `held.read_at` come from a `ReadEphemeral`
    // whose fields are crate-private, and both were already refused on the
    // way in -- by `commit_unread_inbound` and by `mark_read` -- so they
    // are fail-closed guards on inputs that can no longer arrive, not
    // untested paths. `cursor_bounds` is the same: a `Cursor` is only ever
    // handed back by a previous page.
    //
    // AND THE NAME NOW SAYS THREE, because a test name is read on its own.
    // It was `every_reachable_timestamp_site_…`, which claimed five; the
    // comment conceding that the name overclaims "unless the two siblings
    // are read with it" was asking a reader to carry a footnote into a
    // `cargo test` listing, where only the name appears. Review finding on
    // PR #86.
    //
    // THE EARLIER ACCOUNTING SAID "three of the six", which is wrong twice:
    // `sql_timestamp` has eight call sites, not six, and five are reachable,
    // not three. It reached its number by omitting the two the sibling tests
    // cover and counting `cursor_bounds` inside the total. Counted rather
    // than remembered. Review finding on PR #86.
    let mut store = memory();

    // 1. `created_at`, through the outbound commit.
    let mut out = outbound("0000000000000000000000000000000a", vec![1]);
    out.created_at = u64::MAX;
    match store.commit_pending_outbound(&out) {
        Err(StoreError::TimestampOutOfRange { field, got }) => {
            assert_eq!(field, "created_at");
            assert_eq!(got, u64::MAX);
        }
        other => panic!("an unrepresentable created_at must be refused: {other:?}"),
    }
    assert!(
        store.pending_outbound().expect("read").is_empty(),
        "and nothing is stored under a saturated value"
    );

    // 2. `at_ms`, through the attempt counter.
    let sane = outbound("0000000000000000000000000000000b", vec![2]);
    let row = store.commit_pending_outbound(&sane).expect("committed");
    match store.record_attempt(row, u64::MAX) {
        Err(StoreError::TimestampOutOfRange { field, got }) => {
            assert_eq!(field, "at_ms");
            assert_eq!(got, u64::MAX);
        }
        other => panic!("an unrepresentable attempt time must be refused: {other:?}"),
    }

    // 3. `at_ms`, through `keep`.
    let held_src = inbound("0000000000000000000000000000000c", vec![3]);
    store.commit_unread_inbound(&held_src).expect("committed");
    let unread = store.unread_inbound().expect("read");
    let held = store
        .mark_read(unread[0].row_id, 1_000)
        .expect("read at a sane time");
    match store.keep(&held, u64::MAX) {
        Err(StoreError::TimestampOutOfRange { field, got }) => {
            assert_eq!(field, "at_ms");
            assert_eq!(got, u64::MAX);
        }
        other => panic!("an unrepresentable keep time must be refused: {other:?}"),
    }
    assert!(
        store.kept_inbound().expect("read").is_empty(),
        "and nothing is kept under a saturated value"
    );
}
