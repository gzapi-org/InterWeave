// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The UI model: presentation state reduced from what the composition
//! root tells it, and the [`Intent`]s a person's actions raise. Pure and
//! synchronous (agreed item 1): it holds no facade and no store, does no
//! I/O, and never re-opens anything -- it shows `Reconnecting` while the
//! facade does.

use std::collections::{BTreeMap, HashSet, VecDeque};

use interweave_human_chat_protocol::{HumanChatV2, Rendered, is_allowed_link_scheme, render};
use interweave_human_client_api::{
    ClientEvent, Connectivity, Destination, Diagnostics, Origin, OutboundStatus, SendError,
    SessionState,
};
use interweave_human_core::RowId;
use interweave_transport_api::{ChannelId, EndpointId, TransportIdentity};

use crate::labels::{
    ErrorClass, LabelKey, outbound_label, send_error_class, session_problem_class,
};

/// How many items the model holds that the store no longer does --
/// read-and-unkept inbound and terminal outbound -- before it evicts the
/// oldest of them (agreed item 3g). Everything else can be re-listed from
/// the store, so nothing else is evicted.
pub const SESSION_ITEM_CAP: usize = 1_024;

/// How many (origin, application id) pairs the model remembers to drop a
/// duplicate within a session, oldest forgotten first.
pub const DEDUP_CAP: usize = 4_096;

/// A conversation: a direct route or a channel (`human-client-ui.md` §4).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConversationKey {
    /// A direct conversation, keyed by the remote route.
    Direct {
        /// The remote peer.
        peer: TransportIdentity,
        /// The remote endpoint, or `None` for the peer's default.
        endpoint: Option<EndpointId>,
    },
    /// A channel conversation.
    Channel(ChannelId),
}

/// An `EndpointId` as a view shows it: a routing label, never an
/// identity, a person or an authenticated role (`human-client-ui.md` §3).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteLabel(String);

impl RouteLabel {
    /// The label's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&EndpointId> for RouteLabel {
    fn from(endpoint: &EndpointId) -> Self {
        Self(endpoint.as_str().to_owned())
    }
}

/// What trust the view may show for a peer. Stage 14 has no source for
/// it (plan §17 (4)): every peer is "not verified by this client", never
/// an invented value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// No verification by this client.
    NotVerifiedByThisClient,
}

/// Which table an item's row is in: a row id names a row within one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Table {
    /// `pending_outbound`.
    Pending,
    /// `unread_inbound`.
    Unread,
    /// `kept_inbound`.
    Kept,
}

/// A stable key for a list item, so a view keeps focus and selection
/// when the list changes (agreed item 3b). An item keeps the key it was
/// first shown under for the session, even when its row moves table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemKey(Table, RowId);

/// An inbound message's retention state (`human-client-ui.md` §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// Durable until read.
    Unread,
    /// Read and not kept: shown this session only.
    ReadEphemeral,
    /// Read and kept by this receiver.
    Kept,
}

/// Which way a message went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Sent by this client.
    Outbound,
    /// Received.
    Inbound,
}

/// A message's status, for its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemStatus {
    /// An outbound row's status, as the facade reports it.
    Outbound(OutboundStatus),
    /// An inbound message's retention state.
    Inbound(Retention),
}

/// What a message replies to, if anything (`human-client-ui.md` §15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The referenced message is shown in this session.
    Present(ItemKey),
    /// It is not held here: show a neutral placeholder. Never fetched,
    /// never a reason to doubt the message.
    Unavailable(String),
}

/// One message as a view renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageItem {
    /// Its stable key.
    pub key: ItemKey,
    /// Which way it went.
    pub direction: Direction,
    /// Its status.
    pub status: ItemStatus,
    /// The status's label key.
    pub label: LabelKey,
    /// The text, rendered within the subset.
    pub body: Rendered,
    /// The text as written: always viewable (`human-client-ui.md` §4).
    pub source: String,
    /// The authenticated sender of an inbound message, or `None` for
    /// this client's own.
    pub author: Option<TransportIdentity>,
    /// The route label the sender asserted, display only.
    pub route_label: Option<RouteLabel>,
    /// What it replies to.
    pub reply: Option<Reply>,
    /// When the sender says it sent it: peer-asserted, display only,
    /// never an order (A3).
    pub sent_at_ms: Option<u64>,
    /// The local wall-clock time the list is ordered by.
    pub local_at: u64,
}

/// One conversation in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSummary {
    /// Its key.
    pub key: ConversationKey,
    /// Its title: a channel's name, or for a direct route the short
    /// `PeerId` and the route label -- never anything the peer asserted
    /// about itself (agreed item 3c).
    pub title: String,
    /// How many of its messages are unread.
    pub unread: usize,
    /// The latest local time of any of its messages.
    pub last_activity: u64,
}

/// A session state a person can act on (agreed item 3f).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionNotice {
    /// The facade is re-opening on its own.
    Reconnecting,
    /// Storage cannot hold new messages; [`Intent::RecheckStorage`]
    /// resolves it once space is freed.
    StorageDegraded,
    /// Opening was refused; [`Intent::Reopen`] tries again.
    Refused(ErrorClass),
}

impl SessionNotice {
    /// The intent that resolves it, if a person can.
    #[must_use]
    pub const fn resolution(self) -> Option<Intent> {
        match self {
            Self::Reconnecting => None,
            Self::StorageDegraded => Some(Intent::RecheckStorage),
            Self::Refused(_) => Some(Intent::Reopen),
        }
    }
}

/// What a person's action asks the composition root to do (agreed items
/// 1, 3e). The only way anything leaves the model: a view never calls the
/// facade or the store. None of these touches trust, administration or
/// recovery -- `human-client-ui.md` §13's tests assert that by
/// exhaustive match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Mark an unread row read (deletes its durable copy unless kept).
    MarkRead(RowId),
    /// Keep a read message: only after it was read.
    Keep(ItemKey),
    /// Remove Keep from a kept row.
    Unkeep(RowId),
    /// Retry a pending row now.
    Retry(RowId),
    /// Cancel a pending row.
    Cancel(RowId),
    /// Send the conversation's draft.
    Send {
        /// Where.
        key: ConversationKey,
        /// What.
        draft: String,
    },
    /// Open an allowlisted link a person activated.
    OpenLink(String),
    /// Leave a refused session: try opening again.
    Reopen,
    /// Re-check storage now.
    RecheckStorage,
}

/// A conversation's composer: its draft and why the last send was
/// refused, if it was (agreed item 2b).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composer {
    /// The text being written.
    pub draft: String,
    /// Why the facade refused the last send, with no row.
    pub refused: Option<ErrorClass>,
}

/// An outbound message the store holds pending, as the root read it
/// (agreed item 2a: at start, so a restart has content to show).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedOutbound {
    /// Its pending row.
    pub row: RowId,
    /// Where it is going.
    pub destination: Destination,
    /// The envelope it carries.
    pub envelope: HumanChatV2,
    /// When it was composed, on the wall clock.
    pub created_at: u64,
}

/// An inbound message the store holds, as the root read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedInbound {
    /// Its row.
    pub row: RowId,
    /// Who sent it.
    pub origin: Origin,
    /// The envelope.
    pub envelope: HumanChatV2,
    /// When it was received, on the wall clock.
    pub received_at: u64,
}

#[derive(Debug, Clone)]
struct Item {
    conversation: ConversationKey,
    view: MessageItem,
    /// The row it is in now: the key's row until it moves table.
    row: Option<(Table, RowId)>,
    app_message_id: Option<String>,
}

/// The human client's presentation state.
#[derive(Debug)]
pub struct UiModel {
    items: BTreeMap<ItemKey, Item>,
    /// Session-only items (read-unkept, terminal outbound), oldest first.
    ephemeral: VecDeque<ItemKey>,
    seen: HashSet<(String, String)>,
    seen_order: VecDeque<(String, String)>,
    composers: BTreeMap<ConversationKey, Composer>,
    connectivity: Connectivity,
    session: SessionState,
    diagnostics: Diagnostics,
}

impl Default for UiModel {
    fn default() -> Self {
        Self::new()
    }
}

impl UiModel {
    /// An empty model: connectivity unknown, the facade reconnecting.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: BTreeMap::new(),
            ephemeral: VecDeque::new(),
            seen: HashSet::new(),
            seen_order: VecDeque::new(),
            composers: BTreeMap::new(),
            connectivity: Connectivity::Unknown,
            session: SessionState::Reconnecting {
                attempt: 0,
                next_at: 0,
            },
            diagnostics: Diagnostics::default(),
        }
    }

    // --- inputs ------------------------------------------------------------

    /// An event the facade emitted.
    pub fn client_event(&mut self, event: ClientEvent) {
        match event {
            ClientEvent::Outbound(update) => {
                let key = ItemKey(Table::Pending, update.row);
                let terminal = update.status.is_terminal();
                if let Some(item) = self.items.get_mut(&key) {
                    item.view.label = outbound_label(&update.status);
                    item.view.status = ItemStatus::Outbound(update.status);
                    if terminal {
                        item.row = None;
                        self.ephemeral.push_back(key);
                    }
                }
                if terminal {
                    self.evict();
                }
            }
            ClientEvent::Session(state) => self.session = state,
            ClientEvent::Connectivity(connectivity) => self.connectivity = connectivity,
            // A peer's connection ending changes no conversation and adds
            // no message: a path change is not a new logical event
            // (`human-client-ui.md` §13, at the model until Stage 15).
            ClientEvent::PeerDisconnected { .. } | ClientEvent::UnreadInStore { .. } => {}
        }
    }

    /// The facade's diagnostic counts, for the diagnostics view.
    pub const fn diagnostics_updated(&mut self, diagnostics: Diagnostics) {
        self.diagnostics = diagnostics;
    }

    /// A message `drain` handed over: committed unread already.
    pub fn received(&mut self, received: interweave_human_client_api::Received) {
        self.inbound(
            received.row,
            Table::Unread,
            Retention::Unread,
            &received.origin,
            received.envelope,
            received.received_at,
        );
    }

    /// The store's unread rows, at start or after `UnreadInStore`: merged
    /// by row id with what is already shown (agreed A5, rule 2).
    pub fn unread_listed(&mut self, rows: Vec<ListedInbound>) {
        for row in rows {
            self.inbound(
                row.row,
                Table::Unread,
                Retention::Unread,
                &row.origin,
                row.envelope,
                row.received_at,
            );
        }
    }

    /// The store's kept rows, at start.
    pub fn kept_listed(&mut self, rows: Vec<ListedInbound>) {
        for row in rows {
            self.inbound(
                row.row,
                Table::Kept,
                Retention::Kept,
                &row.origin,
                row.envelope,
                row.received_at,
            );
        }
    }

    /// A message this client sent: the facade committed `row`.
    pub fn sent(&mut self, row: RowId, destination: &Destination, envelope: HumanChatV2, at: u64) {
        let conversation = match destination {
            Destination::Direct { peer, endpoint } => ConversationKey::Direct {
                peer: peer.clone(),
                endpoint: endpoint.clone(),
            },
            Destination::Broadcast(channel) => ConversationKey::Channel(channel.clone()),
        };
        if let Some(composer) = self.composers.get_mut(&conversation) {
            composer.draft.clear();
            composer.refused = None;
        }
        self.outbound(row, conversation, envelope, at);
    }

    /// The store's pending rows, at start (agreed item 2a).
    pub fn pending_listed(&mut self, rows: Vec<ListedOutbound>) {
        for row in rows {
            let conversation = match &row.destination {
                Destination::Direct { peer, endpoint } => ConversationKey::Direct {
                    peer: peer.clone(),
                    endpoint: endpoint.clone(),
                },
                Destination::Broadcast(channel) => ConversationKey::Channel(channel.clone()),
            };
            self.outbound(row.row, conversation, row.envelope, row.created_at);
        }
    }

    /// The facade refused a send with no row: the composer keeps the
    /// draft and shows why (agreed item 2b).
    pub fn send_refused(&mut self, key: ConversationKey, draft: String, error: &SendError) {
        let composer = self.composers.entry(key).or_default();
        composer.draft = draft;
        composer.refused = Some(send_error_class(error));
    }

    /// The person edited a draft.
    pub fn draft_changed(&mut self, key: ConversationKey, draft: String) {
        let composer = self.composers.entry(key).or_default();
        composer.draft = draft;
        composer.refused = None;
    }

    /// The root marked an unread row read: its durable copy is gone, its
    /// content stays for this session.
    pub fn read(&mut self, row: RowId) {
        let Some(key) = self.key_of(Table::Unread, row) else {
            return;
        };
        if let Some(item) = self.items.get_mut(&key) {
            item.view.status = ItemStatus::Inbound(Retention::ReadEphemeral);
            item.view.label = LabelKey::ReadNotKept;
            item.row = None;
        }
        self.ephemeral.push_back(key);
        self.evict();
    }

    /// The root kept a read message as `kept_row`.
    pub fn kept(&mut self, key: ItemKey, kept_row: RowId) {
        if let Some(item) = self.items.get_mut(&key) {
            item.view.status = ItemStatus::Inbound(Retention::Kept);
            item.view.label = LabelKey::Kept;
            item.row = Some((Table::Kept, kept_row));
            self.ephemeral.retain(|k| *k != key);
        }
    }

    /// The root removed Keep from `kept_row`: back to this session only.
    pub fn unkept(&mut self, kept_row: RowId) {
        let Some(key) = self.key_of(Table::Kept, kept_row) else {
            return;
        };
        if let Some(item) = self.items.get_mut(&key) {
            item.view.status = ItemStatus::Inbound(Retention::ReadEphemeral);
            item.view.label = LabelKey::ReadNotKept;
            item.row = None;
        }
        self.ephemeral.push_back(key);
        self.evict();
    }

    /// The view shows conversation `key`: if the window has focus, its
    /// unread messages are read now. Read is a retention act -- it
    /// deletes the durable copy -- so it comes only from here, never on
    /// receipt, from a notification, or while unfocused (agreed 2c).
    #[must_use]
    pub fn conversation_viewed(&self, key: &ConversationKey, focused: bool) -> Vec<Intent> {
        if !focused {
            return Vec::new();
        }
        self.items
            .values()
            .filter(|i| &i.conversation == key)
            .filter_map(|i| match (i.view.status.clone(), i.row) {
                (ItemStatus::Inbound(Retention::Unread), Some((Table::Unread, row))) => {
                    Some(Intent::MarkRead(row))
                }
                _ => None,
            })
            .collect()
    }

    /// A person activated a link in an item: an intent only for an
    /// allowlisted scheme (`HUMAN-CHAT.md`). Remote text alone raises
    /// nothing -- there is no path from receipt to an intent.
    #[must_use]
    pub fn link_activated(&self, destination: &str) -> Option<Intent> {
        is_allowed_link_scheme(destination).then(|| Intent::OpenLink(destination.to_owned()))
    }

    // --- outputs -----------------------------------------------------------

    /// Every conversation, most recent activity first.
    #[must_use]
    pub fn conversations(&self) -> Vec<ConversationSummary> {
        let mut by: BTreeMap<&ConversationKey, ConversationSummary> = BTreeMap::new();
        for item in self.items.values() {
            let summary = by
                .entry(&item.conversation)
                .or_insert_with(|| ConversationSummary {
                    key: item.conversation.clone(),
                    title: title(&item.conversation),
                    unread: 0,
                    last_activity: 0,
                });
            if item.view.status == ItemStatus::Inbound(Retention::Unread) {
                summary.unread += 1;
            }
            summary.last_activity = summary.last_activity.max(item.view.local_at);
        }
        let mut list: Vec<_> = by.into_values().collect();
        list.sort_by(|a, b| {
            b.last_activity
                .cmp(&a.last_activity)
                .then_with(|| a.key.cmp(&b.key))
        });
        list
    }

    /// A conversation's messages, in LOCAL time order (A3).
    #[must_use]
    pub fn messages(&self, key: &ConversationKey) -> Vec<MessageItem> {
        let mut list: Vec<MessageItem> = self
            .items
            .values()
            .filter(|i| &i.conversation == key)
            .map(|i| i.view.clone())
            .collect();
        list.sort_by(|a, b| a.local_at.cmp(&b.local_at).then_with(|| a.key.cmp(&b.key)));
        list
    }

    /// The intents legal on `key` now (agreed item 3e).
    #[must_use]
    pub fn actions(&self, key: ItemKey) -> Vec<Intent> {
        let Some(item) = self.items.get(&key) else {
            return Vec::new();
        };
        match (&item.view.status, item.row) {
            (ItemStatus::Inbound(Retention::Unread), Some((Table::Unread, row))) => {
                vec![Intent::MarkRead(row)]
            }
            // Keep only after read, and only while the content is held.
            (ItemStatus::Inbound(Retention::ReadEphemeral), _) => vec![Intent::Keep(key)],
            (ItemStatus::Inbound(Retention::Kept), Some((Table::Kept, row))) => {
                vec![Intent::Unkeep(row)]
            }
            (ItemStatus::Outbound(status), Some((Table::Pending, row)))
                if !status.is_terminal() =>
            {
                vec![Intent::Retry(row), Intent::Cancel(row)]
            }
            _ => Vec::new(),
        }
    }

    /// A conversation's composer.
    #[must_use]
    pub fn composer(&self, key: &ConversationKey) -> Composer {
        self.composers.get(key).cloned().unwrap_or_default()
    }

    /// The send intent for a conversation's draft, if there is one.
    #[must_use]
    pub fn send_draft(&self, key: &ConversationKey) -> Option<Intent> {
        let draft = &self.composers.get(key)?.draft;
        (!draft.is_empty()).then(|| Intent::Send {
            key: key.clone(),
            draft: draft.clone(),
        })
    }

    /// Connectivity: always present, `Unknown` never shown as `Offline`.
    #[must_use]
    pub const fn connectivity(&self) -> Connectivity {
        self.connectivity
    }

    /// The session state a person can act on, if any.
    #[must_use]
    pub const fn session_notice(&self) -> Option<SessionNotice> {
        match &self.session {
            SessionState::Ready { .. } | SessionState::Closed => None,
            SessionState::Reconnecting { .. } => Some(SessionNotice::Reconnecting),
            SessionState::StorageDegraded => Some(SessionNotice::StorageDegraded),
            SessionState::Refused { problem } => {
                Some(SessionNotice::Refused(session_problem_class(*problem)))
            }
        }
    }

    /// What trust to show for `peer`.
    #[must_use]
    pub const fn trust(&self, _peer: &TransportIdentity) -> Trust {
        Trust::NotVerifiedByThisClient
    }

    /// The facade's counts, for the diagnostics view.
    #[must_use]
    pub const fn diagnostics(&self) -> Diagnostics {
        self.diagnostics
    }

    /// How many items the model holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    // --- internals ---------------------------------------------------------

    fn inbound(
        &mut self,
        row: RowId,
        table: Table,
        retention: Retention,
        origin: &Origin,
        envelope: HumanChatV2,
        at: u64,
    ) {
        // Merged by row: the same row listed again is the same item.
        if self.key_of(table, row).is_some() {
            return;
        }
        let (conversation, author, route_label, origin_key) = match origin {
            Origin::Direct { peer, endpoint } => (
                ConversationKey::Direct {
                    peer: peer.clone(),
                    endpoint: Some(endpoint.clone()),
                },
                peer.clone(),
                Some(RouteLabel::from(endpoint)),
                format!("direct:{}:{}", peer.as_str(), endpoint.as_str()),
            ),
            Origin::Channel { channel, publisher } => (
                ConversationKey::Channel(channel.clone()),
                publisher.clone(),
                None,
                format!("channel:{}:{}", channel.as_str(), publisher.as_str()),
            ),
        };
        // A second copy of one message within the session is one message
        // (agreed 2d). Across a restart, once the first was read and not
        // kept, a late copy shows again: closing that needs read ids
        // retained, a RETENTION.md question.
        let dedup = (origin_key, envelope.app_message_id.clone());
        if self.seen.contains(&dedup) {
            return;
        }
        self.remember(dedup);
        let reply = self.reply(&envelope);
        let key = ItemKey(table, row);
        let item = MessageItem {
            key,
            direction: Direction::Inbound,
            status: ItemStatus::Inbound(retention),
            label: match retention {
                Retention::Unread => LabelKey::Unread,
                Retention::ReadEphemeral => LabelKey::ReadNotKept,
                Retention::Kept => LabelKey::Kept,
            },
            body: render(&envelope.text),
            source: envelope.text.clone(),
            author: Some(author),
            route_label,
            reply,
            sent_at_ms: envelope.sent_at_ms,
            local_at: at,
        };
        self.items.insert(
            key,
            Item {
                conversation,
                view: item,
                row: Some((table, row)),
                app_message_id: Some(envelope.app_message_id),
            },
        );
    }

    fn outbound(
        &mut self,
        row: RowId,
        conversation: ConversationKey,
        envelope: HumanChatV2,
        at: u64,
    ) {
        if self.key_of(Table::Pending, row).is_some() {
            return;
        }
        let status = OutboundStatus::Sending {
            attempts: 0,
            next_retry_at: None,
            last_problem: None,
        };
        let key = ItemKey(Table::Pending, row);
        let reply = self.reply(&envelope);
        let item = MessageItem {
            key,
            direction: Direction::Outbound,
            label: outbound_label(&status),
            status: ItemStatus::Outbound(status),
            body: render(&envelope.text),
            source: envelope.text.clone(),
            author: None,
            route_label: match &conversation {
                ConversationKey::Direct {
                    endpoint: Some(endpoint),
                    ..
                } => Some(RouteLabel::from(endpoint)),
                _ => None,
            },
            reply,
            sent_at_ms: envelope.sent_at_ms,
            local_at: at,
        };
        self.items.insert(
            key,
            Item {
                conversation,
                view: item,
                row: Some((Table::Pending, row)),
                app_message_id: Some(envelope.app_message_id),
            },
        );
    }

    fn reply(&self, envelope: &HumanChatV2) -> Option<Reply> {
        let target = envelope.reply_to.as_ref()?;
        let found = self
            .items
            .iter()
            .find(|(_, i)| i.app_message_id.as_deref() == Some(target.as_str()))
            .map(|(k, _)| *k);
        Some(found.map_or_else(|| Reply::Unavailable(target.clone()), Reply::Present))
    }

    fn key_of(&self, table: Table, row: RowId) -> Option<ItemKey> {
        self.items
            .iter()
            .find(|(_, i)| i.row == Some((table, row)))
            .map(|(k, _)| *k)
    }

    fn remember(&mut self, pair: (String, String)) {
        if self.seen_order.len() >= DEDUP_CAP
            && let Some(oldest) = self.seen_order.pop_front()
        {
            self.seen.remove(&oldest);
        }
        self.seen.insert(pair.clone());
        self.seen_order.push_back(pair);
    }

    /// Evict the oldest session-only items past [`SESSION_ITEM_CAP`]:
    /// only those, since everything else is re-listed from the store.
    fn evict(&mut self) {
        while self.ephemeral.len() > SESSION_ITEM_CAP {
            if let Some(key) = self.ephemeral.pop_front() {
                self.items.remove(&key);
            }
        }
    }
}

/// A conversation's title: a channel's name, or a direct route's short
/// `PeerId` and route label -- never anything the peer asserted about
/// itself (agreed item 3c, `human-client-ui.md` §2, §3).
fn title(key: &ConversationKey) -> String {
    match key {
        ConversationKey::Channel(channel) => format!("#{}", channel.as_str()),
        ConversationKey::Direct { peer, endpoint } => {
            let id = peer.as_str();
            let short = &id[id.len().saturating_sub(8)..];
            endpoint.as_ref().map_or_else(
                || format!("…{short}"),
                |e| format!("…{short} / {}", e.as_str()),
            )
        }
    }
}
