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
pub const SCHEMA_VERSION: i64 = 3;

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

/// Tables whose SQLite-generated indexes are legitimate.
///
/// An autoindex is named `sqlite_autoindex_<table>_<n>` and is caught by
/// the `sqlite_` prefix; this covers any index SQLite names after the
/// table it backs.
const INTERNAL_INDEX_OWNERS: &[&str] = &[
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
    if current < 2 {
        migration_2(&tx)?;
    }
    if current < 3 {
        migration_3(&tx)?;
    }
    // The version bump rides the SAME transaction as the DDL above, which
    // is what makes a crashed migration a no-op rather than a schema the
    // store misreads on the next open.
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// v3 — inbound identity is scoped to the endpoint as well as the peer.
///
/// `ENDPOINTS.md` is explicit about why, and the store was one scope
/// level short of it:
///
/// > Including `source_endpoint` prevents a message ID collision between
/// > two endpoints on the same authenticated peer from suppressing an
/// > independent delivery.
///
/// `direct/dedup-key.schema.json` says the same thing about transport's
/// key. That is a different layer — `app_message_id` is HumanChatV2's
/// application identity, not `DirectContentFingerprintV1` — but the harm
/// is identical here, and it is the harm [`migration_2`] was written to
/// stop: one peer's `human` and `automation` endpoints reusing an id
/// collapsed into one row, and the second message silently never reached
/// durable state while the caller was told it had.
///
/// # Why a generated column and not `UNIQUE(source_peer, source_endpoint, app_message_id)`
///
/// `source_endpoint` is nullable — direct inbound always carries one, but
/// the record type permits `None` and broadcast has no source endpoint at
/// all. SQLite treats NULLs in a UNIQUE key as DISTINCT, so the direct
/// spelling silently removes dedup for every row whose endpoint is NULL:
/// two identical unendpointed messages both insert, and the keep upsert's
/// `ON CONFLICT` never fires. Widening the key that way would have
/// reintroduced, for NULL endpoints, precisely the bug being fixed.
///
/// `source_endpoint_key` collapses NULL to the empty string. That cannot
/// alias a real endpoint: `endpoints/endpoint-id.schema.json` gives the
/// grammar as `^[a-z][a-z0-9._-]{0,63}$` with `minLength: 1`, so an
/// EndpointId always has a leading lower-case letter and the empty string
/// is outside the language. It is
/// VIRTUAL: it computes on read and stores nothing, so it is not a second
/// place a body can be kept and does not widen the content surface
/// ADR-0044 bounds. `PRAGMA table_info` does not report generated columns
/// at all, so [`EXPECTED_SCHEMA`] does not list it; `PRAGMA index_info`
/// does report it BY NAME, which is what keeps [`verify_shape`]'s unique
/// key assertion load-bearing. A bare expression index would have been
/// reported with a NULL column name and silently dropped by
/// [`actual_unique_keys`], leaving the guard passing a key it no longer
/// checked.
///
/// The copy is a plain `INSERT`: this migration WIDENS the key, and a
/// wider key cannot turn two distinct rows into a collision. A failure
/// here is real corruption, not an expected duplicate, so it is not
/// suppressed with `OR IGNORE`.
fn migration_3(tx: &Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(
        "
        CREATE TABLE unread_inbound_v3 (
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
        INSERT INTO unread_inbound_v3
            (row_id, app_message_id, source_peer, source_endpoint, channel_id,
             media_type, payload, received_at)
            SELECT row_id, app_message_id, source_peer, source_endpoint, channel_id,
                   media_type, payload, received_at FROM unread_inbound;
        DROP TABLE unread_inbound;
        ALTER TABLE unread_inbound_v3 RENAME TO unread_inbound;

        CREATE TABLE kept_inbound_v3 (
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
        INSERT INTO kept_inbound_v3
            (row_id, app_message_id, source_peer, source_endpoint, channel_id,
             media_type, payload, received_at, read_at, kept_at)
            SELECT row_id, app_message_id, source_peer, source_endpoint, channel_id,
                   media_type, payload, received_at, read_at, kept_at FROM kept_inbound;
        DROP TABLE kept_inbound;
        ALTER TABLE kept_inbound_v3 RENAME TO kept_inbound;
        ",
    )
    .map_err(|e| StoreError::Migration(e.to_string()))
}

/// v2 — inbound identity is scoped to the peer that asserted it.
///
/// `app_message_id` is HumanChatV2's APPLICATION reply/retention
/// identity. It is chosen by the sender, so on the inbound side it is
/// remote-controlled data and not a dedup identity this store may trust
/// globally — that is transport's `DirectContentFingerprintV1`, at a
/// different layer.
///
/// A `UNIQUE` on it alone meant a peer reusing one of its own prior ids,
/// or two peers happening to pick the same one, collided in the store.
/// The keep upsert would then update the OLDER row's timestamps and
/// leave its body in place, so the newer message quietly never reached
/// durable kept state and the caller was told it had.
///
/// SQLite cannot drop a column constraint, so the tables are rebuilt.
/// They hold only what this build itself wrote, and the rebuild is
/// inside the migration transaction with the version bump.
fn migration_2(tx: &Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(
        "
        CREATE TABLE unread_inbound_v2 (
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
        INSERT OR IGNORE INTO unread_inbound_v2
            (row_id, app_message_id, source_peer, source_endpoint, channel_id,
             media_type, payload, received_at)
            SELECT row_id, app_message_id, source_peer, source_endpoint, channel_id,
                   media_type, payload, received_at FROM unread_inbound;
        DROP TABLE unread_inbound;
        ALTER TABLE unread_inbound_v2 RENAME TO unread_inbound;

        CREATE TABLE kept_inbound_v2 (
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
        INSERT OR IGNORE INTO kept_inbound_v2
            (row_id, app_message_id, source_peer, source_endpoint, channel_id,
             media_type, payload, received_at, read_at, kept_at)
            SELECT row_id, app_message_id, source_peer, source_endpoint, channel_id,
                   media_type, payload, received_at, read_at, kept_at FROM kept_inbound;
        DROP TABLE kept_inbound;
        ALTER TABLE kept_inbound_v2 RENAME TO kept_inbound;
        ",
    )
    .map_err(|e| StoreError::Migration(e.to_string()))
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

/// One column, exactly as `PRAGMA table_info` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Column {
    name: String,
    /// The DECLARED type. SQLite does not enforce it, which is why it
    /// is checked: a rebuilt table whose `payload` became `TEXT` would
    /// still work and would still be a different schema.
    decl_type: String,
    not_null: bool,
    /// Part of the primary key.
    primary_key: bool,
}

/// One table's complete expected shape.
struct TableShape {
    name: &'static str,
    /// Ordered, because `table_info` is ordered and a reordering is a
    /// different table. `(name, declared type, NOT NULL, primary key)`.
    columns: &'static [(&'static str, &'static str, bool, bool)],
    /// Every UNIQUE key, as its ordered column list. Includes the
    /// implicit index behind a `TEXT PRIMARY KEY`.
    unique_keys: &'static [&'static [&'static str]],
    /// Whether the rowid primary key is AUTOINCREMENT.
    ///
    /// Not visible through any pragma, and load-bearing: a bare
    /// `INTEGER PRIMARY KEY` reuses the highest deleted row's id, which
    /// in a store that deletes constantly hands a caller someone else's
    /// body. See [`migration_1`].
    autoincrement: bool,
}

const fn col(
    name: &'static str,
    decl_type: &'static str,
    not_null: bool,
) -> (&'static str, &'static str, bool, bool) {
    (name, decl_type, not_null, false)
}

/// The rowid primary key.
///
/// `not_null` is false because SQLite does not mark a PRIMARY KEY
/// column NOT NULL unless it is declared so. Asserting the truth rather
/// than the tidy answer keeps this a description of the schema.
const fn pk(
    name: &'static str,
    decl_type: &'static str,
) -> (&'static str, &'static str, bool, bool) {
    (name, decl_type, false, true)
}

/// The complete schema this build understands, column by column.
///
/// # Why names were not enough
///
/// [`verify_shape`] used to read `SELECT type, name FROM sqlite_master`
/// and allowlist the names. That refuses a `chat_archive` TABLE and
/// accepts
///
/// ```sql
/// ALTER TABLE unread_inbound ADD COLUMN archive_payload BLOB;
/// ```
///
/// which is the same retention violation inside a permitted name: a
/// second durable content surface, in the table whose whole contract is
/// that its body disappears when the message is read. Under ADR-0044 an
/// unknown column is not forward compatibility, it is a place a body
/// can be kept, so it is refused.
const EXPECTED_SCHEMA: &[TableShape] = &[
    TableShape {
        name: "pending_outbound",
        columns: &[
            pk("row_id", "INTEGER"),
            col("app_message_id", "TEXT", true),
            col("destination_peer", "TEXT", true),
            col("destination_endpoint", "TEXT", false),
            col("channel_id", "TEXT", false),
            col("media_type", "TEXT", false),
            col("payload", "BLOB", true),
            col("created_at", "INTEGER", true),
            col("last_attempt_at", "INTEGER", false),
            col("attempts", "INTEGER", true),
        ],
        unique_keys: &[&["app_message_id"]],
        autoincrement: true,
    },
    TableShape {
        name: "unread_inbound",
        columns: &[
            pk("row_id", "INTEGER"),
            col("app_message_id", "TEXT", true),
            col("source_peer", "TEXT", true),
            col("source_endpoint", "TEXT", false),
            col("channel_id", "TEXT", false),
            col("media_type", "TEXT", false),
            col("payload", "BLOB", true),
            col("received_at", "INTEGER", true),
        ],
        // SCOPED TO THE PEER *AND THE ENDPOINT*. A bare UNIQUE on the id
        // alone let one peer collide with another's message
        // (`migration_2`); scoping only to the peer let one peer's two
        // endpoints collide with each other (`migration_3`).
        unique_keys: &[&["source_peer", "source_endpoint_key", "app_message_id"]],
        autoincrement: true,
    },
    TableShape {
        name: "kept_inbound",
        columns: &[
            pk("row_id", "INTEGER"),
            col("app_message_id", "TEXT", true),
            col("source_peer", "TEXT", true),
            col("source_endpoint", "TEXT", false),
            col("channel_id", "TEXT", false),
            col("media_type", "TEXT", false),
            col("payload", "BLOB", true),
            col("received_at", "INTEGER", true),
            col("read_at", "INTEGER", true),
            col("kept_at", "INTEGER", true),
        ],
        unique_keys: &[&["source_peer", "source_endpoint_key", "app_message_id"]],
        autoincrement: true,
    },
    TableShape {
        name: "settings",
        columns: &[pk("key", "TEXT"), col("value", "TEXT", true)],
        unique_keys: &[&["key"]],
        autoincrement: false,
    },
];

/// Read one table's columns as `PRAGMA table_info` reports them.
fn actual_columns(conn: &Connection, table: &str) -> Result<Vec<Column>, StoreError> {
    // `PRAGMA table_info(?)` does not accept a bound parameter, and the
    // name here is a compile-time constant from EXPECTED_SCHEMA rather
    // than anything a caller supplied.
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| {
        Ok(Column {
            name: r.get::<_, String>(1)?,
            decl_type: r.get::<_, String>(2)?.to_ascii_uppercase(),
            not_null: r.get::<_, i64>(3)? != 0,
            primary_key: r.get::<_, i64>(5)? != 0,
        })
    })?;
    rows.collect::<Result<_, _>>().map_err(StoreError::from)
}

/// Read one table's UNIQUE keys as ordered column lists.
fn actual_unique_keys(conn: &Connection, table: &str) -> Result<Vec<Vec<String>>, StoreError> {
    let mut list = conn.prepare(&format!("PRAGMA index_list({table})"))?;
    let indexes: Vec<(String, i64)> = list
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))?
        .collect::<Result<_, _>>()?;

    let mut keys = Vec::new();
    for (name, unique) in indexes {
        if unique == 0 {
            continue;
        }
        let mut info = conn.prepare(&format!("PRAGMA index_info(\"{name}\")"))?;
        let mut cols: Vec<(i64, Option<String>)> = info
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(2)?))
            })?
            .collect::<Result<_, _>>()?;
        cols.sort_by_key(|(seq, _)| *seq);
        keys.push(cols.into_iter().filter_map(|(_, c)| c).collect());
    }
    keys.sort();
    Ok(keys)
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
    let mut stmt = conn.prepare("SELECT type, name FROM sqlite_master")?;
    let objects: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<_, _>>()?;

    let tables: Vec<&String> = objects
        .iter()
        .filter(|(kind, _)| kind == "table")
        .map(|(_, name)| name)
        .collect();

    for required in REQUIRED_TABLES {
        if !tables.iter().any(|n| *n == required) {
            return Err(StoreError::Migration(format!(
                "table `{required}` is missing"
            )));
        }
    }

    // EVERY COLUMN, not merely every table name. Refusing a
    // `chat_archive` table while accepting
    // `ALTER TABLE unread_inbound ADD COLUMN archive_payload BLOB`
    // catches the clumsy version of the retention violation and misses
    // the one that fits inside a permitted name.
    for shape in EXPECTED_SCHEMA {
        let actual = actual_columns(conn, shape.name)?;
        let matches = actual.len() == shape.columns.len()
            && actual.iter().zip(shape.columns).all(|(a, e)| {
                a.name == e.0 && a.decl_type == e.1 && a.not_null == e.2 && a.primary_key == e.3
            });
        if !matches {
            return Err(StoreError::Migration(format!(
                "table `{}` does not have the shape this build wrote: expected {:?}, found {:?}",
                shape.name, shape.columns, actual
            )));
        }

        let mut expected_keys: Vec<Vec<String>> = shape
            .unique_keys
            .iter()
            .map(|k| k.iter().map(|c| (*c).to_owned()).collect())
            .collect();
        expected_keys.sort();
        let actual_keys = actual_unique_keys(conn, shape.name)?;
        if actual_keys != expected_keys {
            return Err(StoreError::Migration(format!(
                "table `{}` has unique keys {actual_keys:?}; this build wrote {expected_keys:?}",
                shape.name
            )));
        }

        // AUTOINCREMENT is invisible to every pragma and is what stops
        // a deleted row's id being handed to the next insert.
        let sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [shape.name],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let has = sql
            .as_deref()
            .is_some_and(|q| q.to_ascii_uppercase().contains("AUTOINCREMENT"));
        if has != shape.autoincrement {
            return Err(StoreError::Migration(format!(
                "table `{}` autoincrement is {has}; this build wrote {}",
                shape.name, shape.autoincrement
            )));
        }
    }

    // AN ALLOWLIST, not a list of names someone thought of.
    //
    // The named-enemies list still runs, because a `messages` table
    // deserves the message that says why it is forbidden. But it can only
    // ever catch what it names, and the doc comment above claims
    // REQUIRED_TABLES is "the whole content surface" — a table called
    // `chat_archive` passed while being exactly the archive ADR-0044
    // forbids. Anything not on the list is refused now, so an addition
    // has to be a decision made here rather than one nobody noticed.
    //
    // Views and triggers are refused for the same reason and are worse:
    // a view is a content surface with no storage of its own, and a
    // trigger can copy a row somewhere on its way out.
    for (kind, name) in &objects {
        let lowered = name.to_ascii_lowercase();
        if FORBIDDEN_TABLES.contains(&lowered.as_str()) {
            return Err(StoreError::Migration(format!(
                "table `{name}` is a general message archive; ADR-0044 allows only \
                 pending_outbound, unread_inbound, and kept_inbound to hold content"
            )));
        }
        // SQLite's own bookkeeping, which the store does not create and
        // cannot remove: `sqlite_sequence` exists because the content
        // tables are AUTOINCREMENT, and autoindexes back the UNIQUE
        // constraints those tables declare.
        if lowered.starts_with("sqlite_") {
            continue;
        }
        match kind.as_str() {
            "table" if REQUIRED_TABLES.contains(&name.as_str()) => {}
            "index" if INTERNAL_INDEX_OWNERS.iter().any(|t| lowered.contains(t)) => {}
            _ => {
                return Err(StoreError::Migration(format!(
                    "{kind} `{name}` is not part of this store's schema; ADR-0044 makes \
                     pending_outbound, unread_inbound, kept_inbound and settings the whole \
                     content surface, and anything else must be an explicit decision"
                )));
            }
        }
    }
    Ok(())
}
