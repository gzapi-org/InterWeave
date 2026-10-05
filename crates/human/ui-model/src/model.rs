// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The UI model: presentation state reduced from what the composition
//! root tells it, and the [`Intent`]s a person's actions raise. Pure and
//! synchronous (agreed item 1): it holds no facade and no store, does no
//! I/O, and never re-opens anything -- it shows `Reconnecting` while the
//! facade does.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use interweave_human_chat_protocol::{HumanChatV2, Rendered, is_allowed_link_scheme, render};
use interweave_human_client_api::{
    ClientEvent, Connectivity, Destination, Diagnostics, Origin, OutboundStatus, OutboundUpdate,
    SendError, SessionState,
};
use interweave_human_core::RowId;
use interweave_transport_api::{
    ChannelId, EndpointId, PeerPath, TransportError, TransportIdentity,
};

use crate::labels::{
    ErrorClass, LabelKey, UiText, fill, outbound_label, placeholder_en, send_error_class,
    session_problem_class, short_peer,
};

/// How many items the model holds that the store no longer does --
/// read-and-unkept inbound and terminal outbound -- before it evicts the
/// oldest of them (agreed item 3g). Everything else can be re-listed from
/// the store, so nothing else is evicted.
pub const SESSION_ITEM_CAP: usize = 1_024;

/// How many outbound updates the model holds for rows not listed yet,
/// latest per row, the oldest dropped past it (agreed U3b).
pub const HELD_UPDATE_CAP: usize = 1_024;

/// How many (origin, application id) pairs the model remembers, to attach
/// a copy to the item already shown, oldest forgotten first. The same
/// bound holds the released rows a stale listing cannot bring back.
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

/// What a view asks of the composition root: a legal intent, or an edit
/// to a draft. It is the view's output, defined here rather than in the
/// toolkit crate so a root that names no toolkit can handle it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewEvent {
    /// A legal intent, resolved against the model at take time.
    Intent(Intent),
    /// The person edited a conversation's draft: the root passes it to
    /// [`UiModel::draft_changed`] before taking the view's events again.
    DraftChanged {
        /// The conversation.
        key: ConversationKey,
        /// The draft as it now reads.
        draft: String,
    },
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
    /// Whether a kept row holds this message too: an unread re-sent copy
    /// of a kept message shows both (agreed U1b).
    pub kept: bool,
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
    /// Its title: a channel's name, or for a direct route the
    /// authenticated short `PeerId` and the route label the peer asserts,
    /// shown as a label (agreed item 3c). Never the envelope's
    /// `from_endpoint` or anything in the text.
    pub title: String,
    /// How many of its messages are unread.
    pub unread: usize,
    /// The latest local time of any of its messages.
    pub last_activity: u64,
}

/// A session state a person can act on (agreed item 3f).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionNotice {
    /// No transport daemon serves this profile: the window connects on its
    /// own once one starts. Guidance only -- starting the daemon is the
    /// operator's (architect-cto's Q9 ruling, relay seq 11163).
    NoDaemon,
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
            Self::NoDaemon | Self::Reconnecting => None,
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
    Keep {
        /// The item to keep.
        item: ItemKey,
        /// The row whose read or unkeep handed this session the content:
        /// the root keeps the copy that row's store call returned.
        from: (Table, RowId),
    },
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

/// How often a composer was edited, and at which count Send was pressed.
#[derive(Debug, Clone, Copy, Default)]
struct Edits {
    revision: u64,
    pressed: Option<u64>,
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

/// An outbound row's raw failure, for the diagnostics view only: kept
/// off [`MessageItem`] so a view cannot render a raw code by accident
/// (`human-client-ui.md` §12, agreed U2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemDiagnostics {
    /// The last transport failure the facade reported for the row.
    pub last_code: Option<TransportError>,
}

#[derive(Debug, Clone)]
struct Item {
    conversation: ConversationKey,
    direction: Direction,
    /// The store rows this item stands for: its own, and any duplicate
    /// attached to it (agreed U1). Empty once only this session holds it.
    rows: Vec<(Table, RowId)>,
    envelope: HumanChatV2,
    body: Rendered,
    author: Option<TransportIdentity>,
    route_label: Option<RouteLabel>,
    outbound: Option<OutboundStatus>,
    last_code: Option<TransportError>,
    local_at: u64,
    /// The last row whose release handed this session the content -- a
    /// read of an unread row, or an unkeep of a kept one. Keep is offered
    /// only with one, since the store keeps only content it handed back.
    released_from: Option<(Table, RowId)>,
}

impl Item {
    fn has(&self, table: Table) -> bool {
        self.rows.iter().any(|(t, _)| *t == table)
    }

    fn row_in(&self, table: Table) -> Option<RowId> {
        self.rows.iter().find(|(t, _)| *t == table).map(|(_, r)| *r)
    }

    /// An inbound item is Unread while ANY of its rows is, Kept while any
    /// is kept and none is unread, and read-ephemeral once none is held
    /// (agreed U1b).
    fn retention(&self) -> Retention {
        if self.has(Table::Unread) {
            Retention::Unread
        } else if self.has(Table::Kept) {
            Retention::Kept
        } else {
            Retention::ReadEphemeral
        }
    }

    fn status(&self) -> ItemStatus {
        self.outbound.as_ref().map_or_else(
            || ItemStatus::Inbound(self.retention()),
            |s| ItemStatus::Outbound(s.clone()),
        )
    }

    fn label(&self) -> LabelKey {
        match &self.outbound {
            Some(status) => outbound_label(status),
            None => match self.retention() {
                Retention::Unread => LabelKey::Unread,
                Retention::ReadEphemeral => LabelKey::ReadNotKept,
                Retention::Kept => LabelKey::Kept,
            },
        }
    }
}

/// The human client's presentation state.
#[derive(Debug)]
pub struct UiModel {
    items: BTreeMap<ItemKey, Item>,
    /// Which item a store row belongs to: a row is looked up, never
    /// searched for (review F9).
    by_row: HashMap<(Table, RowId), ItemKey>,
    /// The items holding an application id in a conversation, for replies.
    by_app: HashMap<(ConversationKey, String), Vec<ItemKey>>,
    /// Session-only items (no store row), oldest first, each once.
    ephemeral: VecDeque<ItemKey>,
    ephemeral_set: HashSet<ItemKey>,
    /// Every item an (origin, application id) is shown as: one per
    /// distinct envelope, since new text under an old id is a new item.
    seen: HashMap<(String, String), Vec<ItemKey>>,
    seen_order: VecDeque<(String, String)>,
    /// Rows the store no longer holds (read, unkept, terminal): a listing
    /// taken before the release cannot bring one back. Row ids are never
    /// reused (AUTOINCREMENT), so a released id is never a new row.
    released: HashSet<(Table, RowId)>,
    released_order: VecDeque<(Table, RowId)>,
    /// Outbound updates for rows not listed yet (agreed U3).
    held: HashMap<RowId, OutboundUpdate>,
    held_order: VecDeque<RowId>,
    composers: BTreeMap<ConversationKey, Composer>,
    /// Each composer's edit count and the count at its pending press, so
    /// an answer to a send tells an edit made after the press from the
    /// same text left unedited -- which comparing text cannot.
    edits: BTreeMap<ConversationKey, Edits>,
    /// No daemon serves the profile, as the root last saw it.
    daemon_absent: bool,
    connectivity: Connectivity,
    /// The path to each peer a direct conversation is with, once the
    /// runtime has said: the route indicator's (`human-client-ui.md` §7).
    paths: HashMap<TransportIdentity, PeerPath>,
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
            by_row: HashMap::new(),
            by_app: HashMap::new(),
            ephemeral: VecDeque::new(),
            ephemeral_set: HashSet::new(),
            seen: HashMap::new(),
            seen_order: VecDeque::new(),
            released: HashSet::new(),
            released_order: VecDeque::new(),
            held: HashMap::new(),
            held_order: VecDeque::new(),
            composers: BTreeMap::new(),
            edits: BTreeMap::new(),
            daemon_absent: false,
            connectivity: Connectivity::Unknown,
            paths: HashMap::new(),
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
                if self.by_row.contains_key(&(Table::Pending, update.row)) {
                    self.apply(update);
                } else {
                    // Not listed or sent yet: held, latest wins per row,
                    // the oldest dropped past the cap (agreed U3).
                    self.hold(update);
                }
            }
            // A route is a session's (`LOCAL-CLIENT.md` §2), and a path is
            // said only for a route and only on a change, so no path
            // outlives the session it was said in: every session event
            // clears them, `Ready` included -- the facade raises one only
            // on a change, so a `Ready` is a new session, and the queue may
            // have folded the ending into it
            // (`a_session_event_clears_every_path`).
            ClientEvent::Session(state) => {
                self.session = state;
                self.paths.clear();
            }
            ClientEvent::Connectivity(connectivity) => self.connectivity = connectivity,
            // A path change updates the route indicator and nothing else:
            // no item, no unread count, no conversation (`human-client-ui.md`
            // §7 and §13). Kept for a peer a direct conversation is with;
            // the facade hands a path change over after the messages of
            // the same take, so a first message's conversation is there.
            ClientEvent::PeerPath { peer, path } => {
                let known = self.items.values().any(|item| {
                    matches!(&item.conversation, ConversationKey::Direct { peer: p, .. } if *p == peer)
                });
                if known {
                    self.paths.insert(peer, path);
                }
            }
            // A peer's connection ending changes no conversation and adds
            // no message; its path is no longer known.
            ClientEvent::PeerDisconnected { peer } => {
                self.paths.remove(&peer);
            }
            ClientEvent::UnreadInStore { .. } => {}
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
                &row.origin,
                row.envelope,
                row.received_at,
            );
        }
    }

    /// A message this client sent: the facade committed `row`.
    ///
    /// The composer is cleared only if it was not edited since the press
    /// that sent it ([`send_pressed`](Self::send_pressed)): the commit is
    /// answered after the press, and anything typed in between -- the same
    /// text typed again included -- is the person's next message, not this
    /// one's. A send with no recorded press clears nothing.
    pub fn sent(&mut self, row: RowId, destination: &Destination, envelope: HumanChatV2, at: u64) {
        let conversation = conversation_of(destination);
        let unedited = self.take_press(&conversation);
        if let Some(composer) = self.composers.get_mut(&conversation) {
            if unedited {
                composer.draft.clear();
                self.edits.entry(conversation.clone()).or_default().revision += 1;
            }
            composer.refused = None;
        }
        self.outbound(row, conversation, envelope, at);
    }

    /// The person pressed Send in conversation `key`, for the draft as it
    /// reads now: the facade's answer may touch the composer only if it
    /// was not edited after this. The root calls it when it issues the
    /// send; one send per conversation is in flight at a time.
    pub fn send_pressed(&mut self, key: &ConversationKey) {
        let edit = self.edits.entry(key.clone()).or_default();
        edit.pressed = Some(edit.revision);
    }

    /// Whether the composer of `key` is unedited since the recorded press,
    /// forgetting the press: an answer is the press's only one.
    fn take_press(&mut self, key: &ConversationKey) -> bool {
        self.edits.get_mut(key).is_some_and(|edit| {
            let unedited = edit.pressed == Some(edit.revision);
            edit.pressed = None;
            unedited
        })
    }

    /// The store's pending rows, at start (agreed item 2a). Authoritative:
    /// a held update for a row it does not list is discarded, a terminal
    /// one included (agreed U3a).
    pub fn pending_listed(&mut self, rows: Vec<ListedOutbound>) {
        for row in rows {
            let conversation = conversation_of(&row.destination);
            self.outbound(row.row, conversation, row.envelope, row.created_at);
        }
        self.held.clear();
        self.held_order.clear();
    }

    /// The facade refused a send with no row: the composer keeps the
    /// draft and shows why (agreed item 2b).
    ///
    /// An edit made after the press -- clearing the composer included --
    /// is newer than the refusal and is never replaced by it. With no
    /// edit since the press the composer already holds the refused text;
    /// with no recorded press, the text is put back only into an empty
    /// composer.
    pub fn send_refused(&mut self, key: ConversationKey, draft: String, error: &SendError) {
        let pressed = self
            .edits
            .get(&key)
            .is_some_and(|edit| edit.pressed.is_some());
        let unedited = self.take_press(&key);
        let composer = self.composers.entry(key).or_default();
        if (pressed && unedited) || (!pressed && composer.draft.is_empty()) {
            composer.draft = draft;
        }
        composer.refused = Some(send_error_class(error));
    }

    /// Whether a transport daemon serves this profile, as the root sees
    /// it: `Some(true)`, `Some(false)`, or `None` when it cannot tell.
    /// Only `Some(false)` makes a reconnecting session's notice say no
    /// daemon runs; cannot-tell clears it, since the guidance to start
    /// the daemon may then be wrong.
    pub fn daemon_seen(&mut self, present: Option<bool>) {
        self.daemon_absent = present == Some(false);
    }

    /// The person edited a draft.
    pub fn draft_changed(&mut self, key: ConversationKey, draft: String) {
        self.edits.entry(key.clone()).or_default().revision += 1;
        let composer = self.composers.entry(key).or_default();
        composer.draft = draft;
        composer.refused = None;
    }

    /// The root marked an unread row read: its durable copy is gone, and
    /// the content stays for this session unless another row holds it.
    pub fn read(&mut self, row: RowId) {
        self.release(Table::Unread, row);
    }

    /// The content a Keep of `key` would keep is no longer held this
    /// session -- the root dropped its copy past a bound -- so Keep is no
    /// longer offered for it. The message stays shown, read, until it is
    /// evicted or the session ends.
    pub fn copy_gone(&mut self, key: ItemKey) {
        if let Some(item) = self.items.get_mut(&key) {
            item.released_from = None;
        }
    }

    /// The root kept the read message `key` as `kept_row`.
    pub fn kept(&mut self, key: ItemKey, kept_row: RowId) {
        if let Some(item) = self.items.get_mut(&key) {
            item.rows.push((Table::Kept, kept_row));
            self.by_row.insert((Table::Kept, kept_row), key);
            if self.ephemeral_set.remove(&key) {
                self.ephemeral.retain(|k| *k != key);
            }
        }
    }

    /// The root removed Keep from `kept_row`.
    pub fn unkept(&mut self, kept_row: RowId) {
        self.release(Table::Kept, kept_row);
    }

    /// The view shows conversation `key`: if the window has focus, its
    /// unread rows are read now. Read is a retention act -- it deletes the
    /// durable copy -- so it is raised from here alone, never on receipt,
    /// from a notification, or while unfocused (agreed 2c); `actions`
    /// does not offer it (review F1).
    #[must_use]
    pub fn conversation_viewed(&self, key: &ConversationKey, focused: bool) -> Vec<Intent> {
        if !focused {
            return Vec::new();
        }
        self.items
            .values()
            .filter(|i| &i.conversation == key)
            .flat_map(|i| {
                i.rows
                    .iter()
                    .filter(|(t, _)| *t == Table::Unread)
                    .map(|(_, r)| Intent::MarkRead(*r))
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
            if item.outbound.is_none() && item.has(Table::Unread) {
                summary.unread += 1;
            }
            summary.last_activity = summary.last_activity.max(item.local_at);
        }
        let mut list: Vec<_> = by.into_values().collect();
        list.sort_by(|a, b| {
            b.last_activity
                .cmp(&a.last_activity)
                .then_with(|| a.key.cmp(&b.key))
        });
        list
    }

    /// A conversation's messages, in LOCAL time order (A3). Replies are
    /// resolved here, within the conversation, so a target that was
    /// evicted or is ambiguous reads `Unavailable` (review F5).
    #[must_use]
    pub fn messages(&self, key: &ConversationKey) -> Vec<MessageItem> {
        let mut list: Vec<MessageItem> = self
            .items
            .iter()
            .filter(|(_, i)| &i.conversation == key)
            .map(|(k, i)| MessageItem {
                key: *k,
                direction: i.direction,
                status: i.status(),
                label: i.label(),
                kept: i.has(Table::Kept),
                body: i.body.clone(),
                source: i.envelope.text.clone(),
                author: i.author.clone(),
                route_label: i.route_label.clone(),
                reply: self.reply(&i.conversation, i.envelope.reply_to.as_deref()),
                sent_at_ms: i.envelope.sent_at_ms,
                local_at: i.local_at,
            })
            .collect();
        list.sort_by(|a, b| a.local_at.cmp(&b.local_at).then_with(|| a.key.cmp(&b.key)));
        list
    }

    /// The intents legal on `key` now (agreed item 3e). Never `MarkRead`:
    /// that comes from a focused view alone (review F1).
    #[must_use]
    pub fn actions(&self, key: ItemKey) -> Vec<Intent> {
        let Some(item) = self.items.get(&key) else {
            return Vec::new();
        };
        if let Some(status) = &item.outbound {
            return match item.row_in(Table::Pending) {
                Some(row) if !status.is_terminal() => vec![Intent::Retry(row), Intent::Cancel(row)],
                _ => Vec::new(),
            };
        }
        if let Some(kept_row) = item.row_in(Table::Kept) {
            vec![Intent::Unkeep(kept_row)]
        } else if let (true, Some(from)) = (item.rows.is_empty(), item.released_from) {
            // Read and not kept: Keep is offered only here, after read,
            // and only with the row whose release handed the content back.
            vec![Intent::Keep { item: key, from }]
        } else {
            Vec::new()
        }
    }

    /// An outbound row's raw failure, for the diagnostics view only.
    #[must_use]
    pub fn item_diagnostics(&self, key: ItemKey) -> Option<ItemDiagnostics> {
        self.items.get(&key).map(|i| ItemDiagnostics {
            last_code: i.last_code,
        })
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

    /// The path to the peer a direct conversation is with, once the
    /// runtime has said; `None` for a channel, or before it has.
    #[must_use]
    pub fn path(&self, key: &ConversationKey) -> Option<PeerPath> {
        match key {
            ConversationKey::Direct { peer, .. } => self.paths.get(peer).copied(),
            ConversationKey::Channel(_) => None,
        }
    }

    /// The session state a person can act on, if any.
    #[must_use]
    pub const fn session_notice(&self) -> Option<SessionNotice> {
        match &self.session {
            SessionState::Ready { .. } | SessionState::Closed => None,
            // In place of "reconnecting" only: re-opening cannot succeed
            // while nothing serves the profile, and the person can act on
            // that. Storage trouble and a refusal are said as they are.
            SessionState::Reconnecting { .. } if self.daemon_absent => {
                Some(SessionNotice::NoDaemon)
            }
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

    /// How many (conversation, application id) entries the reply index
    /// holds: at most one per item held, so it shrinks with eviction.
    #[must_use]
    pub fn indexed_ids(&self) -> usize {
        self.by_app.len()
    }

    /// How many item keys the copy index holds across every (origin,
    /// application id): an evicted item's key is dropped when its pair is
    /// next seen, so one id reused with ever-new text stays bounded.
    #[must_use]
    pub fn copy_index_keys(&self) -> usize {
        self.seen.values().map(Vec::len).sum()
    }

    /// How many released rows are remembered, at most [`DEDUP_CAP`].
    #[must_use]
    pub fn released_rows(&self) -> usize {
        self.released.len()
    }

    /// How many outbound updates are held for rows not listed yet.
    #[must_use]
    pub fn held_updates(&self) -> usize {
        self.held.len()
    }

    // --- internals ---------------------------------------------------------

    fn inbound(
        &mut self,
        row: RowId,
        table: Table,
        origin: &Origin,
        envelope: HumanChatV2,
        at: u64,
    ) {
        // Merged by row: the same row listed again is the same item, and a
        // row already released is not listed back by a stale snapshot.
        if self.by_row.contains_key(&(table, row)) || self.released.contains(&(table, row)) {
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
        // A second copy of one message is one item -- but a store row is
        // never hidden: the copy's row is ATTACHED to the item, so it is
        // counted, read when viewed, and unkept, like the first (agreed
        // U1). Only an identical envelope is a copy: the id is chosen by
        // the peer, and new text under an old id is a new item (U1a).
        let dedup = (origin_key, envelope.app_message_id.clone());
        let copy_of = self.seen.get(&dedup).and_then(|keys| {
            keys.iter()
                .copied()
                .find(|k| self.items.get(k).is_some_and(|i| i.envelope == envelope))
        });
        if let Some(existing) = copy_of
            && let Some(item) = self.items.get_mut(&existing)
        {
            item.rows.push((table, row));
            self.by_row.insert((table, row), existing);
            if self.ephemeral_set.remove(&existing) {
                self.ephemeral.retain(|k| *k != existing);
            }
            return;
        }
        let key = ItemKey(table, row);
        self.remember(dedup, key);
        self.by_app
            .entry((conversation.clone(), envelope.app_message_id.clone()))
            .or_default()
            .push(key);
        self.by_row.insert((table, row), key);
        self.items.insert(
            key,
            Item {
                conversation,
                direction: Direction::Inbound,
                rows: vec![(table, row)],
                body: render(&envelope.text),
                envelope,
                author: Some(author),
                route_label,
                outbound: None,
                last_code: None,
                local_at: at,
                released_from: None,
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
        // Merged by row, and a row already terminal is not listed back by
        // a snapshot taken before its terminal update.
        if self.by_row.contains_key(&(Table::Pending, row))
            || self.released.contains(&(Table::Pending, row))
        {
            return;
        }
        let key = ItemKey(Table::Pending, row);
        self.by_app
            .entry((conversation.clone(), envelope.app_message_id.clone()))
            .or_default()
            .push(key);
        self.by_row.insert((Table::Pending, row), key);
        let route_label = match &conversation {
            ConversationKey::Direct {
                endpoint: Some(endpoint),
                ..
            } => Some(RouteLabel::from(endpoint)),
            _ => None,
        };
        self.items.insert(
            key,
            Item {
                conversation,
                direction: Direction::Outbound,
                rows: vec![(Table::Pending, row)],
                body: render(&envelope.text),
                envelope,
                author: None,
                route_label,
                outbound: Some(OutboundStatus::Sending {
                    attempts: 0,
                    next_retry_at: None,
                    last_problem: None,
                }),
                last_code: None,
                local_at: at,
                released_from: None,
            },
        );
        // A status the facade reported before the row was listed.
        if let Some(update) = self.held.remove(&row) {
            self.held_order.retain(|r| *r != row);
            self.apply(update);
        }
    }

    fn apply(&mut self, update: OutboundUpdate) {
        let Some(key) = self.by_row.get(&(Table::Pending, update.row)).copied() else {
            return;
        };
        let terminal = update.status.is_terminal();
        if let Some(item) = self.items.get_mut(&key) {
            item.outbound = Some(update.status);
            if update.last_code.is_some() {
                item.last_code = update.last_code;
            }
        }
        if terminal {
            self.release(Table::Pending, update.row);
        }
    }

    fn hold(&mut self, update: OutboundUpdate) {
        let row = update.row;
        if self.held.insert(row, update).is_none() {
            self.held_order.push_back(row);
            while self.held_order.len() > HELD_UPDATE_CAP {
                if let Some(oldest) = self.held_order.pop_front() {
                    self.held.remove(&oldest);
                }
            }
        }
    }

    /// A store row went: the item keeps showing, and becomes session-only
    /// once it holds no row.
    fn release(&mut self, table: Table, row: RowId) {
        let Some(key) = self.by_row.remove(&(table, row)) else {
            return;
        };
        if self.released.insert((table, row)) {
            self.released_order.push_back((table, row));
            while self.released_order.len() > DEDUP_CAP {
                if let Some(oldest) = self.released_order.pop_front() {
                    self.released.remove(&oldest);
                }
            }
        }
        let now_session_only = self.items.get_mut(&key).is_some_and(|item| {
            item.rows.retain(|r| *r != (table, row));
            if table != Table::Pending {
                item.released_from = Some((table, row));
            }
            item.rows.is_empty()
        });
        if now_session_only && self.ephemeral_set.insert(key) {
            self.ephemeral.push_back(key);
            self.evict();
        }
    }

    fn reply(&self, conversation: &ConversationKey, target: Option<&str>) -> Option<Reply> {
        let target = target?;
        let holders = self
            .by_app
            .get(&(conversation.clone(), target.to_owned()))
            .map(|keys| {
                keys.iter()
                    .filter(|k| self.items.contains_key(k))
                    .copied()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Some(match holders.as_slice() {
            [only] => Reply::Present(*only),
            _ => Reply::Unavailable(target.to_owned()),
        })
    }

    fn remember(&mut self, pair: (String, String), key: ItemKey) {
        let keys = self.seen.entry(pair.clone()).or_default();
        let new_pair = keys.is_empty();
        // Only keys still held: an evicted item can no longer be a copy.
        keys.retain(|k| self.items.contains_key(k));
        keys.push(key);
        if new_pair {
            self.seen_order.push_back(pair);
            while self.seen_order.len() > DEDUP_CAP {
                if let Some(oldest) = self.seen_order.pop_front() {
                    self.seen.remove(&oldest);
                }
            }
        }
    }

    /// Evict the oldest session-only items past [`SESSION_ITEM_CAP`]:
    /// only those, since everything else is re-listed from the store.
    fn evict(&mut self) {
        while self.ephemeral.len() > SESSION_ITEM_CAP {
            let Some(key) = self.ephemeral.pop_front() else {
                return;
            };
            self.ephemeral_set.remove(&key);
            if let Some(item) = self.items.remove(&key) {
                let index = (item.conversation, item.envelope.app_message_id);
                if let Some(keys) = self.by_app.get_mut(&index) {
                    keys.retain(|k| *k != key);
                    if keys.is_empty() {
                        self.by_app.remove(&index);
                    }
                }
            }
        }
    }
}

fn conversation_of(destination: &Destination) -> ConversationKey {
    match destination {
        Destination::Direct { peer, endpoint } => ConversationKey::Direct {
            peer: peer.clone(),
            endpoint: endpoint.clone(),
        },
        Destination::Broadcast(channel) => ConversationKey::Channel(channel.clone()),
    }
}

/// A conversation's title: a channel's name, or a direct route's
/// authenticated short `PeerId` with the route label the peer asserts,
/// shown as a label beside it (agreed item 3c). The envelope's own
/// `from_endpoint` and the message text never enter it.
fn title(key: &ConversationKey) -> String {
    match key {
        ConversationKey::Channel(channel) => format!("#{}", channel.as_str()),
        ConversationKey::Direct { peer, endpoint } => {
            let short = short_peer(peer.as_str());
            endpoint.as_ref().map_or_else(
                || short.clone(),
                |e| {
                    fill(
                        placeholder_en::text(UiText::DirectTitle),
                        &[("peer", &short), ("route", e.as_str())],
                    )
                },
            )
        }
    }
}
