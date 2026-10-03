// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The facade side: owns the transport facade (and through it the store),
//! turns the model side's [`Command`]s into facade and store calls, and
//! reports what happened as [`Update`]s.
//!
//! It is driven, never self-driving: the root calls [`FacadeSide::turn`]
//! often enough that the session's events never wait long -- on the
//! desktop, from its own runtime, since a session whose events go
//! undrained stops answering keepalive pings and loses its lease.

use std::collections::{HashMap, VecDeque};

use interweave_human_chat_protocol::{HumanChatV2, MessageKind};
use interweave_human_client_api::{ClientEvent, Destination, Diagnostics};
use interweave_human_store::{ReadEphemeral, RowId, StoreError};
use interweave_human_transport_client::TransportClient;
use interweave_human_ui_model::{ConversationKey, Table};
use interweave_local_client_api::{AdminBinding, DataSessionBinding};
use interweave_transport_api::EndpointId;

use crate::listing;
use crate::protocol::{Command, Listing, Update};

/// How many read-and-unkept copies the facade side holds for a later
/// Keep, at most. Each is a message's content, up to the 48 KiB payload
/// limit, so the bound is on memory as much as on count; past it the
/// oldest copy is dropped and a Keep of that message fails (reported as
/// [`Update::Failed`], nothing changed).
pub const READ_COPY_CAP: usize = 256;

/// How many received messages one turn hands over, at most.
const DRAIN_PER_TURN: usize = 64;

/// The facade side of the root.
pub struct FacadeSide<B: DataSessionBinding, A: AdminBinding> {
    client: TransportClient<B, A>,
    endpoint: Option<EndpointId>,
    wall: Box<dyn Fn() -> u64 + Send + Sync>,
    copies: ReadCopies<ReadEphemeral>,
    reported: Option<Diagnostics>,
}

impl<B: DataSessionBinding, A: AdminBinding> FacadeSide<B, A> {
    /// Wrap a facade. `endpoint` is the local endpoint a sent message
    /// names as its `from_endpoint` -- the one the facade leases -- and
    /// `wall` the wall clock in Unix milliseconds.
    pub fn new(
        client: TransportClient<B, A>,
        endpoint: Option<EndpointId>,
        wall: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            client,
            endpoint,
            wall: Box::new(wall),
            copies: ReadCopies::default(),
            reported: None,
        }
    }

    /// The facade, to read its state.
    #[must_use]
    pub const fn client(&self) -> &TransportClient<B, A> {
        &self.client
    }

    /// The store's rows, decoded: the first update a root applies.
    ///
    /// # Errors
    /// The store's, when it cannot be read.
    pub fn listing(&mut self) -> Result<Listing, StoreError> {
        listing::list_all(self.client.store_mut())
    }

    /// One turn of the facade: run it, hand over what it received, and
    /// report its events and any change in its counters. `now` is the
    /// caller's monotonic clock in milliseconds.
    pub async fn turn(&mut self, now: u64) -> Vec<Update> {
        let mut updates = Vec::new();
        // Boxed: the facade's futures are large.
        Box::pin(self.client.tick(now)).await;
        let received = Box::pin(self.client.drain(DRAIN_PER_TURN, now)).await;
        updates.extend(received.into_iter().map(Update::Received));
        while let Some(event) = self.client.next_event() {
            let relist = matches!(event, ClientEvent::UnreadInStore { .. });
            updates.push(Update::Client(event));
            if relist {
                // What the facade committed but could not hand over is in
                // the store: list it, so the person sees it now.
                if let Ok((unread, _)) = listing::list_unread(self.client.store_mut()) {
                    updates.push(Update::UnreadListed(unread));
                }
            }
        }
        let diagnostics = self.client.diagnostics();
        if self.reported != Some(diagnostics) {
            self.reported = Some(diagnostics);
            updates.push(Update::Diagnostics(diagnostics));
        }
        updates
    }

    /// Carry out `command`. The updates end with [`Update::Done`] or
    /// [`Update::Failed`] for it, whatever happened.
    pub async fn execute(&mut self, command: Command, now: u64) -> Vec<Update> {
        let mut updates = Vec::new();
        let ok = match &command {
            Command::Send { key, draft } => {
                updates.push(self.send(key, draft, now).await);
                true
            }
            Command::MarkRead(row) => {
                let at = (self.wall)();
                match self.client.store_mut().mark_read(*row, at) {
                    Ok(held) => {
                        self.copies.insert((Table::Unread, *row), held);
                        updates.push(Update::Read(*row));
                        true
                    }
                    Err(_) => false,
                }
            }
            Command::Keep { item, from } => {
                let at = (self.wall)();
                let kept = match self.copies.get(from) {
                    Some(held) => self.client.store_mut().keep(held, at).ok(),
                    None => None,
                };
                if let Some(row) = kept {
                    self.copies.remove(from);
                    updates.push(Update::Kept { item: *item, row });
                }
                kept.is_some()
            }
            Command::Unkeep(row) => match self.client.store_mut().unkeep(*row) {
                Ok(held) => {
                    if let Some(held) = held {
                        self.copies.insert((Table::Kept, *row), held);
                    }
                    updates.push(Update::Unkept(*row));
                    true
                }
                Err(_) => false,
            },
            Command::Retry(row) => Box::pin(self.client.retry(*row, now)).await.is_ok(),
            Command::Cancel(row) => self.client.cancel(*row).is_ok(),
            Command::Reopen => {
                self.client.reopen(now);
                true
            }
            Command::RecheckStorage => {
                self.client.recheck(now);
                true
            }
        };
        updates.push(if ok {
            Update::Done(command)
        } else {
            Update::Failed(command)
        });
        updates
    }

    /// Close the session: the endpoint lease is released. The daemon is
    /// not asked to stop -- closing a window never stops the transport
    /// (ADR-0040).
    pub async fn close(&mut self) {
        Box::pin(self.client.close()).await;
    }

    async fn send(&mut self, key: &ConversationKey, draft: &str, now: u64) -> Update {
        let destination = match key {
            ConversationKey::Direct { peer, endpoint } => Destination::Direct {
                peer: peer.clone(),
                endpoint: endpoint.clone(),
            },
            ConversationKey::Channel(channel) => Destination::Broadcast(channel.clone()),
        };
        let at = (self.wall)();
        let envelope = HumanChatV2 {
            v: 2,
            kind: MessageKind::Text,
            app_message_id: format!("{:032x}", rand::random::<u128>()),
            text: draft.to_owned(),
            reply_to: None,
            sent_at_ms: Some(at),
            from_endpoint: self.endpoint.clone(),
        };
        match Box::pin(self.client.send(destination.clone(), &envelope, now)).await {
            Ok(row) => Update::Sent {
                key: key.clone(),
                row,
                destination,
                envelope,
                at,
            },
            Err(error) => Update::SendRefused {
                key: key.clone(),
                draft: draft.to_owned(),
                error,
            },
        }
    }
}

/// The read-and-unkept copies a Keep needs, keyed by the row whose read
/// or unkeep handed them back; the oldest goes first past the cap.
/// Generic only so the bound is testable: a `ReadEphemeral` is built by the
/// store alone.
struct ReadCopies<V> {
    by_row: HashMap<(Table, RowId), V>,
    order: VecDeque<(Table, RowId)>,
}

impl<V> Default for ReadCopies<V> {
    fn default() -> Self {
        Self {
            by_row: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

impl<V> ReadCopies<V> {
    fn insert(&mut self, from: (Table, RowId), held: V) {
        if self.by_row.insert(from, held).is_none() {
            self.order.push_back(from);
        }
        while self.order.len() > READ_COPY_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.by_row.remove(&oldest);
            }
        }
    }

    fn get(&self, from: &(Table, RowId)) -> Option<&V> {
        self.by_row.get(from)
    }

    fn remove(&mut self, from: &(Table, RowId)) {
        if self.by_row.remove(from).is_some() {
            self.order.retain(|k| k != from);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.by_row.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copies_never_hold_more_than_the_cap_and_drop_the_oldest() {
        let row = |n: usize| {
            (
                Table::Unread,
                RowId::from_stored(i64::try_from(n).expect("small")),
            )
        };
        let mut copies = ReadCopies::default();
        for n in 0..=READ_COPY_CAP {
            copies.insert(row(n), n);
        }
        assert_eq!(copies.len(), READ_COPY_CAP, "never past the cap");
        assert!(copies.get(&row(0)).is_none(), "the oldest went first");
        assert_eq!(copies.get(&row(READ_COPY_CAP)), Some(&READ_COPY_CAP));
        // A re-insert of a held row does not count twice.
        copies.insert(row(READ_COPY_CAP), 0);
        assert_eq!(copies.len(), READ_COPY_CAP);
        assert_eq!(copies.order.len(), READ_COPY_CAP);
        copies.remove(&row(READ_COPY_CAP));
        assert_eq!(
            copies.order.len(),
            READ_COPY_CAP - 1,
            "removal clears the order too"
        );
    }
}
