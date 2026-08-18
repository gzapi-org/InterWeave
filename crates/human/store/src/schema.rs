// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Versioned, transactional schema migration.
//!
//! Every migration runs inside one transaction and bumps
//! `PRAGMA user_version` in that same transaction, so a migration that
//! fails partway leaves the database at the version it started from. A
//! half-applied schema would be worse than an old one: the store would
//! believe it had columns it does not have.
//!
//! # Migration failure never touches identity
//!
//! Nothing here can regenerate a PeerId, rewrite trust policy, or delete
//! a key. The human store is application state and is deletable by
//! design ([`crate`] docs); a failed migration surfaces as
//! [`StoreError::Migration`] and the caller enters recovery/export mode.
//!
//! # There is no history table
//!
//! [`REQUIRED_TABLES`] is the whole content surface, and
//! [`verify_shape`] is called on every open. A future migration that
//! added a general `messages` table would fail that check on the next
//! open rather than quietly becoming the archive ADR-0044 forbids.

use rusqlite::{Connection, Transaction};

use crate::StoreError;

/// The schema version this build writes and expects.
pub const SCHEMA_VERSION: i64 = 1;

/// Every table the store is allowed to contain.
///
/// Checked on open. The three content tables are the retention states of
/// ADR-0044; the rest is content-free metadata that cannot reconstruct a
/// deleted body.
pub const REQUIRED_TABLES: &[&str] = &[
    "pending_outbound",
    "unread_inbound",
    "kept_inbound",
    "settings",
];

/// Table names that would make this a conversation archive.
///
/// Named explicitly rather than inferred, because the failure this
/// guards against is a plausible-sounding addition — "just an index",
/// "just for the UI" — and a rule that names its enemies is one a
/// reviewer can check.
const FORBIDDEN_TABLES: &[&str] = &[
    "messages",
    "message_history",
    "conversation_history",
    "history",
    "sent_messages",
    "read_inbound",
];

/// Apply every migration needed to bring `conn` to [`SCHEMA_VERSION`].
///
/// # Errors
/// Returns [`StoreError::Migration`] if a migration fails, or if the
/// database was written by a NEWER build — downgrading by running old
/// migrations over a newer schema is how data gets destroyed, so it
/// refuses instead.
pub fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if current > SCHEMA_VERSION {
        return Err(StoreError::Migration(format!(
            "database is at schema version {current}, newer than this build's {SCHEMA_VERSION}; \
             refusing to downgrade"
        )));
    }
    if current == SCHEMA_VERSION {
        return Ok(());
    }

    let tx = conn.transaction()?;
    if current < 1 {
        migration_1(&tx)?;
    }
    // The version bump rides the SAME transaction as the DDL above, which
    // is what makes a crashed migration a no-op rather than a schema the
    // store misreads on the next open.
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// v1 — the three retention tables plus content-free settings.
///
/// # Why every content table is AUTOINCREMENT
///
/// A bare `INTEGER PRIMARY KEY` reuses the id of the highest row once it
/// is deleted, and deletion is what this store does constantly. Combined
/// with an idempotent `transport_terminal` — a retry reaching terminal
/// twice must not error — a late duplicate event for a finished message
/// would delete whatever message inherited its id, silently losing
/// something the user had just composed. On the inbound side a reused id
/// would hand a caller someone else's body and delete the durable copy
/// of a message nobody had seen.
///
/// AUTOINCREMENT costs one internal `sqlite_sequence` row and makes the
/// hazard unreachable rather than defended against.
fn migration_1(tx: &Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(
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

        CREATE TABLE settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        ",
    )?;
    Ok(())
}

/// Prove the opened database is the store this build understands.
///
/// Two questions, and the second is the one that matters: every required
/// table is present, and NO forbidden table is. A general history table
/// is not a compatibility problem — it is a retention violation, and it
/// is caught here because a store that contains one must not be opened
/// and used as though ADR-0044 held.
///
/// # Errors
/// Returns [`StoreError::Migration`] naming the missing or forbidden
/// table.
pub fn verify_shape(conn: &Connection) -> Result<(), StoreError> {
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;

    for required in REQUIRED_TABLES {
        if !names.iter().any(|n| n == required) {
            return Err(StoreError::Migration(format!(
                "table `{required}` is missing"
            )));
        }
    }
    for name in &names {
        let lowered = name.to_ascii_lowercase();
        if FORBIDDEN_TABLES.contains(&lowered.as_str()) {
            return Err(StoreError::Migration(format!(
                "table `{name}` is a general message archive; ADR-0044 allows only \
                 pending_outbound, unread_inbound, and kept_inbound to hold content"
            )));
        }
    }
    Ok(())
}
