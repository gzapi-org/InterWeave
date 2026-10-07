// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The seeded store, its driven transitions and its census.
//!
//! Seeded through the store's own API with only the three states
//! `RETENTION.md` §5 lets it hold: three pending outbound (`P1`-`P3`),
//! three unread inbound (`U1`-`U3`) and three kept inbound (`K1`-`K3`,
//! each read and then kept). The transitions are what a client does:
//! `U1` read without Keep, `K1` unkept, `P1` transport-terminal
//! (`Accepted`). Each deletes its durable copy; nothing is ever written
//! directly in a state the store may not hold.

use std::path::Path;

use interweave_human_store::{
    AppMessageId, HumanStore, InboundOrigin, NewInbound, NewOutbound, OutboundDestination,
    StoreOptions, TerminalCause,
};
use interweave_transport_api::{DirectDestination, MediaType, MessageId, TransportIdentity};

/// TEST-ONLY: the frozen fixture's PeerId (fixtures/identity/), a public vector.
const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

fn id(n: u8) -> String {
    format!("{n:02x}").repeat(16)
}

fn peer() -> Result<TransportIdentity, String> {
    TransportIdentity::parse(PEER).map_err(|e| format!("peer: {e}"))
}

fn open(path: &Path) -> Result<HumanStore, String> {
    HumanStore::open(path, StoreOptions::default()).map_err(|e| format!("open: {e}"))
}

/// A value written into this module's JSON strings, quotes and backslashes
/// escaped: a store error's Debug text carries quotes (#214's re-review).
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn label(payload: &[u8]) -> String {
    String::from_utf8_lossy(payload).into_owned()
}

/// Seed a fresh store at `path`. Refuses to seed one that already holds rows,
/// so a re-run cannot double the set the census is compared against.
///
/// # Errors
/// Any store error, or a store that is not empty.
pub fn seed(path: &Path) -> Result<String, String> {
    let mut store = open(path)?;
    let held = store.pending_outbound().map_err(|e| e.to_string())?.len()
        + store.unread_inbound().map_err(|e| e.to_string())?.len()
        + store.kept_inbound().map_err(|e| e.to_string())?.len();
    if held != 0 {
        return Err(format!("not empty: {held} rows"));
    }
    let media = MediaType::parse("application/vnd.interweave-human-chat+json;v=2")
        .map_err(|e| format!("media: {e}"))?;
    for n in 1..=3u8 {
        store
            .commit_pending_outbound(&NewOutbound {
                app_message_id: AppMessageId::parse(id(n)).map_err(|e| e.to_string())?,
                transport_message_id: MessageId::parse_hex(&id(n + 0x80))
                    .map_err(|e| e.to_string())?,
                destination: OutboundDestination::Direct(DirectDestination::to_default(peer()?)),
                media_type: Some(media.clone()),
                payload: format!("P{n}").into_bytes(),
                created_at: 1_000,
            })
            .map_err(|e| format!("P{n}: {e}"))?;
    }
    for (prefix, base) in [("U", 0x10u8), ("K", 0x20u8)] {
        for n in 1..=3u8 {
            let row = store
                .commit_unread_inbound(&NewInbound {
                    app_message_id: AppMessageId::parse(id(base + n)).map_err(|e| e.to_string())?,
                    origin: InboundOrigin {
                        peer: peer()?,
                        endpoint: None,
                        channel: None,
                    },
                    media_type: None,
                    payload: format!("{prefix}{n}").into_bytes(),
                    received_at: 2_000,
                })
                .map_err(|e| format!("{prefix}{n}: {e}"))?;
            if prefix == "K" {
                let read = store
                    .mark_read(row, 3_000)
                    .map_err(|e| format!("K{n} read: {e}"))?;
                store
                    .keep(&read, 3_001)
                    .map_err(|e| format!("K{n} keep: {e}"))?;
            }
        }
    }
    census(path)
}

/// Drive the three transitions: `U1` read without Keep, `K1` unkept, `P1`
/// transport-terminal.
///
/// # Errors
/// Any store error, or a seeded row that is missing.
pub fn transitions(path: &Path) -> Result<String, String> {
    let mut store = open(path)?;
    let u1 = store
        .unread_inbound()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|r| r.payload == b"U1")
        .ok_or("U1 missing")?;
    store
        .mark_read(u1.row_id, 4_000)
        .map_err(|e| format!("U1 read: {e}"))?;
    let k1 = store
        .kept_inbound()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|r| r.payload == b"K1")
        .ok_or("K1 missing")?;
    store
        .unkeep(k1.row_id, 4_001)
        .map_err(|e| format!("K1 unkeep: {e}"))?;
    let p1 = store
        .pending_outbound()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|r| r.payload == b"P1")
        .ok_or("P1 missing")?;
    store
        .transport_terminal(p1.row_id, TerminalCause::Accepted)
        .map_err(|e| format!("P1 terminal: {e}"))?;
    drop(store);
    census(path)
}

/// What the store holds, by label, and what its backup predicate offers:
/// `{"pending":[..],"unread":[..],"kept":[..],"eligible":[..]}`.
///
/// # Errors
/// Any store error.
pub fn census(path: &Path) -> Result<String, String> {
    let store = open(path)?;
    let mut pending: Vec<String> = store
        .pending_outbound()
        .map_err(|e| e.to_string())?
        .iter()
        .map(|r| label(&r.payload))
        .collect();
    let mut unread: Vec<String> = store
        .unread_inbound()
        .map_err(|e| e.to_string())?
        .iter()
        .map(|r| label(&r.payload))
        .collect();
    let mut kept: Vec<String> = store
        .kept_inbound()
        .map_err(|e| e.to_string())?
        .iter()
        .map(|r| label(&r.payload))
        .collect();
    let mut eligible: Vec<String> = store
        .backup_eligible_content()
        .map_err(|e| e.to_string())?
        .iter()
        .map(|r| label(&r.payload))
        .collect();
    for v in [&mut pending, &mut unread, &mut kept, &mut eligible] {
        v.sort();
    }
    let list = |v: &[String]| {
        v.iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(",")
    };
    Ok(format!(
        "{{\"pending\":[{}],\"unread\":[{}],\"kept\":[{}],\"eligible\":[{}]}}",
        list(&pending),
        list(&unread),
        list(&kept),
        list(&eligible)
    ))
}

fn inbound(id: u8, label: &str, payload_len: usize) -> Result<NewInbound, String> {
    let mut payload = label.as_bytes().to_vec();
    payload.resize(payload_len.max(payload.len()), b'.');
    Ok(NewInbound {
        app_message_id: AppMessageId::parse(id_of(id)).map_err(|e| e.to_string())?,
        origin: InboundOrigin {
            peer: peer()?,
            endpoint: None,
            channel: None,
        },
        media_type: None,
        payload,
        received_at: 5_000,
    })
}

fn id_of(n: u8) -> String {
    id(n)
}

/// `RETENTION.md` §5's duplicate suppression (STATE.md `read_pairs`): `U1`,
/// read and not kept by `transitions`, delivered again must not come back as
/// unread; a message never seen before (`C1`) is the control and is
/// committed. Run after `transitions`, and again after a restart.
///
/// # Errors
/// Any store error other than the refusal being measured.
pub fn redeliver(path: &Path) -> Result<String, String> {
    let mut store = open(path)?;
    let again = match store.commit_unread_inbound(&inbound(0x11, "U1", 0)?) {
        Ok(_) => "committed".to_owned(),
        Err(e) => esc(&format!("{e:?}")),
    };
    let control = match store.commit_unread_inbound(&inbound(0x31, "C1", 0)?) {
        Ok(_) => "committed".to_owned(),
        Err(e) => esc(&format!("{e:?}")),
    };
    Ok(format!(
        "{{\"u1_again\":\"{again}\",\"control_c1\":\"{control}\"}}"
    ))
}

/// `RETENTION.md` conformance 14: when storage cannot hold unread content
/// the store degrades instead of claiming durability. A store of its own at
/// `path`, opened with a page quota (`max_pages`, a real `SQLITE_FULL`),
/// takes 8 KiB unread messages until it refuses one; then its health, a
/// further commit, and whether what it holds is still readable.
///
/// # Errors
/// Any error before the first commit is attempted.
pub fn fill(path: &Path) -> Result<String, String> {
    const PAGES: u32 = 48;
    let mut store = HumanStore::open(
        path,
        StoreOptions {
            max_pages: Some(PAGES),
        },
    )
    .map_err(|e| format!("open: {e}"))?;
    let mut committed = 0u32;
    let mut first_error = String::from("none");
    for n in 0..100u8 {
        match store.commit_unread_inbound(&inbound(0x40 + n, &format!("F{n}"), 8 * 1024)?) {
            Ok(_) => committed += 1,
            Err(e) => {
                first_error = esc(&format!("{e:?}"));
                break;
            }
        }
    }
    let health = format!("{:?}", store.health());
    let after = match store.commit_unread_inbound(&inbound(0xf0, "after", 16)?) {
        Ok(_) => "committed".to_owned(),
        Err(e) => esc(&format!("{e:?}")),
    };
    let readable = store
        .unread_inbound()
        .map_or_else(|e| esc(&format!("{e:?}")), |v| v.len().to_string());
    Ok(format!(
        "{{\"max_pages\":{PAGES},\"committed\":{committed},\"first_error\":\"{first_error}\",\"health\":\"{health}\",\"then\":\"{after}\",\"readable_unread\":\"{readable}\"}}"
    ))
}

/// TEST-ONLY markers for [`forensic`]: long and unique, so a byte search
/// cannot match them by chance.
pub const RELEASED_MARKER: &[u8] = b"SPIKE008-RELEASED-CONTENT-7f3a9c2e5b1d4f6a8c0e2b4d6f8a0c2e";
pub const RELEASED_LATER_MARKER: &[u8] =
    b"SPIKE008-RELEASED-LATER-CONTENT-4c6e8a0b2d4f6a8c0e1b3d5f7a9c";
pub const KEPT_MARKER: &[u8] = b"SPIKE008-KEPT-CONTENT-1b3d5f7a9c0e2b4d6f8a1c3e5b7d9f0a2c4e";

fn holds(path: &Path, needle: &[u8]) -> String {
    match std::fs::read(path) {
        Ok(bytes) => bytes.windows(needle.len()).any(|w| w == needle).to_string(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "no file".to_owned(),
        Err(e) => format!("unreadable: {e}"),
    }
}

/// `RETENTION.md` §5 and §8 at the file level: a message read without Keep
/// leaves the store's queries, but does its content leave the database
/// file and its write-ahead log? A store of its own at `path`: one unread
/// message carrying [`RELEASED_MARKER`], read and not kept (released), and
/// one carrying [`KEPT_MARKER`], read and kept -- the control, which the
/// search must find or it proves nothing. Measured with the store open and
/// again after it is closed.
///
/// # Errors
/// Any store error.
pub fn forensic(path: &Path) -> Result<String, String> {
    let wal = path.with_extension("sqlite-wal");
    let mut store = open(path)?;
    let released = store
        .commit_unread_inbound(&NewInbound {
            payload: RELEASED_MARKER.to_vec(),
            ..inbound(0x71, "X", 0)?
        })
        .map_err(|e| format!("released commit: {e}"))?;
    store
        .mark_read(released, 6_000)
        .map_err(|e| format!("released read: {e}"))?;
    let kept = store
        .commit_unread_inbound(&NewInbound {
            payload: KEPT_MARKER.to_vec(),
            ..inbound(0x72, "Y", 0)?
        })
        .map_err(|e| format!("kept commit: {e}"))?;
    let read = store
        .mark_read(kept, 6_001)
        .map_err(|e| format!("kept read: {e}"))?;
    store
        .keep(&read, 6_002)
        .map_err(|e| format!("kept keep: {e}"))?;
    let open_released = (holds(path, RELEASED_MARKER), holds(&wal, RELEASED_MARKER));
    let open_kept = (holds(path, KEPT_MARKER), holds(&wal, KEPT_MARKER));
    drop(store);
    let closed_released = (holds(path, RELEASED_MARKER), holds(&wal, RELEASED_MARKER));
    let closed_kept = (holds(path, KEPT_MARKER), holds(&wal, KEPT_MARKER));
    // The realistic case: a message that sat unread across a restart --
    // already checkpointed into the database file -- and is read later.
    let second = RELEASED_LATER_MARKER;
    let mut store = open(path)?;
    store
        .commit_unread_inbound(&NewInbound {
            payload: second.to_vec(),
            ..inbound(0x73, "Z", 0)?
        })
        .map_err(|e| format!("later commit: {e}"))?;
    drop(store);
    let before_read = holds(path, second);
    let mut store = open(path)?;
    let row = store
        .unread_inbound()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|r| r.payload == second)
        .ok_or("later row missing")?;
    store
        .mark_read(row.row_id, 7_000)
        .map_err(|e| format!("later read: {e}"))?;
    let later_open = (holds(path, second), holds(&wal, second));
    drop(store);
    let later_closed = (holds(path, second), holds(&wal, second));
    // Each object from its own named pair: a positional list of twelve
    // values once printed the open-store readings under the wrong labels
    // (#214's re-review), and `the_file_level_search_labels_...` pins them.
    let files = |(db, wal): &(String, String)| format!("{{\"db\":\"{db}\",\"wal\":\"{wal}\"}}");
    Ok(format!(
        "{{\"later\":{{\"before_read_db\":\"{before_read}\",\"open\":{},\"closed\":{}}},\"open\":{{\"released\":{},\"kept_control\":{}}},\"closed\":{{\"released\":{},\"kept_control\":{}}}}}",
        files(&later_open),
        files(&later_closed),
        files(&open_released),
        files(&open_kept),
        files(&closed_released),
        files(&closed_kept),
    ))
}
