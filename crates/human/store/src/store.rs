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
use interweave_transport_api::payload::MAX_PAYLOAD_BYTES;
use interweave_transport_api::{
    ChannelId, DirectDestination, EndpointId, MediaType, TransportIdentity,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;
use crate::records::{
    AppMessageId, BackupCursor, BackupTable, Cursor, InboundOrigin, NewInbound, NewOutbound,
    OutboundDestination, Page, PageLimits, PendingOutbound, ReadEphemeral, RowId, StoredInbound,
};
use crate::schema::{migrate, verify_shape};

/// How the store is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoreOptions {
    /// A hard page ceiling for the database file, if the application
    /// imposes a quota.
    ///
    /// `None` means the filesystem is the only limit. When set, exceeding
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
        // Owner-only because this directory holds message content. The
        // mode is applied at creation rather than after, so there is no
        // window in which it is world-traversable. This duplicates three
        // lines of `interweave-profile-config` on purpose: the store must
        // not depend on configuration to protect its own files.
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            create_private_dir(parent)?;
            // CREATED owner-only says nothing about one that was already
            // there. A pre-existing state directory — restored, copied,
            // made by an older build, or simply made by hand — carries
            // whatever mode it has, and the store's documentation
            // promised a protection it had not checked.
            //
            // Refused rather than tightened, for the reason the identity
            // key is: content that has been broadly readable should be
            // treated as exposed, and quietly narrowing the mode would
            // hide that it ever was.
            require_owner_only(parent, "the state directory")?;
        }
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
        }

        let conn = Connection::open(path)?;
        // The database and its WAL/SHM companions hold the same message
        // content as the directory, and SQLite creates the companions
        // itself with the process umask. Checked after the connection so
        // they exist to be checked.
        for (suffix, what) in [
            ("", "the database"),
            ("-wal", "the write-ahead log"),
            ("-shm", "the shared-memory index"),
        ] {
            let mut companion = path.as_os_str().to_owned();
            companion.push(suffix);
            let companion = std::path::PathBuf::from(companion);
            if companion.exists() {
                require_owner_only(&companion, what)?;
            }
        }
        Self::from_connection(conn, options)
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
        if let Some(max_pages) = options.max_pages {
            conn.pragma_update(None, "max_page_count", max_pages)?;
        }

        migrate(&mut conn)?;
        verify_shape(&conn)?;

        Ok(Self {
            conn,
            health: StorageHealth::Healthy,
        })
    }

    /// Whether new unread content can still be committed durably.
    #[must_use]
    pub const fn health(&self) -> StorageHealth {
        self.health
    }

    /// Re-test the storage medium and clear degradation if it recovered.
    ///
    /// Writes and rolls back a real row rather than reading a pragma: a
    /// full or read-only database answers `SELECT` perfectly well, so
    /// only an attempted write is evidence.
    ///
    /// # Errors
    /// Returns the underlying [`StoreError`] if the probe fails, having
    /// first recorded the degradation.
    pub fn recheck_health(&mut self) -> Result<StorageHealth, StoreError> {
        let probe = (|| -> Result<(), rusqlite::Error> {
            let tx = self.conn.transaction()?;
            tx.execute(
                "INSERT INTO settings (key, value) VALUES ('__health_probe', '1')
                 ON CONFLICT(key) DO UPDATE SET value = '1'",
                [],
            )?;
            tx.rollback()
        })();

        match probe {
            Ok(()) => {
                self.health = StorageHealth::Healthy;
                Ok(self.health)
            }
            Err(e) => Err(self.note_failure(e)),
        }
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
    /// [`StoreError::PayloadTooLarge`] above the transport ceiling, or a
    /// storage error.
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
                 (app_message_id, destination_peer, destination_endpoint, channel_id,
                  media_type, payload, created_at, last_attempt_at, attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, 0)",
            params![
                new.app_message_id.as_str(),
                // A broadcast row has no peer; the column is NOT NULL, so
                // the channel row stores the empty string and the channel
                // column is what identifies it. Reading discriminates on
                // channel_id, never on this.
                peer.unwrap_or_default(),
                endpoint,
                channel,
                new.media_type.as_ref().map(MediaType::as_str),
                new.payload,
                i64::try_from(new.created_at).unwrap_or(i64::MAX),
            ],
        );

        match result {
            Ok(_) => Ok(RowId::new(self.conn.last_insert_rowid())),
            Err(e) => Err(self.note_failure(e)),
        }
    }

    /// Record that a send attempt was made.
    ///
    /// Metadata only. The message stays pending and stays durable —
    /// a failed attempt is exactly the case the durable copy exists for.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn record_attempt(&mut self, row_id: RowId, at_ms: u64) -> Result<(), StoreError> {
        let result = self.conn.execute(
            "UPDATE pending_outbound
                SET last_attempt_at = ?2, attempts = attempts + 1
              WHERE row_id = ?1",
            params![row_id.get(), i64::try_from(at_ms).unwrap_or(i64::MAX)],
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
                    Ok(_) => Ok(()),
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

    /// One page of pending outbound, resuming after `after`.
    ///
    /// # Errors
    /// Returns a storage error, or [`StoreError::Corrupt`] if a stored
    /// row no longer parses.
    pub fn pending_outbound_page(
        &self,
        after: Option<Cursor>,
        limits: PageLimits,
    ) -> Result<Page<PendingOutbound>, StoreError> {
        let (sort_key, row_id) = cursor_bounds(after);
        // One past the page, so a full page can tell "exactly this many"
        // from "more to come" without a second query.
        let fetch = i64::try_from(limits.max_records.saturating_add(1)).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT row_id, app_message_id, destination_peer, destination_endpoint, channel_id,
                    media_type, payload, created_at, last_attempt_at, attempts
               FROM pending_outbound
              WHERE (created_at, row_id) > (?1, ?2)
              ORDER BY created_at, row_id
              LIMIT ?3",
        )?;
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
                r.get::<_, i64>(9)?,
            ))
        })?;

        let mut out = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        for row in rows {
            let (id, amid, peer, endpoint, channel, media_type, payload, created, last, attempts) =
                row?;
            // The first row of a page always goes in, even alone over
            // budget: stalling the enumeration on one large message is
            // worse than one page being one message too big.
            if !out.is_empty()
                && (out.len() >= limits.max_records
                    || bytes.saturating_add(payload.len()) > limits.max_bytes)
            {
                more = true;
                break;
            }
            bytes = bytes.saturating_add(payload.len());
            let destination = match channel {
                Some(c) => OutboundDestination::Broadcast(
                    ChannelId::parse(c).map_err(|e| StoreError::Corrupt(e.to_string()))?,
                ),
                None => {
                    let peer = TransportIdentity::parse(peer)
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?;
                    let endpoint = endpoint
                        .map(EndpointId::parse)
                        .transpose()
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?;
                    OutboundDestination::Direct(DirectDestination { peer, endpoint })
                }
            };
            out.push(PendingOutbound {
                row_id: RowId::new(id),
                app_message_id: AppMessageId::parse(amid)?,
                destination,
                media_type: parse_media_type(media_type)?,
                payload,
                created_at: u64::try_from(created).unwrap_or(0),
                last_attempt_at: last.map(|v| u64::try_from(v).unwrap_or(0)),
                attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
            });
        }
        Ok(Page {
            next: more.then(|| Cursor {
                sort_key: out.last().map_or(0, |r| r.created_at),
                row_id: out.last().map_or(RowId::new(0), |r| r.row_id),
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
    /// than keep accepting a stream it cannot retain.
    pub fn commit_unread_inbound(&mut self, new: &NewInbound) -> Result<RowId, StoreError> {
        self.reject_if_degraded()?;
        check_payload(&new.payload)?;

        let result = self.conn.execute(
            "INSERT INTO unread_inbound
                 (app_message_id, source_peer, source_endpoint, channel_id,
                  media_type, payload, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                new.app_message_id.as_str(),
                new.origin.peer.as_str(),
                new.origin.endpoint.as_ref().map(|e| e.as_str()),
                new.origin.channel.as_ref().map(|c| c.as_str()),
                new.media_type.as_ref().map(MediaType::as_str),
                new.payload,
                i64::try_from(new.received_at).unwrap_or(i64::MAX),
            ],
        );

        match result {
            Ok(_) => Ok(RowId::new(self.conn.last_insert_rowid())),
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
    /// Returns [`StoreError::NoSuchRow`] if the row is not unread, or a
    /// storage error.
    pub fn mark_read(&mut self, row_id: RowId, at_ms: u64) -> Result<ReadEphemeral, StoreError> {
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
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<String>>(4)?,
                            r.get::<_, Vec<u8>>(5)?,
                            r.get::<_, i64>(6)?,
                        ))
                    },
                )
                .optional()?;

            let Some(row) = row else {
                tx.rollback()?;
                return Ok(None);
            };

            let held = ReadEphemeral {
                app_message_id: AppMessageId::parse(row.0)?,
                origin: InboundOrigin {
                    peer: TransportIdentity::parse(row.1)
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                    endpoint: row
                        .2
                        .map(EndpointId::parse)
                        .transpose()
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                    channel: row
                        .3
                        .map(ChannelId::parse)
                        .transpose()
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                },
                media_type: parse_media_type(row.4)?,
                payload: row.5,
                received_at: u64::try_from(row.6).unwrap_or(0),
                read_at: at_ms,
            };

            if durability == Durability::Remove {
                tx.execute(
                    "DELETE FROM unread_inbound WHERE row_id = ?1",
                    params![row_id.get()],
                )?;
            }
            tx.commit()?;
            Ok(Some(held))
        })();

        match result {
            Ok(Some(held)) => Ok(held),
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
    /// the state machine refuses, or a storage error.
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
        // someone else's row id.
        // RETURNING rather than last_insert_rowid(): that counter is not
        // updated when an upsert takes the UPDATE path, so it would hand
        // back whichever row was inserted most recently — a different
        // message's id, if anything was committed in between.
        let result = self.conn.query_row(
            "INSERT INTO kept_inbound
                 (app_message_id, source_peer, source_endpoint, channel_id,
                  media_type, payload, received_at, read_at, kept_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(source_peer, app_message_id) DO UPDATE SET
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
                held.origin.endpoint.as_ref().map(|e| e.as_str()),
                held.origin.channel.as_ref().map(|c| c.as_str()),
                held.media_type.as_ref().map(MediaType::as_str),
                held.payload,
                i64::try_from(held.received_at).unwrap_or(i64::MAX),
                i64::try_from(held.read_at).unwrap_or(i64::MAX),
                i64::try_from(at_ms).unwrap_or(i64::MAX),
            ],
            |r| r.get::<_, i64>(0),
        );

        match result {
            Ok(row_id) => Ok(RowId::new(row_id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(StoreError::IdentityConflict {
                app_message_id: held.app_message_id.as_str().to_owned(),
                source_peer: held.origin.peer.as_str().to_owned(),
            }),
            Err(e) => Err(self.note_failure(e)),
        }
    }

    /// The receiver removes `Keep`.
    ///
    /// Deletion is immediate — now, not at a later cleanup pass.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn unkeep(&mut self, row_id: RowId) -> Result<(), StoreError> {
        let mut message = InboundMessage::committed_unread();
        message.mark_read();
        let _ = message.keep();
        let durability = message.unkeep();

        match durability {
            Durability::Durable => Ok(()),
            Durability::Remove => {
                let result = self.conn.execute(
                    "DELETE FROM kept_inbound WHERE row_id = ?1",
                    params![row_id.get()],
                );
                match result {
                    Ok(_) => Ok(()),
                    Err(e) => Err(self.note_failure(e)),
                }
            }
        }
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
    /// Returns a storage error, or [`StoreError::Corrupt`] for an
    /// unparseable stored row.
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
    /// Returns a storage error, or [`StoreError::Corrupt`] for an
    /// unparseable stored row.
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

        let (sort_key, row_id) = cursor_bounds(after);
        let fetch = i64::try_from(limits.max_records.saturating_add(1)).unwrap_or(i64::MAX);
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
                && (out.len() >= limits.max_records
                    || bytes.saturating_add(payload.len()) > limits.max_bytes)
            {
                more = true;
                break;
            }
            bytes = bytes.saturating_add(payload.len());
            out.push(StoredInbound {
                row_id: RowId::new(id),
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
                received_at: u64::try_from(received).unwrap_or(0),
                read_at: read.map(|v| u64::try_from(v).unwrap_or(0)),
                kept_at: keptat.map(|v| u64::try_from(v).unwrap_or(0)),
            });
        }
        Ok(Page {
            next: more.then(|| Cursor {
                sort_key: out.last().map_or(0, |r| r.received_at),
                row_id: out.last().map_or(RowId::new(0), |r| r.row_id),
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
    /// Returns a storage error, or [`StoreError::Corrupt`] for an
    /// unparseable stored row.
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

/// Create `dir` and its parents, readable only by the owner.
fn create_private_dir(dir: &std::path::Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(StoreError::Io)
    }
    #[cfg(not(unix))]
    {
        // Refusing beats creating a directory of message content this
        // build cannot protect.
        let _ = dir;
        Err(StoreError::UnsupportedPlatform)
    }
}

/// Refuse anything holding message content that others can reach.
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

/// The `(sort_key, row_id)` a cursor resumes after.
///
/// `None` starts before every row. `-1` rather than `0` because a
/// timestamp of zero is legal and `> (0, 0)` would skip it.
fn cursor_bounds(after: Option<Cursor>) -> (i64, i64) {
    after.map_or((-1, -1), |c| {
        (
            i64::try_from(c.sort_key).unwrap_or(i64::MAX),
            c.row_id.get(),
        )
    })
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
