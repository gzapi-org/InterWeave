// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The durable store itself.
//!
//! Every retention decision is delegated to `interweave-human-core`
//! rather than re-decided here. That looks indirect for a transition
//! whose answer is obvious — a terminal outbound is deleted, and this
//! module knows that — but re-deciding would create a second authority
//! on ADR-0044, and the two would eventually disagree. The state machine
//! decides [`Durability`]; this module is the thing that carries it out,
//! and if the state machine ever changes its answer, the store follows
//! without being edited.

use std::path::Path;

use interweave_human_core::retention::{
    Durability, InboundMessage, OutboundMessage, StorageHealth, TerminalCause,
};
use interweave_profile_config as profile_config;
use interweave_transport_api::payload::MAX_PAYLOAD_BYTES;
use interweave_transport_api::{
    ChannelId, DirectDestination, EndpointId, MediaType, MessageId, TransportIdentity,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;
use crate::records::{
    AppMessageId, BackupCursor, BackupTable, Cursor, InboundOrigin, NewInbound, NewOutbound,
    OutboundDestination, Page, PageLimits, PendingOutbound, ReadEphemeral, RowId, StoredInbound,
};
use crate::schema::{migrate, verify_shape};

/// A stored timestamp, refusing a negative one rather than flattening it.
///
/// `unwrap_or(0)` looked like a harmless normalization and was not. A
/// negative `received_at` read back as zero while SQL kept ordering by
/// the raw value, so the row sorted first and the cursor built from it
/// carried zero -- and the next page asked for rows after zero, walking
/// straight past every other malformed row. A corrupt value became a
/// silently truncated result set.
fn stored_ms(field: &'static str, value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt(format!("{field} is negative: {value}")))
}

/// A stored non-negative counter, refusing a negative one.
fn stored_count(field: &'static str, value: i64) -> Result<u32, StoreError> {
    u32::try_from(value).map_err(|_| StoreError::Corrupt(format!("{field} is not a u32: {value}")))
}

/// How the store is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoreOptions {
    /// A hard page ceiling for the database file, if the application
    /// imposes a quota.
    ///
    /// `None` means the filesystem is the only limit. `Some(0)` is
    /// refused at open with [`StoreError::QuotaNotApplied`]: SQLite
    /// ignores it, which would leave no quota. A ceiling below the size an
    /// existing database already has cannot be applied: SQLite enforces
    /// the database's size instead, and the store opens
    /// [`StorageHealth::Degraded`] and stays so until its used pages fit
    /// the ceiling asked for (`HumanStore::recheck_health` then applies
    /// it), its content readable and releasable meanwhile. When set,
    /// exceeding
    /// it produces a real `SQLITE_FULL` from SQLite — the same error a
    /// full disk produces — which is what lets the degradation path be
    /// tested against the code that actually runs in production rather
    /// than against an injected fake.
    pub max_pages: Option<u32>,
}

/// Durable ADR-0044 retention storage.
///
/// Holds message content in exactly three states and nothing else. See
/// the crate documentation for what is deliberately absent.
#[derive(Debug)]
pub struct HumanStore {
    conn: Connection,
    health: StorageHealth,
    /// The page ceiling asked for (`StoreOptions::max_pages`), kept
    /// because SQLite may be enforcing a LOOSER one: a ceiling below the
    /// database's size is raised to that size. While it is, the store is
    /// degraded, whatever a probe finds (`recheck_health`).
    quota: Option<u32>,
}

/// The settings key that marks a store rewritten without the freed space a
/// build before `secure_delete` left (`HumanStore::scrub_at_open`).
const SCRUBBED: &str = "released_content_scrubbed";

/// THE CLOSE truncates the WAL too (`RETENTION.md` §8: absent "after a
/// clean close"). DEFENCE IN DEPTH, NOT SEPARATELY PINNED: SQLite's own
/// close of the last connection checkpoints and removes the WAL as well,
/// so no test can tell this truncate apart; `tests/released_content.rs`
/// pins the outcome (absent after a clean close), not this line.
impl Drop for HumanStore {
    fn drop(&mut self) {
        let _ = self.truncate_wal();
    }
}

/// A public `u64` millisecond timestamp as SQLite's signed integer.
///
/// REFUSED RATHER THAN SATURATED. Every one of these was
/// `i64::try_from(value).unwrap_or(i64::MAX)`, so `i64::MAX`, `i64::MAX +
/// 1` and `u64::MAX` all became one stored value -- distinct accepted
/// inputs collapsing into each other, which is a public invariant broken
/// quietly rather than a limit enforced. No clock reaches it (`i64::MAX`
/// milliseconds is some 292 million years), so a caller who does is a bug
/// or a hostile input, and refusing tells them. Review finding.
fn sql_timestamp(field: &'static str, value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::TimestampOutOfRange { field, got: value })
}

impl HumanStore {
    /// Open (creating if needed) the store at `path`.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the file cannot be opened, a migration
    /// fails, or the database contains a table ADR-0044 forbids.
    pub fn open(path: &Path, options: StoreOptions) -> Result<Self, StoreError> {
        // Create the parent, owner-only. SQLite will not, and a caller
        // that has to remember to mkdir first is a caller that will
        // eventually not — the peer cache and the config writer both
        // create their own parents, and a store that alone did not would
        // be the one that failed on a fresh profile.
        //
        // AN EMPTY PARENT IS THE WORKING DIRECTORY, not "no parent".
        // `Path::new("messages.db").parent()` is `Some("")`, so filtering
        // the empty string out skipped every check below for a bare
        // relative path — in a shared or attacker-writable working
        // directory, exactly the case that most needed them.
        //
        // Every open below is made under the directory AS RESOLVED, not
        // the configured text: the rule makes that path's components
        // unchangeable by anyone but root and this uid, so nothing can
        // be swapped in between the judgement and the open (ADR-0028 A
        // 2026-10-08).
        // The file's name FIRST: a path ending in `..` names no file, and
        // asking after the directory was made left that directory behind
        // a refusal (`a_path_naming_no_file_is_refused_before_any_directory_is_made`).
        let name = path.file_name().ok_or(StoreError::NotAFile {
            what: "the database path names no file",
        })?;
        let dir = private_dir(private_parent_of(path))?;
        let resolved = dir.join(name);
        let path = resolved.as_path();
        // CREATE IT OWNER-ONLY OURSELVES. SQLite creates the database with
        // the process umask, which is 0644 on a default system — message
        // content readable by every local account. Creating the file
        // first, empty and 0600, means SQLite opens an existing file
        // rather than making one, and it copies the database's mode onto
        // the WAL and SHM companions it creates later.
        //
        // `create_new` so this cannot truncate an existing store, and a
        // lost race is not an error: the other process created it and the
        // check below decides whether what it created is acceptable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
            {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(StoreError::Io(e)),
            }

            // WHAT IS ACTUALLY THERE, not what the name resolves to. The
            // lost `create_new` race above is not an error precisely
            // because this decides whether the existing file is
            // acceptable — and `metadata` FOLLOWS symlinks, so it
            // answered about the target. Another local account can
            // pre-create the database path as a link and redirect this
            // process, using its own authority, into creating or opening
            // a database somewhere else entirely.
            //
            // `symlink_metadata` asks about the path itself, and anything
            // that is not a regular file is refused rather than repaired:
            // a store that has been redirected should be reported, not
            // silently relocated.
            let here = std::fs::symlink_metadata(path).map_err(StoreError::Io)?;
            if here.file_type().is_symlink() {
                return Err(StoreError::NotAFile {
                    what: "the database path is a symbolic link",
                });
            }
            if !here.is_file() {
                return Err(StoreError::NotAFile {
                    what: "the database path is not a regular file",
                });
            }
        }

        // BEFORE any connection: opening runs `journal_mode=WAL`, which
        // rewrites a rollback-mode file's header and makes companions. A
        // file whose header says it is from a newer build is refused here,
        // by that header alone, writing nothing (STATE.md Migrations).
        // NOT COVERED: a newer build that crashed with its version bump
        // still in an un-checkpointed WAL leaves an older-looking header;
        // `migrate` refuses that file through the WAL, and closing the
        // connection checkpoints the WAL into it -- the newer build's own
        // committed data, so nothing is lost, but the file's bytes change.
        if let Some(version) = header_user_version(path)?
            && version > crate::schema::SCHEMA_VERSION
        {
            return Err(StoreError::Migration(format!(
                "database is at schema version {version}, newer than this build's {}; \
                 refusing to downgrade",
                crate::schema::SCHEMA_VERSION
            )));
        }

        let conn = Connection::open(path)?;
        // The database and its WAL/SHM companions hold the same message
        // content as the directory, and SQLite creates the companions
        // itself with the process umask. Checked after the connection so
        // they exist to be checked.
        //
        // WHAT IS THERE, as for the database above: `exists` and `metadata`
        // follow a link, so a companion left as a link to an
        // owner-only file passed this check. For `-wal` and `-shm` SQLite
        // then refused it on its own, as `CannotOpen` -- an unclassified
        // open failure a client shows as "try again", which no wait
        // fixes; a linked `-journal` it opened over without complaint.
        // Judged here, each is refused as what it is
        // (`a_companion_that_is_a_link_is_refused_not_followed`).
        for (suffix, what, not_a_file) in [
            (
                "",
                "the database",
                "the database path is not a regular file",
            ),
            (
                "-wal",
                "the write-ahead log",
                "the write-ahead log is not a regular file",
            ),
            (
                "-shm",
                "the shared-memory index",
                "the shared-memory index is not a regular file",
            ),
            // A rollback journal left hot by a crash -- a file written
            // before WAL, or a build that ran without it -- is read by
            // SQLite on the first query, and holds the same content.
            (
                "-journal",
                "the rollback journal",
                "the rollback journal is not a regular file",
            ),
        ] {
            let mut companion = path.as_os_str().to_owned();
            companion.push(suffix);
            let companion = std::path::PathBuf::from(companion);
            match std::fs::symlink_metadata(&companion) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(StoreError::Io(e)),
                Ok(meta) if !meta.file_type().is_file() => {
                    return Err(StoreError::NotAFile { what: not_a_file });
                }
                Ok(_) => require_owner_only(&companion, what)?,
            }
        }
        // EXISTING AS THE CONNECTION SEES IT, not as the file's header
        // says: a store whose every session ended before a checkpoint -- an
        // Android process killed is the normal end -- holds its schema and
        // its messages only in the WAL, with a header still at version 0
        // (#214's review, F1). The header read above stays for the
        // newer-build refusal only. AFTER the owner-only checks above: reading
        // runs WAL recovery, and a refused connection, the last one, then
        // checkpoints and deletes the WAL as it closes -- the refusal
        // destroying what it refused (#214's re-review).
        let existing: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let mut store = Self::from_connection(conn, options)?;
        store.scrub_at_open(existing > 0)?;
        Ok(store)
    }

    /// Open a store that exists only for this process.
    ///
    /// For tests of logic that does not involve restart. Anything
    /// claiming a restart survives must use [`HumanStore::open`] against
    /// a real file and reopen it — an in-memory database cannot prove
    /// durability, since it has none.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the schema cannot be created.
    pub fn open_in_memory(options: StoreOptions) -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn, options)
    }

    fn from_connection(mut conn: Connection, options: StoreOptions) -> Result<Self, StoreError> {
        // WAL so a reader never blocks the commit of an inbound message,
        // and synchronous=FULL because this store's whole purpose is
        // surviving an abrupt end. NORMAL survives a process crash but
        // can lose the last transactions to a power cut, and "the message
        // you were told about is durable" must not have that asterisk.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        // RELEASED CONTENT LEAVES THE STORE'S OWN FILES (`RETENTION.md` §8,
        // A 2026-10-07, on SPIKE-008's measurement): without this a deleted
        // row's bytes stayed in its page, in plaintext, after the store
        // closed. Read back, as the quota is: a build that cannot apply it
        // is refused rather than run without it.
        let secure: i64 =
            conn.pragma_update_and_check(None, "secure_delete", true, |row| row.get(0))?;
        if secure != 1 {
            return Err(StoreError::SecureDeleteNotApplied);
        }
        // READ BACK, not assumed (review R5 on fa3eab8): the pragma
        // answers with the ceiling it set, which is not the one asked for
        // when the request is zero or below the database's current size.
        //
        // THE TWO ARE NOT THE SAME FAILURE (#117's blind review, F6). A
        // ceiling SQLite ignored -- zero -- leaves no quota at all, and is
        // refused. A ceiling below the database's size is raised to that
        // size: LOOSER than asked, though nothing can grow past what the
        // database already is, and refusing to open would leave no way
        // to read the unread content or release it. So that store opens
        // DEGRADED and stays degraded until its used pages fit the
        // ceiling asked for, when `recheck_health` compacts the file and
        // applies it (its re-review, F2: an earlier version let a
        // successful probe report Healthy under the looser ceiling).
        let mut health = StorageHealth::Healthy;
        if let Some(max_pages) = options.max_pages {
            let effective: i64 =
                conn.pragma_update_and_check(None, "max_page_count", max_pages, |row| row.get(0))?;
            if effective != i64::from(max_pages) {
                let size: i64 = conn.pragma_query_value(None, "page_count", |row| row.get(0))?;
                if max_pages == 0 || effective != size {
                    return Err(StoreError::QuotaNotApplied {
                        requested: max_pages,
                        effective,
                    });
                }
                health = StorageHealth::Degraded;
            }
        }

        migrate(&mut conn)?;
        verify_shape(&conn)?;

        Ok(Self {
            conn,
            health,
            quota: options.max_pages,
        })
    }

    /// At every open of a file store (`RETENTION.md` §8: absent "after the
    /// next open following an unclean one"). The WAL is checkpointed into
    /// the database and truncated: what this build wrote there was deleted
    /// under `secure_delete`, so its released bytes are already zeroed.
    ///
    /// A store written BEFORE `secure_delete` (an `existing` file whose
    /// settings lack [`SCRUBBED`]) can hold released bytes in freed pages
    /// and in the free space of pages still in use; `VACUUM` rewrites the
    /// file without any of it, once, and the WAL it wrote is truncated. Row
    /// ids survive: every table's key is declared. A fresh store has
    /// nothing to scrub, and is marked at creation.
    ///
    /// A store that cannot write the rewrite (its quota, a full or failing
    /// medium) opens degraded rather than not at all -- its content stays
    /// readable and releasable -- and a later open scrubs it.
    fn scrub_at_open(&mut self, existing: bool) -> Result<(), StoreError> {
        // A fresh store has released nothing and is written only under
        // `secure_delete` from here on: it is marked so, in the WAL its new
        // schema went to, and left to the close to checkpoint.
        if !existing {
            if self.mark_scrubbed().is_err() {
                self.health = StorageHealth::Degraded;
            }
            return Ok(());
        }
        if self.truncate_wal().is_err() {
            self.health = StorageHealth::Degraded;
            return Ok(());
        }
        if self.scrubbed()? {
            return Ok(());
        }
        let rewrite = self
            .conn
            .execute_batch("VACUUM")
            .map_err(StoreError::from)
            .and_then(|()| self.mark_scrubbed());
        if rewrite.is_err() || self.truncate_wal().is_err() {
            self.health = StorageHealth::Degraded;
        }
        Ok(())
    }

    fn mark_scrubbed(&self) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, '1')",
            params![SCRUBBED],
        )?;
        Ok(())
    }

    fn scrubbed(&self) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM settings WHERE key = ?1",
                params![SCRUBBED],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Checkpoint the WAL into the database and truncate it to nothing, so
    /// the page images it held -- a released message's content among them
    /// -- leave the log. With `secure_delete` the released bytes are
    /// already zeroed in the page the checkpoint writes back.
    ///
    /// ONE CONNECTION IS ASSUMED. The store is opened once per process (the
    /// desktop client under its single-instance lock), and nothing else
    /// writes it. A reader elsewhere -- the desktop e2e tests read the
    /// running client's file -- can hold the log: SQLite then waits the
    /// busy timeout and answers busy IN THE RESULT ROW, not as an error,
    /// so the row is read and busy is a failure (#214's review, F2).
    fn truncate_wal(&self) -> Result<(), StoreError> {
        let busy: i64 = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        if busy != 0 {
            return Err(StoreError::LogNotTruncated);
        }
        Ok(())
    }

    /// After every release (`transport_terminal`, `mark_read`, `unkeep`):
    /// the bound `RETENTION.md` §8 asks for while the store is open is
    /// ONE RELEASE -- the content leaves the log before the call returns.
    /// Measured on the Android test device (SPIKE-008, A40, API 30): a
    /// delete with `secure_delete` and this truncate took 14-22 ms
    /// (median), 23-37 ms (95th percentile), against 4-5 ms without them;
    /// human-paced, so no larger batch is taken. A failure here does not
    /// undo the release, which is committed: the next release, the next
    /// open and the close each truncate again.
    fn release_done(&self) {
        let _ = self.truncate_wal();
    }

    /// Whether new unread content can still be committed durably.
    #[must_use]
    pub const fn health(&self) -> StorageHealth {
        self.health
    }

    /// Re-test the storage medium and clear degradation if it recovered.
    ///
    /// COMMITS a real row of the largest size a message can be, then
    /// commits its deletion, rather than reading a pragma or writing and
    /// rolling back: a full or read-only database answers `SELECT`
    /// perfectly well, a write that is rolled back is satisfied in the
    /// page cache and never reaches the medium, and a small row can fit
    /// where a 48 KiB message cannot. So the evidence is the durable
    /// commit of a payload-sized row under this store's own
    /// `synchronous=FULL`, and only that. Pinned by
    /// `recheck_health_stays_degraded_while_durable_writes_fail` (the
    /// medium refusing the write at commit) and
    /// `recheck_health_clears_degradation_when_the_medium_recovers`.
    ///
    /// The probe row is reserved metadata under `settings`, which no
    /// content read enumerates, and is gone again before this returns; a
    /// probe whose insert committed but whose deletion did not leaves
    /// the row for the next probe to overwrite and reports the failure.
    ///
    /// A PASSING PROBE IS NOT ALWAYS HEALTH. A store opened above its
    /// quota runs under the looser ceiling SQLite set (its size), and
    /// stays degraded until its used pages fit the quota asked for; this
    /// then compacts the file with `VACUUM` -- a full rewrite -- and
    /// applies the quota, read back (`within_quota`).
    ///
    /// # Errors
    /// Returns the underlying [`StoreError`] if the probe fails, having
    /// first recorded the degradation, or if compacting or applying the
    /// quota fails, which leaves the store degraded as it was.
    pub fn recheck_health(&mut self) -> Result<StorageHealth, StoreError> {
        let probe = (|| -> Result<(), rusqlite::Error> {
            let filler = "x".repeat(MAX_PAYLOAD_BYTES);
            let tx = self.conn.transaction()?;
            tx.execute(
                "INSERT INTO settings (key, value) VALUES ('__health_probe', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![filler],
            )?;
            tx.commit()?;
            let tx = self.conn.transaction()?;
            tx.execute("DELETE FROM settings WHERE key = '__health_probe'", [])?;
            tx.commit()
        })();

        match probe {
            Ok(()) if !self.within_quota()? => {
                self.health = StorageHealth::Degraded;
                Ok(self.health)
            }
            Ok(()) => {
                self.health = StorageHealth::Healthy;
                Ok(self.health)
            }
            Err(e) => Err(self.note_failure(e)),
        }
    }

    /// Whether SQLite enforces the page ceiling asked for, applying it if
    /// the database's USED pages now fit it.
    ///
    /// A store opened above its quota runs under a looser ceiling -- the
    /// size it had -- and releasing content frees pages without shrinking
    /// the file, so the ceiling asked for cannot be set until the file is
    /// compacted. Once the used pages fit, it is: `VACUUM`, then the
    /// quota, read back.
    fn within_quota(&mut self) -> Result<bool, StoreError> {
        let Some(quota) = self.quota else {
            return Ok(true);
        };
        let ceiling: i64 = self
            .conn
            .pragma_query_value(None, "max_page_count", |row| row.get(0))?;
        if ceiling == i64::from(quota) {
            return Ok(true);
        }
        let pages: i64 = self
            .conn
            .pragma_query_value(None, "page_count", |row| row.get(0))?;
        let free: i64 = self
            .conn
            .pragma_query_value(None, "freelist_count", |row| row.get(0))?;
        if pages - free > i64::from(quota) {
            return Ok(false);
        }
        self.conn.execute_batch("VACUUM")?;
        let applied: i64 =
            self.conn
                .pragma_update_and_check(None, "max_page_count", quota, |row| row.get(0))?;
        Ok(applied == i64::from(quota))
    }

    /// Record a storage failure and return it as a [`StoreError`].
    ///
    /// A constraint violation is NOT a storage failure: a duplicate
    /// `app_message_id` says the caller sent the same message twice, and
    /// the medium is fine. Degrading on it would release the human
    /// endpoint over an application bug.
    fn note_failure(&mut self, err: rusqlite::Error) -> StoreError {
        self.note_store_failure(StoreError::from(err))
    }

    fn note_store_failure(&mut self, err: StoreError) -> StoreError {
        if let StoreError::Sql(inner) = &err
            && is_medium_failure(inner)
        {
            self.health = StorageHealth::Degraded;
        }
        err
    }

    // ---------------------------------------------------------------
    // Outbound
    // ---------------------------------------------------------------

    /// Commit a pending-outbound record.
    ///
    /// Call this **before** invoking transport. The order is the contract
    /// (`RETENTION.md` §2): sending first would lose the message if the
    /// process died between the transport call and the record.
    ///
    /// # Errors
    /// Returns [`StoreError::Degraded`] while storage is degraded,
    /// [`StoreError::PayloadTooLarge`] above the transport ceiling,
    /// [`StoreError::TimestampOutOfRange`] if `created_at` cannot be
    /// represented, or a storage error.
    pub fn commit_pending_outbound(&mut self, new: &NewOutbound) -> Result<RowId, StoreError> {
        self.reject_if_degraded()?;
        check_payload(&new.payload)?;

        let (peer, endpoint, channel) = match &new.destination {
            OutboundDestination::Direct(d) => (
                Some(d.peer.as_str().to_owned()),
                d.endpoint.as_ref().map(|e| e.as_str().to_owned()),
                None,
            ),
            OutboundDestination::Broadcast(c) => (None, None, Some(c.as_str().to_owned())),
        };

        let result = self.conn.execute(
            "INSERT INTO pending_outbound
                 (app_message_id, transport_message_id, destination_peer,
                  destination_endpoint, channel_id, media_type, payload, created_at,
                  last_attempt_at, attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 0)",
            params![
                new.app_message_id.as_str(),
                new.transport_message_id.as_bytes().as_slice(),
                // A broadcast row has no peer; the column is NOT NULL, so
                // the channel row stores the empty string and the channel
                // column is what identifies it. Reading discriminates on
                // channel_id, never on this.
                peer.unwrap_or_default(),
                endpoint,
                channel,
                new.media_type.as_ref().map(MediaType::as_str),
                new.payload,
                sql_timestamp("created_at", new.created_at)?,
            ],
        );

        match result {
            Ok(_) => Ok(RowId::from_stored(self.conn.last_insert_rowid())),
            Err(e) => Err(self.note_failure(e)),
        }
    }

    /// Record that a send attempt was made.
    ///
    /// Metadata only. The message stays pending and stays durable —
    /// a failed attempt is exactly the case the durable copy exists for.
    ///
    /// # Errors
    /// Returns [`StoreError::TimestampOutOfRange`] if `at_ms` cannot be
    /// represented, or a storage error.
    pub fn record_attempt(&mut self, row_id: RowId, at_ms: u64) -> Result<(), StoreError> {
        let result = self.conn.execute(
            "UPDATE pending_outbound
                SET last_attempt_at = ?2, attempts = attempts + 1
              WHERE row_id = ?1",
            params![row_id.get(), sql_timestamp("at_ms", at_ms)?],
        );
        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(self.note_failure(e)),
        }
    }

    /// Record that transport will do no more with this message.
    ///
    /// The durable pending copy is deleted, because the state machine
    /// says [`Durability::Remove`]. It may remain in RAM for the current
    /// session so the conversation still renders.
    ///
    /// Idempotent: a duplicate terminal event for a row that is already
    /// gone succeeds, since the required end state is the one that
    /// already holds.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn transport_terminal(
        &mut self,
        row_id: RowId,
        cause: TerminalCause,
    ) -> Result<(), StoreError> {
        let mut message = OutboundMessage::composed();
        let durability = message.transport_terminal(cause);
        self.apply_outbound_durability(row_id, durability)
    }

    fn apply_outbound_durability(
        &mut self,
        row_id: RowId,
        durability: Durability,
    ) -> Result<(), StoreError> {
        match durability {
            // The state machine's answer, not this module's. If a future
            // amendment made some terminal cause stay durable, this arm
            // would start being taken and no edit here would be needed.
            Durability::Durable => Ok(()),
            Durability::Remove => {
                let result = self.conn.execute(
                    "DELETE FROM pending_outbound WHERE row_id = ?1",
                    params![row_id.get()],
                );
                match result {
                    Ok(_) => {
                        self.release_done();
                        Ok(())
                    }
                    Err(e) => Err(self.note_failure(e)),
                }
            }
        }
    }

    /// Every message still waiting to reach a transport-terminal state.
    ///
    /// What a restart reloads. Ordered by creation so the client retries
    /// in the order the human composed.
    ///
    /// # Errors
    /// Returns a storage error, or [`StoreError::Corrupt`] if a stored
    /// row no longer parses.
    pub fn pending_outbound(&self) -> Result<Vec<PendingOutbound>, StoreError> {
        let page = self.pending_outbound_page(None, PageLimits::default())?;
        if page.next.is_some() {
            return Err(StoreError::TooManyRows {
                use_instead: "pending_outbound_page",
            });
        }
        Ok(page.items)
    }

    /// One pending-outbound row, or `None` once it is gone (terminal, or
    /// never committed). The single-row read a sender needs to attempt
    /// one message, so nothing reassembles the whole table to find it.
    ///
    /// # Errors
    /// Returns a storage error, or [`StoreError::Corrupt`] if the row no
    /// longer parses.
    pub fn pending_outbound_row(
        &self,
        row_id: RowId,
    ) -> Result<Option<PendingOutbound>, StoreError> {
        self.conn
            .query_row(
                "SELECT row_id, app_message_id, destination_peer, destination_endpoint,
                        channel_id, media_type, payload, created_at, last_attempt_at,
                        attempts, transport_message_id
                   FROM pending_outbound
                  WHERE row_id = ?1",
                [row_id.get()],
                read_raw_pending,
            )
            .optional()?
            .map(pending_from)
            .transpose()
    }

    /// One page of pending outbound, resuming after `after`.
    ///
    /// # Errors
    /// Returns a storage error, [`StoreError::Corrupt`] if a stored row
    /// no longer parses, or [`StoreError::TimestampOutOfRange`] if the
    /// cursor carries a sort key this store cannot represent -- which a
    /// cursor this store handed out never does.
    pub fn pending_outbound_page(
        &self,
        after: Option<Cursor>,
        limits: PageLimits,
    ) -> Result<Page<PendingOutbound>, StoreError> {
        let (sort_key, row_id) = cursor_bounds(after)?;
        // One past the page, so a full page can tell "exactly this many"
        // from "more to come" without a second query.
        let fetch = i64::try_from(limits.max_records().saturating_add(1)).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT row_id, app_message_id, destination_peer, destination_endpoint, channel_id,
                    media_type, payload, created_at, last_attempt_at, attempts,
                    transport_message_id
               FROM pending_outbound
              WHERE (created_at, row_id) > (?1, ?2)
              ORDER BY created_at, row_id
              LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![sort_key, row_id, fetch], read_raw_pending)?;

        let mut out = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        for row in rows {
            let raw = row?;
            // The first row of a page always goes in, even alone over
            // budget: stalling the enumeration on one large message is
            // worse than one page being one message too big.
            if !out.is_empty()
                && (out.len() >= limits.max_records()
                    || bytes.saturating_add(raw.6.len()) > limits.max_bytes())
            {
                more = true;
                break;
            }
            bytes = bytes.saturating_add(raw.6.len());
            out.push(pending_from(raw)?);
        }
        Ok(Page {
            next: more.then(|| Cursor {
                sort_key: out.last().map_or(0, |r| r.created_at),
                row_id: out.last().map_or(RowId::from_stored(0), |r| r.row_id),
            }),
            items: out,
        })
    }

    // ---------------------------------------------------------------
    // Inbound
    // ---------------------------------------------------------------

    /// Commit a received message as unread.
    ///
    /// Call this **before** normal UI presentation or notification, so a
    /// message the user is told about is one the store already holds.
    ///
    /// # Errors
    /// Returns [`StoreError::Degraded`] while storage cannot hold unread
    /// content — the caller must then degrade the human endpoint rather
    /// than keep accepting a stream it cannot retain —
    /// [`StoreError::PayloadTooLarge`] above the transport ceiling,
    /// [`StoreError::TimestampOutOfRange`] if `received_at` cannot be
    /// represented, or a storage error.
    pub fn commit_unread_inbound(&mut self, new: &NewInbound) -> Result<RowId, StoreError> {
        self.reject_if_degraded()?;
        check_payload(&new.payload)?;

        // A message read here and not kept leaves a read pair; a later
        // copy is the same message and is not unread again (STATE.md
        // `read_pairs`). The check is IN the insert, so nothing can slip
        // between them.
        let result = self.conn.execute(
            "INSERT INTO unread_inbound
                 (app_message_id, source_peer, source_endpoint, channel_id,
                  media_type, payload, received_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
              WHERE NOT EXISTS (
                    SELECT 1 FROM read_pairs
                     WHERE source_peer = ?2
                       AND source_endpoint_key = IFNULL(?3, '')
                       AND channel_key = IFNULL(?4, '')
                       AND app_message_id = ?1)",
            params![
                new.app_message_id.as_str(),
                new.origin.peer.as_str(),
                new.origin
                    .endpoint
                    .as_ref()
                    .map(interweave_transport_api::EndpointId::as_str),
                new.origin
                    .channel
                    .as_ref()
                    .map(interweave_transport_api::ChannelId::as_str),
                new.media_type.as_ref().map(MediaType::as_str),
                new.payload,
                sql_timestamp("received_at", new.received_at)?,
            ],
        );

        match result {
            Ok(0) => Err(StoreError::AlreadyRead),
            Ok(_) => Ok(RowId::from_stored(self.conn.last_insert_rowid())),
            Err(e) => Err(self.note_failure(e)),
        }
    }

    /// Enter local read state, deleting the durable unread copy.
    ///
    /// Returns the content as a [`ReadEphemeral`] so the current session
    /// can still render it and the receiver can still choose `Keep`. The
    /// deletion happens in the same transaction as the read, so there is
    /// no window in which the row is returned but survives.
    ///
    /// Generates nothing to send: `read` is a local UI state, there is no
    /// read receipt, and it does not prove a human perceived anything.
    ///
    /// # Errors
    /// Returns [`StoreError::NoSuchRow`] if the row is not unread,
    /// [`StoreError::TimestampOutOfRange`] if `at_ms` cannot be
    /// represented -- refused BEFORE the durable row is deleted, so the
    /// caller may retry with a usable clock -- [`StoreError::Corrupt`] for
    /// a stored row this build cannot decode, which is parsed before it is
    /// deleted, or a storage error.
    pub fn mark_read(&mut self, row_id: RowId, at_ms: u64) -> Result<ReadEphemeral, StoreError> {
        // BEFORE THE DELETE, because this value leaves here inside the
        // `ReadEphemeral` and `keep` refuses it there. Unchecked, a
        // nonsense `at_ms` destroyed the durable unread row and then made
        // the message permanently unkeepable: `keep` failed on `read_at`,
        // and `ReadEphemeral`'s fields are crate-private, so the caller
        // could neither repair the value nor recover the row. Refusing
        // here costs the caller a retry; refusing there cost the message.
        // ADR-0044's "degrade rather than silently violate" is the rule
        // this lands on. Review finding on PR #86.
        sql_timestamp("read_at", at_ms)?;

        let mut message = InboundMessage::committed_unread();
        let durability = message.mark_read();

        // The whole read-and-delete is one closure returning StoreError so
        // the row is PARSED BEFORE IT IS DELETED. Parsing afterwards would
        // mean a row this build cannot decode is destroyed on the way to
        // reporting that it could not be decoded.
        let result = (|| -> Result<Option<ReadEphemeral>, StoreError> {
            let tx = self.conn.transaction()?;
            let row = tx
                .query_row(
                    "SELECT app_message_id, source_peer, source_endpoint, channel_id,
                            media_type, payload, received_at
                       FROM unread_inbound WHERE row_id = ?1",
                    params![row_id.get()],
                    |r| {
                        Ok(InboundColumns {
                            app_message_id: r.get(0)?,
                            peer: r.get(1)?,
                            endpoint: r.get(2)?,
                            channel: r.get(3)?,
                            media_type: r.get(4)?,
                            payload: r.get(5)?,
                            received_at: r.get(6)?,
                        })
                    },
                )
                .optional()?;

            let Some(columns) = row else {
                tx.rollback()?;
                return Ok(None);
            };

            let held = columns.into_read(at_ms)?;

            if durability == Durability::Remove {
                tx.execute(
                    "DELETE FROM unread_inbound WHERE row_id = ?1",
                    params![row_id.get()],
                )?;
                record_pair(&tx, &held, at_ms)?;
            }
            tx.commit()?;
            Ok(Some(held))
        })();

        match result {
            Ok(Some(held)) => {
                self.release_done();
                Ok(held)
            }
            Ok(None) => Err(StoreError::NoSuchRow),
            Err(e) => Err(self.note_store_failure(e)),
        }
    }

    /// The receiver keeps a message they have read.
    ///
    /// Takes a [`ReadEphemeral`] and nothing else. A remote sender has no
    /// way to produce one, and neither does a notification action on a
    /// message that was never opened — see that type's documentation for
    /// why this is enforcement rather than a check.
    ///
    /// # Errors
    /// Returns [`StoreError::Degraded`], [`StoreError::KeepRefused`] if
    /// the state machine refuses, [`StoreError::TimestampOutOfRange`] if
    /// `at_ms` cannot be represented, [`StoreError::IdentityConflict`] if
    /// the upsert matches no row because this peer, on this endpoint and
    /// channel, already used that `app_message_id` for different
    /// content, or a storage error.
    ///
    /// The two timestamps carried by `held` were refused on the way in, so
    /// they cannot fail here.
    ///
    /// The conflict target is four columns, so the same peer reusing the id
    /// on a DIFFERENT endpoint or channel is two rows and no conflict -- structural
    /// rather than tested through this method, since the only test of it
    /// goes through `commit_unread_inbound`. The collision itself is the
    /// outcome of this statement's `WHERE` clause.
    ///
    /// An earlier version of this block put all of that inside an em-dash
    /// pair, which left "or a storage error" attached to the wrong clause
    /// five lines from the list it belongs to -- the same displaced-structure
    /// defect as the sibling block, reintroduced here by the commit that
    /// fixed it there. Review findings on PR #86.
    pub fn keep(&mut self, held: &ReadEphemeral, at_ms: u64) -> Result<RowId, StoreError> {
        self.reject_if_degraded()?;

        // Replay the exact transition through the state machine. It
        // cannot refuse a message reached this way today, and that is the
        // point: if a future amendment narrowed `keep`, the store would
        // start refusing without this module being edited.
        let mut message = InboundMessage::committed_unread();
        message.mark_read();
        let durability = message.keep().map_err(StoreError::KeepRefused)?;
        if durability != Durability::Durable {
            return Err(StoreError::KeepRefused(
                interweave_human_core::retention::KeepRefused::ContentNoLongerHeld,
            ));
        }

        // ON CONFLICT because the state machine treats keeping an
        // already-kept message as fine, and a UI can produce a second
        // Keep from one double-click. Failing here would make the store
        // stricter than the contract it implements.
        //
        // But idempotent means SAME MESSAGE, and the conflict target is
        // remote-controlled data. `app_message_id` is HumanChatV2's
        // application identity, chosen by the sender — so the WHERE is
        // what separates "this exact message again" from "a different
        // message wearing an id this peer already used". Without it the
        // upsert refreshed the older row's timestamps, left its body in
        // place, and reported success for a message that never reached
        // durable kept state.
        //
        // A conflict that fails the WHERE updates no row, so RETURNING
        // yields nothing and the caller is told, rather than handed
        // someone else's row id. The endpoint and channel clauses are
        // implied by the conflict target since both joined the key
        // (the generated keys collapse only NULL, and '' is outside both
        // grammars); they stay so the WHERE reads as the whole identity
        // comparison it is.
        // RETURNING rather than last_insert_rowid(): that counter is not
        // updated when an upsert takes the UPDATE path, so it would hand
        // back whichever row was inserted most recently — a different
        // message's id, if anything was committed in between.
        let result = self.conn.query_row(
            "INSERT INTO kept_inbound
                 (app_message_id, source_peer, source_endpoint, channel_id,
                  media_type, payload, received_at, read_at, kept_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(source_peer, source_endpoint_key, channel_key, app_message_id)
             DO UPDATE SET
                 read_at = excluded.read_at,
                 kept_at = excluded.kept_at
             WHERE kept_inbound.source_endpoint IS excluded.source_endpoint
               AND kept_inbound.channel_id      IS excluded.channel_id
               AND kept_inbound.media_type      IS excluded.media_type
               AND kept_inbound.received_at      = excluded.received_at
               AND kept_inbound.payload          = excluded.payload
             RETURNING row_id",
            params![
                held.app_message_id.as_str(),
                held.origin.peer.as_str(),
                held.origin
                    .endpoint
                    .as_ref()
                    .map(interweave_transport_api::EndpointId::as_str),
                held.origin
                    .channel
                    .as_ref()
                    .map(interweave_transport_api::ChannelId::as_str),
                held.media_type.as_ref().map(MediaType::as_str),
                held.payload,
                sql_timestamp("received_at", held.received_at)?,
                sql_timestamp("read_at", held.read_at)?,
                sql_timestamp("at_ms", at_ms)?,
            ],
            |r| r.get::<_, i64>(0),
        );

        match result {
            Ok(row_id) => Ok(RowId::from_stored(row_id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(StoreError::IdentityConflict {
                app_message_id: held.app_message_id.as_str().to_owned(),
                source_peer: held.origin.peer.as_str().to_owned(),
            }),
            Err(e) => Err(self.note_failure(e)),
        }
    }

    /// The receiver removes `Keep`.
    ///
    /// Deletion is immediate — now, not at a later cleanup pass. The
    /// content comes back as a [`ReadEphemeral`]: the message is read and
    /// unkept, which is exactly the state `keep` accepts, so the receiver
    /// can keep it again in this session (agreed Q6, relay seq 11163).
    /// Across a restart the copy is gone, like any read-unkept message
    /// (RETENTION.md §9 case 11). `None` when no such row is kept: a
    /// second Unkeep of one message is not an error.
    ///
    /// # Errors
    /// Returns a storage error, or [`StoreError::Corrupt`] for a row this
    /// build cannot decode, which is then left in place.
    pub fn unkeep(
        &mut self,
        row_id: RowId,
        at_ms: u64,
    ) -> Result<Option<ReadEphemeral>, StoreError> {
        sql_timestamp("at", at_ms)?;
        let mut message = InboundMessage::committed_unread();
        message.mark_read();
        let _ = message.keep();
        let durability = message.unkeep();

        // Parsed before the delete, in one transaction, for the reason
        // `mark_read` gives: a row this build cannot decode must not be
        // destroyed on the way to reporting that.
        let result = (|| -> Result<Option<ReadEphemeral>, StoreError> {
            let tx = self.conn.transaction()?;
            let row = tx
                .query_row(
                    "SELECT app_message_id, source_peer, source_endpoint, channel_id,
                            media_type, payload, received_at, read_at
                       FROM kept_inbound WHERE row_id = ?1",
                    params![row_id.get()],
                    |r| {
                        Ok((
                            InboundColumns {
                                app_message_id: r.get(0)?,
                                peer: r.get(1)?,
                                endpoint: r.get(2)?,
                                channel: r.get(3)?,
                                media_type: r.get(4)?,
                                payload: r.get(5)?,
                                received_at: r.get(6)?,
                            },
                            r.get::<_, i64>(7)?,
                        ))
                    },
                )
                .optional()?;

            let Some((columns, read_at)) = row else {
                tx.rollback()?;
                return Ok(None);
            };
            let held = columns.into_read(stored_ms("read_at", read_at)?)?;

            if durability == Durability::Remove {
                tx.execute(
                    "DELETE FROM kept_inbound WHERE row_id = ?1",
                    params![row_id.get()],
                )?;
                record_pair(&tx, &held, at_ms)?;
            }
            tx.commit()?;
            Ok(Some(held))
        })();

        let released = result.map_err(|e| self.note_store_failure(e))?;
        if released.is_some() {
            self.release_done();
        }
        Ok(released)
    }

    /// Every unread inbound message, oldest first.
    ///
    /// # Errors
    /// Returns a storage error, or [`StoreError::Corrupt`] for an
    /// unparseable stored row.
    pub fn unread_inbound(&self) -> Result<Vec<StoredInbound>, StoreError> {
        self.all_of("unread_inbound", "unread_inbound_page")
    }

    /// One page of unread inbound, resuming after `after`.
    ///
    /// # Errors
    /// Returns a storage error, [`StoreError::Corrupt`] for an unparseable
    /// stored row, or [`StoreError::TimestampOutOfRange`] if the cursor
    /// carries a sort key this store cannot represent -- which a cursor
    /// this store handed out never does.
    pub fn unread_inbound_page(
        &self,
        after: Option<Cursor>,
        limits: PageLimits,
    ) -> Result<Page<StoredInbound>, StoreError> {
        self.read_inbound_table("unread_inbound", after, limits)
    }

    /// Every inbound message the receiver kept, oldest first.
    ///
    /// # Errors
    /// Returns a storage error, or [`StoreError::Corrupt`] for an
    /// unparseable stored row.
    pub fn kept_inbound(&self) -> Result<Vec<StoredInbound>, StoreError> {
        self.all_of("kept_inbound", "kept_inbound_page")
    }

    /// One page of kept inbound, resuming after `after`.
    ///
    /// # Errors
    /// Returns a storage error, [`StoreError::Corrupt`] for an
    /// unparseable stored row, or [`StoreError::TimestampOutOfRange`] if the
    /// cursor carries a sort key this store cannot represent -- which a
    /// cursor this store handed out never does.
    pub fn kept_inbound_page(
        &self,
        after: Option<Cursor>,
        limits: PageLimits,
    ) -> Result<Page<StoredInbound>, StoreError> {
        self.read_inbound_table("kept_inbound", after, limits)
    }

    /// The whole table, for the small case, refusing a second page.
    fn all_of(
        &self,
        table: &str,
        use_instead: &'static str,
    ) -> Result<Vec<StoredInbound>, StoreError> {
        let page = self.read_inbound_table(table, None, PageLimits::default())?;
        if page.next.is_some() {
            return Err(StoreError::TooManyRows { use_instead });
        }
        Ok(page.items)
    }

    fn read_inbound_table(
        &self,
        table: &str,
        after: Option<Cursor>,
        limits: PageLimits,
    ) -> Result<Page<StoredInbound>, StoreError> {
        // `table` is one of two literals chosen by this module, never
        // caller input; SQLite does not bind identifiers.
        let kept = table == "kept_inbound";
        let sql = if kept {
            "SELECT row_id, app_message_id, source_peer, source_endpoint, channel_id,
                    media_type, payload, received_at, read_at, kept_at
               FROM kept_inbound
              WHERE (received_at, row_id) > (?1, ?2)
              ORDER BY received_at, row_id
              LIMIT ?3"
        } else {
            "SELECT row_id, app_message_id, source_peer, source_endpoint, channel_id,
                    media_type, payload, received_at, NULL, NULL
               FROM unread_inbound
              WHERE (received_at, row_id) > (?1, ?2)
              ORDER BY received_at, row_id
              LIMIT ?3"
        };

        let (sort_key, row_id) = cursor_bounds(after)?;
        let fetch = i64::try_from(limits.max_records().saturating_add(1)).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![sort_key, row_id, fetch], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Vec<u8>>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, Option<i64>>(9)?,
            ))
        })?;

        let mut out = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        for row in rows {
            let (id, amid, peer, endpoint, channel, media_type, payload, received, read, keptat) =
                row?;
            if !out.is_empty()
                && (out.len() >= limits.max_records()
                    || bytes.saturating_add(payload.len()) > limits.max_bytes())
            {
                more = true;
                break;
            }
            bytes = bytes.saturating_add(payload.len());
            out.push(StoredInbound {
                row_id: RowId::from_stored(id),
                app_message_id: AppMessageId::parse(amid)?,
                origin: InboundOrigin {
                    peer: TransportIdentity::parse(peer)
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                    endpoint: endpoint
                        .map(EndpointId::parse)
                        .transpose()
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                    channel: channel
                        .map(ChannelId::parse)
                        .transpose()
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                },
                media_type: parse_media_type(media_type)?,
                payload,
                received_at: stored_ms("received_at", received)?,
                read_at: read.map(|v| stored_ms("read_at", v)).transpose()?,
                kept_at: keptat.map(|v| stored_ms("kept_at", v)).transpose()?,
            });
        }
        Ok(Page {
            next: more.then(|| Cursor {
                sort_key: out.last().map_or(0, |r| r.received_at),
                row_id: out.last().map_or(RowId::from_stored(0), |r| r.row_id),
            }),
            items: out,
        })
    }

    // ---------------------------------------------------------------
    // Backup
    // ---------------------------------------------------------------

    /// The content a future explicit encrypted backup may include.
    ///
    /// Inbound unread and inbound kept, and nothing else. Pending
    /// outbound is excluded so a restored or second device cannot become
    /// an implicit delayed-send or replay source (`RETENTION.md` §6) —
    /// which is why this is a method on the store rather than left to
    /// whoever writes the backup tool to remember.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn backup_eligible_content(&self) -> Result<Vec<StoredInbound>, StoreError> {
        let mut out = self.unread_inbound()?;
        out.extend(self.kept_inbound()?);
        Ok(out)
    }

    /// One page of backup-eligible content, resuming after `after`.
    ///
    /// Walks unread and then kept. The cursor names which table it is in
    /// because the two have independent row-id spaces, so a position on
    /// its own would let a resumed backup duplicate or skip.
    ///
    /// Pending outbound is deliberately absent, for the reason
    /// [`Self::backup_eligible_content`] gives.
    ///
    /// # Errors
    /// Returns a storage error, [`StoreError::Corrupt`] for an unparseable
    /// stored row, or [`StoreError::TimestampOutOfRange`] if the cursor
    /// carries a sort key this store cannot represent -- inherited from
    /// both tables it walks, and a cursor this store handed out never does.
    pub fn backup_eligible_page(
        &self,
        after: Option<BackupCursor>,
        limits: PageLimits,
    ) -> Result<Page<StoredInbound, BackupCursor>, StoreError> {
        let position = after.unwrap_or(BackupCursor {
            table: BackupTable::Unread,
            within: None,
        });

        match position.table {
            BackupTable::Unread => {
                let page = self.unread_inbound_page(position.within, limits)?;
                Ok(Page {
                    items: page.items,
                    // Exhausting unread hands back the START of kept
                    // rather than `None`: reporting the enumeration
                    // finished halfway is how a backup silently loses the
                    // kept half.
                    next: Some(BackupCursor {
                        table: page.next.map_or(BackupTable::Kept, |_| BackupTable::Unread),
                        within: page.next,
                    }),
                })
            }
            BackupTable::Kept => {
                let page = self.kept_inbound_page(position.within, limits)?;
                Ok(Page {
                    items: page.items,
                    next: page.next.map(|within| BackupCursor {
                        table: BackupTable::Kept,
                        within: Some(within),
                    }),
                })
            }
        }
    }

    fn reject_if_degraded(&self) -> Result<(), StoreError> {
        match self.health {
            StorageHealth::Healthy => Ok(()),
            StorageHealth::Degraded => Err(StoreError::Degraded),
        }
    }
}

/// The store's directory, judged by profile-config's one walk (ADR-0028 A
/// 2026-10-08) and created owner-only if missing; answers where it is on
/// disk, to open under.
///
/// Owner-only because this directory holds message content, owned by
/// this uid, and under ancestors and links only root or this uid can
/// change -- a sound mode under an ancestor another account can write is
/// a directory that account can rename away and replace.
///
/// JUDGED BEFORE ANYTHING IS CREATED, as the profile lock's directories
/// are: a missing directory is created only beneath an existing ancestor
/// that meets the rule, so a refusal leaves the tree as it found it
/// (`a_refused_state_directory_creates_nothing`).
///
/// REFUSED RATHER THAN TIGHTENED. A pre-existing directory — restored,
/// copied, made by an older build, or by hand — carries whatever mode it
/// has; content that has been broadly readable should be treated as
/// exposed, and quietly narrowing the mode would hide that it ever was.
///
/// The desktop's `HumanClientLock` judges the same directory first, by
/// the same walk; this judgement is the store's own, so a caller that
/// opens it without that lock gets the rule too.
fn private_dir(dir: &Path) -> Result<std::path::PathBuf, StoreError> {
    let dir = &beyond_missing(dir)?;
    // The missing components, innermost first, and the nearest that is
    // there; an empty ancestor is the working directory.
    let mut missing = Vec::new();
    let mut existing = Path::new(".");
    for ancestor in dir.ancestors() {
        let ancestor = if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            ancestor
        };
        if absent(ancestor)? {
            missing.push(ancestor);
        } else {
            existing = ancestor;
            break;
        }
    }
    if !missing.is_empty() {
        profile_config::resolve_guarded_dir(existing)
            .map_err(|e| StoreError::from_persist(existing, e))?;
        missing.reverse();
        create_each(&missing)?;
    }
    profile_config::resolve_owned_private_dir(dir).map_err(|e| StoreError::from_persist(dir, e))
}

/// `dir` with every `.` and every `..` that follows a MISSING component
/// taken out by text, so each missing component is a name this process
/// will make.
///
/// Exact, not an approximation: a component that is not there cannot be
/// a link, so `new/..` is its parent and nothing else. Left in, `mkdir`
/// answered "exists" for `x/new/..` or `x/new/../wide` -- an existing
/// directory -- and it was judged as one of ours and refused, where the
/// same path opened before
/// (`a_path_through_a_missing_directory_and_back_opens_as_before`). A
/// `..` after a component that IS there is kept, for the walk to resolve
/// as the kernel does.
fn beyond_missing(dir: &Path) -> Result<std::path::PathBuf, StoreError> {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for component in dir.components() {
        let here = if out.as_os_str().is_empty() {
            Path::new(".")
        } else {
            out.as_path()
        };
        let missing = absent(here)?;
        match component {
            Component::CurDir if missing => {}
            Component::ParentDir if missing => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Ok(out)
}

/// Create `components`, outermost first, each owner-only and each by
/// itself rather than in one recursive call.
///
/// ONE AT A TIME because the ancestor judged before them may be a sticky
/// directory others can create in: a component that appears between that
/// judgement and its creation is ADOPTED only if it is a private directory
/// of this uid's -- another of our processes making it -- and otherwise
/// refused before anything is made inside it. A recursive create adopted
/// any directory it found and made ours inside it; the refusal came only
/// after, leaving our directory in another account's
/// (`a_component_that_appears_unprivate_is_refused_before_anything_is_made_in_it`).
fn create_each(components: &[&Path]) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        for component in components {
            match std::fs::DirBuilder::new()
                .mode(profile_config::OWNER_ONLY_DIR)
                .create(component)
            {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    profile_config::resolve_owned_private_dir(component)
                        .map_err(|e| StoreError::from_persist(component, e))?;
                }
                Err(e) => return Err(StoreError::Io(e)),
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        // Refusing beats creating a directory of message content this
        // build cannot protect.
        let _ = components;
        Err(StoreError::UnsupportedPlatform)
    }
}

/// Whether nothing at all is at `path` -- not even a dangling link,
/// which is something there to be judged and refused.
fn absent(path: &Path) -> Result<bool, StoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(StoreError::Io(e)),
    }
}

/// Refuse a file holding message content that others can reach.
fn require_owner_only(path: &std::path::Path, what: &str) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path)
            .map_err(StoreError::Io)?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(StoreError::PermissionsTooOpen {
                what: what.to_owned(),
                mode: mode & 0o777,
            });
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, what);
        Err(StoreError::UnsupportedPlatform)
    }
}

/// Read a stored media type back through the validating type.
///
/// A row this build wrote is always valid, so this only ever speaks for
/// a database that was edited, restored, or corrupted — which is exactly
/// when durable state should not be handed to a caller as if it had come
/// out of the boundary that validates it.
fn parse_media_type(stored: Option<String>) -> Result<Option<MediaType>, StoreError> {
    stored
        .map(MediaType::parse)
        .transpose()
        .map_err(|e| StoreError::Corrupt(e.to_string()))
}

/// The `user_version` an existing SQLite file declares, read from its
/// header (offset 60, four bytes big-endian) without opening it as a
/// database. `None` for a missing, empty or non-SQLite file: those reach
/// the ordinary open, which refuses a non-database before writing.
fn header_user_version(path: &Path) -> Result<Option<i64>, StoreError> {
    use std::io::Read as _;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(StoreError::Io(e)),
    };
    let mut header = [0_u8; 64];
    let mut read = 0;
    while read < header.len() {
        match file.read(&mut header[read..]).map_err(StoreError::Io)? {
            0 => return Ok(None),
            n => read += n,
        }
    }
    if &header[..16] != b"SQLite format 3\0" {
        return Ok(None);
    }
    Ok(Some(i64::from(i32::from_be_bytes([
        header[60], header[61], header[62], header[63],
    ]))))
}

/// How many read pairs the store holds, at most: past it the oldest goes,
/// inside the transaction that records the newest (architect-cto's Q5
/// ruling, relay seq 11163).
pub const READ_PAIR_CAP: usize = 4096;

/// Record that `held` was read here and not kept, at `at_ms`: its origin
/// and application id, and nothing of its content. A pair already held
/// for the message is replaced, so it counts as the newest; then the
/// oldest past [`READ_PAIR_CAP`] are evicted, in the caller's transaction.
fn record_pair(
    tx: &rusqlite::Transaction<'_>,
    held: &ReadEphemeral,
    at_ms: u64,
) -> Result<(), StoreError> {
    let peer = held.origin.peer.as_str();
    let endpoint = held
        .origin
        .endpoint
        .as_ref()
        .map(interweave_transport_api::EndpointId::as_str);
    let channel = held
        .origin
        .channel
        .as_ref()
        .map(interweave_transport_api::ChannelId::as_str);
    let id = held.app_message_id.as_str();
    tx.execute(
        "DELETE FROM read_pairs
          WHERE source_peer = ?2
            AND source_endpoint_key = IFNULL(?3, '')
            AND channel_key = IFNULL(?4, '')
            AND app_message_id = ?1",
        params![id, peer, endpoint, channel],
    )?;
    tx.execute(
        "INSERT INTO read_pairs (app_message_id, source_peer, source_endpoint, channel_id, at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, peer, endpoint, channel, sql_timestamp("at", at_ms)?],
    )?;
    // Only the overflow is deleted, oldest first by `pair_id`: one row in
    // the steady state, found through the primary key.
    let held_pairs: i64 = tx.query_row("SELECT COUNT(*) FROM read_pairs", [], |r| r.get(0))?;
    let over = held_pairs - i64::try_from(READ_PAIR_CAP).unwrap_or(i64::MAX);
    if over > 0 {
        tx.execute(
            "DELETE FROM read_pairs
              WHERE pair_id IN (SELECT pair_id FROM read_pairs ORDER BY pair_id LIMIT ?1)",
            params![over],
        )?;
    }
    Ok(())
}

/// An inbound row's columns as SQLite holds them, before validation: what
/// `mark_read` reads from `unread_inbound` and `unkeep` from
/// `kept_inbound`, so both build a [`ReadEphemeral`] the same way.
struct InboundColumns {
    app_message_id: String,
    peer: String,
    endpoint: Option<String>,
    channel: Option<String>,
    media_type: Option<String>,
    payload: Vec<u8>,
    received_at: i64,
}

impl InboundColumns {
    /// Validate the columns into the content `keep` accepts, read at
    /// `read_at`.
    fn into_read(self, read_at: u64) -> Result<ReadEphemeral, StoreError> {
        Ok(ReadEphemeral {
            app_message_id: AppMessageId::parse(self.app_message_id)?,
            origin: InboundOrigin {
                peer: TransportIdentity::parse(self.peer)
                    .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                endpoint: self
                    .endpoint
                    .map(EndpointId::parse)
                    .transpose()
                    .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                channel: self
                    .channel
                    .map(ChannelId::parse)
                    .transpose()
                    .map_err(|e| StoreError::Corrupt(e.to_string()))?,
            },
            media_type: parse_media_type(self.media_type)?,
            payload: self.payload,
            received_at: stored_ms("received_at", self.received_at)?,
            read_at,
        })
    }
}

/// The `(sort_key, row_id)` a cursor resumes after.
///
/// `None` starts before every row. `-1` rather than `0` because a
/// timestamp of zero is legal and `> (0, 0)` would skip it.
///
/// CHECKED LIKE EVERY OTHER TIMESTAMP. Saturating the cursor's sort key
/// to `i64::MAX` turned an unrepresentable cursor into "past everything",
/// which returns an empty page with no continuation -- the silent
/// end-of-enumeration this store has already been bitten by once, from a
/// zero page ceiling. A cursor always comes from a previous page, so a
/// value this large is a caller error and is told rather than answered
/// with nothing. Review finding.
fn cursor_bounds(after: Option<Cursor>) -> Result<(i64, i64), StoreError> {
    match after {
        None => Ok((-1, -1)),
        Some(c) => Ok((
            sql_timestamp("cursor sort_key", c.sort_key)?,
            c.row_id.get(),
        )),
    }
}

fn check_payload(payload: &[u8]) -> Result<(), StoreError> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(StoreError::PayloadTooLarge {
            got: payload.len(),
            max: MAX_PAYLOAD_BYTES,
        });
    }
    Ok(())
}

/// A pending-outbound row as SQLite returns it, in the column order of
/// the two queries that read it.
type RawPending = (
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Vec<u8>,
    i64,
    Option<i64>,
    i64,
    Vec<u8>,
);

fn read_raw_pending(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawPending> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
        r.get(10)?,
    ))
}

fn pending_from(raw: RawPending) -> Result<PendingOutbound, StoreError> {
    let (id, amid, peer, endpoint, channel, media_type, payload, created, last, attempts, tid) =
        raw;
    let destination = if let Some(c) = channel {
        OutboundDestination::Broadcast(
            ChannelId::parse(c).map_err(|e| StoreError::Corrupt(e.to_string()))?,
        )
    } else {
        let peer =
            TransportIdentity::parse(peer).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let endpoint = endpoint
            .map(EndpointId::parse)
            .transpose()
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        OutboundDestination::Direct(DirectDestination { peer, endpoint })
    };
    Ok(PendingOutbound {
        row_id: RowId::from_stored(id),
        app_message_id: AppMessageId::parse(amid)?,
        transport_message_id: stored_message_id(&tid)?,
        destination,
        media_type: parse_media_type(media_type)?,
        payload,
        created_at: stored_ms("created_at", created)?,
        last_attempt_at: last.map(|v| stored_ms("last_attempt_at", v)).transpose()?,
        attempts: stored_count("attempts", attempts)?,
    })
}

/// Whether an error says the storage MEDIUM failed.
///
/// The distinction decides whether the human endpoint gets released. A
/// full disk, an I/O error, or a corrupt file means this client can no
/// longer claim to be a durable receiver. A constraint violation means
/// the caller made a mistake and the medium is perfectly healthy —
/// degrading on that would take the client offline over a duplicate id.
fn is_medium_failure(err: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode;
    match err {
        rusqlite::Error::SqliteFailure(e, _) => matches!(
            e.code,
            ErrorCode::DiskFull
                | ErrorCode::SystemIoFailure
                | ErrorCode::CannotOpen
                | ErrorCode::OutOfMemory
                | ErrorCode::ReadOnly
                | ErrorCode::DatabaseCorrupt
                | ErrorCode::NotADatabase
                | ErrorCode::DatabaseBusy
                | ErrorCode::DatabaseLocked
        ),
        _ => false,
    }
}

/// A stored transport id: exactly sixteen bytes, or the row is corrupt.
/// The column's CHECK holds this for every row v6 wrote; a hand-edited
/// database is refused here rather than sent under a truncated id.
fn stored_message_id(bytes: &[u8]) -> Result<MessageId, StoreError> {
    <[u8; MessageId::LEN]>::try_from(bytes)
        .map(MessageId::from_bytes)
        .map_err(|_| {
            StoreError::Corrupt(format!(
                "transport_message_id is {} bytes, not {}",
                bytes.len(),
                MessageId::LEN
            ))
        })
}

/// The directory whose privacy protects `path`.
///
/// AN EMPTY PARENT IS THE WORKING DIRECTORY, not "no parent".
/// `Path::new("messages.db").parent()` is `Some("")`, so treating that as
/// absent skipped every directory check for a bare relative path — in a
/// shared or attacker-writable working directory, exactly the case that
/// most needed them.
fn private_parent_of(path: &std::path::Path) -> &std::path::Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("."),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod private_dir_tests {
    use super::{create_each, private_dir};
    use crate::StoreError;

    #[test]
    fn a_component_that_appears_unprivate_is_refused_before_anything_is_made_in_it() {
        // The race made deterministic: `a` appears between the judgement
        // of its parent and its own creation, readable by others.
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir_in("/tmp").expect("tempdir under /tmp");
        let a = dir.path().join("a");
        let b = a.join("b");
        std::fs::create_dir(&a).expect("mkdir");
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        match create_each(&[&a, &b]) {
            Err(StoreError::DirectoryNotPrivate { path, .. }) => assert_eq!(path, a),
            other => panic!("expected the appeared component refused, got {other:?}"),
        }
        assert!(!b.exists(), "nothing was made inside it");

        // Our own, private, is adopted: another of our processes made it.
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        create_each(&[&a, &b]).expect("a private directory of ours is adopted");
        assert!(b.is_dir());
    }

    #[test]
    fn the_store_opens_under_the_directory_as_resolved_not_the_configured_text() {
        // Every open after the judgement is made under what this returns;
        // a configured path through a link must come back without it, or
        // a link repointed after the judgement would redirect the open.
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir_in("/tmp").expect("tempdir under /tmp");
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("state")).expect("mkdir");
        for d in [&real, &real.join("state")] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        }
        std::os::unix::fs::symlink(&real, dir.path().join("link")).expect("link");

        let resolved = private_dir(&dir.path().join("link").join("state")).expect("judged sound");
        assert_eq!(
            resolved,
            std::fs::canonicalize(real.join("state")).expect("canonical")
        );
    }
}

#[cfg(test)]
mod parent_tests {
    use super::private_parent_of;
    use std::path::Path;

    #[test]
    fn a_bare_name_resolves_to_the_working_directory_not_to_nothing() {
        assert_eq!(
            private_parent_of(Path::new("messages.db")),
            Path::new("."),
            "a bare name is protected by the working directory"
        );
        assert_eq!(
            private_parent_of(Path::new("state/messages.db")),
            Path::new("state"),
            "an explicit parent is unchanged"
        );
        assert_eq!(
            private_parent_of(Path::new("/")),
            Path::new("."),
            "a path with no parent at all still yields a directory to check"
        );
    }
}
