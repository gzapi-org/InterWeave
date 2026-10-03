// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The store's rows, decoded for the model: what a root shows at start,
//! and the unread rows again when the facade held content it could not
//! hand over.

use interweave_human_chat_protocol::{HumanChatV2, decode_envelope_bytes, parse_media_type};
use interweave_human_client_api::{Destination, Origin};
use interweave_human_store::{
    Cursor, HumanStore, InboundOrigin, OutboundDestination, Page, PageLimits, PendingOutbound,
    StoreError, StoredInbound,
};
use interweave_human_ui_model::{ListedInbound, ListedOutbound};
use interweave_transport_api::MediaType;

use crate::protocol::Listing;

/// One page of a listing: records, and bytes, at most. Both ceilings are
/// the store's own rule -- a record count says nothing about 48 KiB
/// payloads -- and the walk continues page by page.
const PAGE_RECORDS: usize = 64;
const PAGE_BYTES: usize = 1024 * 1024;

fn limits() -> PageLimits {
    // Both non-zero: `new` refuses only a zero ceiling.
    PageLimits::new(PAGE_RECORDS, PAGE_BYTES).unwrap_or_else(|_| unreachable!())
}

/// Every pending, unread and kept row, decoded.
///
/// # Errors
/// The store's, when a page cannot be read.
pub(crate) fn list_all(store: &HumanStore) -> Result<Listing, StoreError> {
    let mut listing = Listing::default();
    for row in walk(|after| store.pending_outbound_page(after, limits()))? {
        match outbound(&row) {
            Some(listed) => listing.pending.push(listed),
            None => listing.undecodable += 1,
        }
    }
    let (unread, undecodable) = list_unread(store)?;
    listing.unread = unread;
    listing.undecodable += undecodable;
    for row in walk(|after| store.kept_inbound_page(after, limits()))? {
        match inbound(&row) {
            Some(listed) => listing.kept.push(listed),
            None => listing.undecodable += 1,
        }
    }
    Ok(listing)
}

/// Every unread row, decoded, with how many could not be.
///
/// # Errors
/// The store's, when a page cannot be read.
pub(crate) fn list_unread(store: &HumanStore) -> Result<(Vec<ListedInbound>, usize), StoreError> {
    let mut listed = Vec::new();
    let mut undecodable = 0;
    for row in walk(|after| store.unread_inbound_page(after, limits()))? {
        match inbound(&row) {
            Some(item) => listed.push(item),
            None => undecodable += 1,
        }
    }
    Ok((listed, undecodable))
}

fn walk<T>(
    mut page: impl FnMut(Option<Cursor>) -> Result<Page<T>, StoreError>,
) -> Result<Vec<T>, StoreError> {
    let mut rows = Vec::new();
    let mut after = None;
    loop {
        let Page { items, next } = page(after)?;
        rows.extend(items);
        match next {
            Some(cursor) => after = Some(cursor),
            None => return Ok(rows),
        }
    }
}

fn outbound(row: &PendingOutbound) -> Option<ListedOutbound> {
    Some(ListedOutbound {
        row: row.row_id,
        destination: match &row.destination {
            OutboundDestination::Direct(direct) => Destination::Direct {
                peer: direct.peer.clone(),
                endpoint: direct.endpoint.clone(),
            },
            OutboundDestination::Broadcast(channel) => Destination::Broadcast(channel.clone()),
        },
        envelope: decode(row.media_type.as_ref(), &row.payload)?,
        created_at: row.created_at,
    })
}

fn inbound(row: &StoredInbound) -> Option<ListedInbound> {
    let InboundOrigin {
        peer,
        endpoint,
        channel,
    } = &row.origin;
    // A direct row records the endpoint the sender asserted, a broadcast
    // row its channel; a row with neither, or both, is not one the
    // facade wrote.
    let origin = match (endpoint, channel) {
        (Some(endpoint), None) => Origin::Direct {
            peer: peer.clone(),
            endpoint: endpoint.clone(),
        },
        (None, Some(channel)) => Origin::Channel {
            channel: channel.clone(),
            publisher: peer.clone(),
        },
        _ => return None,
    };
    Some(ListedInbound {
        row: row.row_id,
        origin,
        envelope: decode(row.media_type.as_ref(), &row.payload)?,
        received_at: row.received_at,
    })
}

/// The envelope a stored payload carries: the same steps the facade
/// takes on receipt (`HUMAN-CHAT.md` §Consumers), so a row that was
/// shown when it arrived is shown again after a restart.
fn decode(media_type: Option<&MediaType>, payload: &[u8]) -> Option<HumanChatV2> {
    let info = parse_media_type(media_type?.as_str()).ok()?;
    let text = decode_envelope_bytes(payload, info.encoding).ok()?;
    HumanChatV2::parse(&text).ok()
}
