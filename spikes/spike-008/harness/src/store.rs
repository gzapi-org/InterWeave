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
