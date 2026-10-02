// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The facade: the client's half of retention over any
//! [`DataSessionBinding`], with the admin `Status` connection beside it
//! for connectivity (plan §17 (1), (5)).
//!
//! Poll-driven, on purpose: [`TransportClient::tick`] runs what is due,
//! every method takes the caller's monotonic clock in milliseconds, and
//! nothing is spawned -- so the same code runs in-process on Android,
//! which has no shell loop to drive one, and a test replays a schedule
//! exactly.

use std::collections::{BTreeMap, BTreeSet};

use interweave_human_chat_protocol::{
    ContentEncoding, HumanChatV2, decode_envelope_bytes, encode_outbound, parse_media_type,
};
use interweave_human_store::{
    AppMessageId, HumanStore, InboundOrigin, NewInbound, NewOutbound, OutboundDestination,
    PageLimits, PendingOutbound, RowId, StorageHealth, StoreError, TerminalCause,
};
use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, DataCapability, DataSessionBinding, DataSessionPort,
    LocalSessionEvent, SessionEvent, SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectInboundState, EndpointId, Health, MediaType, MessageId,
    PathReadiness, Payload, TransportError, TransportIdentity,
};

use crate::backoff::{RECHECK, REOPEN, SEND};
use crate::model::{
    ClientEvent, Connectivity, Diagnostics, Origin, OutboundStatus, OutboundUpdate, Received,
    SessionState,
};
use crate::problem::{
    AttemptFailure, OpenFailure, SendProblem, classify_open, classify_send, ends_session,
};
use crate::queue::EventQueue;

/// How often a healthy admin connection is asked for status.
const STATUS_INTERVAL_MS: u64 = 5_000;

/// What the facade is configured with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// The client kind the session declares.
    pub client_kind: String,
    /// The endpoint to lease, or `None` for a session that only sends
    /// broadcasts and receives none directly.
    pub endpoint: Option<EndpointId>,
    /// The channels to join while the session is ready.
    pub channels: Vec<ChannelId>,
    /// The transport's effective payload limit for an outbound message.
    pub max_payload_bytes: usize,
}

/// Where a new message is going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// One remote endpoint, or the peer's configured default.
    Direct {
        /// The peer.
        peer: TransportIdentity,
        /// The endpoint, or `None` for the peer's default.
        endpoint: Option<EndpointId>,
    },
    /// A channel this client joined.
    Broadcast(ChannelId),
}

/// Why `send` committed nothing. The composer keeps the text (agreed
/// item 1a): no row exists for any of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// The envelope is over the decoded ceiling, or does not fit the
    /// payload limit even compressed.
    TooLarge,
    /// The envelope is not one this client may send.
    InvalidEnvelope,
    /// The store cannot hold the pending copy: storage is degraded.
    StorageUnavailable,
    /// A pending row with this application id already exists.
    AlreadyPending,
}

/// Why a row operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowError {
    /// No pending row with that id.
    NoSuchRow,
    /// The store could not record it.
    StorageUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Sending,
    Unconfirmed,
    NeedsAttention(SendProblem),
}

#[derive(Debug, Clone)]
struct Row {
    app_message_id: AppMessageId,
    attempts: u32,
    phase: Phase,
    next_at: Option<u64>,
    last_problem: Option<SendProblem>,
    last_code: Option<TransportError>,
    may_have_reached: bool,
}

/// The human client's transport facade.
pub struct TransportClient<B: DataSessionBinding, A: AdminBinding> {
    binding: B,
    admin_binding: A,
    store: HumanStore,
    config: ClientConfig,
    session: Option<B::Session>,
    admin: Option<A::Admin>,
    state: SessionState,
    reopen_attempt: u32,
    recheck_attempt: u32,
    next_recheck_at: u64,
    connectivity: Connectivity,
    admin_attempt: u32,
    next_status_at: u64,
    rows: BTreeMap<RowId, Row>,
    queue: EventQueue,
    diagnostics: Diagnostics,
}

impl<B: DataSessionBinding, A: AdminBinding> TransportClient<B, A> {
    /// A facade over `binding` and `admin_binding`, holding `store`.
    ///
    /// Opens nothing yet: the first [`tick`](Self::tick) does. Every row
    /// the store holds pending is reported once, due now (agreed item
    /// 1e), so a row that needed attention before a restart is attempted
    /// once more -- its peer's trust may have changed meanwhile.
    ///
    /// # Errors
    /// The store's, if its pending rows cannot be read.
    pub fn new(
        binding: B,
        admin_binding: A,
        store: HumanStore,
        config: ClientConfig,
        now: u64,
    ) -> Result<Self, StoreError> {
        let mut client = Self {
            binding,
            admin_binding,
            store,
            config,
            session: None,
            admin: None,
            state: SessionState::Reconnecting {
                attempt: 0,
                next_at: now,
            },
            reopen_attempt: 0,
            recheck_attempt: 0,
            next_recheck_at: now,
            connectivity: Connectivity::Unknown,
            admin_attempt: 0,
            next_status_at: now,
            rows: BTreeMap::new(),
            queue: EventQueue::default(),
            diagnostics: Diagnostics::default(),
        };
        for pending in client.all_pending()? {
            let row = Row {
                app_message_id: pending.app_message_id.clone(),
                attempts: pending.attempts,
                phase: Phase::Sending,
                next_at: Some(now),
                last_problem: None,
                last_code: None,
                // Attempted before this process: their outcomes are not
                // known here, so a cancel must not claim "not delivered".
                may_have_reached: pending.attempts > 0,
            };
            client.rows.insert(pending.row_id, row);
            client.report(pending.row_id);
        }
        let state = client.state.clone();
        client.queue.push(ClientEvent::Session(state));
        client
            .queue
            .push(ClientEvent::Connectivity(Connectivity::Unknown));
        Ok(client)
    }

    /// The session's state.
    #[must_use]
    pub const fn session_state(&self) -> &SessionState {
        &self.state
    }

    /// Connectivity as last read.
    #[must_use]
    pub const fn connectivity(&self) -> Connectivity {
        self.connectivity
    }

    /// The diagnostic counts.
    #[must_use]
    pub const fn diagnostics(&self) -> Diagnostics {
        self.diagnostics
    }

    /// The store, for the operations the facade does not own: read,
    /// keep and unkeep (agreed item 3).
    pub const fn store_mut(&mut self) -> &mut HumanStore {
        &mut self.store
    }

    /// The oldest pending event, coalesced per key.
    pub fn next_event(&mut self) -> Option<ClientEvent> {
        self.queue.pop()
    }

    /// Run what is due at `now`: re-check a degraded store, re-open a
    /// session that ended, read connectivity, and retry rows whose time
    /// has come.
    pub async fn tick(&mut self, now: u64) {
        // A store that went degraded outside a commit -- opened over its
        // quota, or failed a write the facade did not make -- is noticed
        // here, before a session is opened or kept on it.
        if self.store.health() == StorageHealth::Degraded
            && !matches!(
                self.state,
                SessionState::StorageDegraded | SessionState::Closed
            )
        {
            self.enter_degraded(now).await;
        }
        self.tick_storage(now);
        self.tick_session(now).await;
        self.tick_connectivity(now).await;
        if matches!(self.state, SessionState::Ready { .. }) {
            self.retry_due(now).await;
        }
    }

    /// Compose: commit the pending copy FIRST, then attempt it if the
    /// session is ready (`RETENTION.md` §2's order).
    ///
    /// # Errors
    /// See [`SendError`]: no row exists after any of them.
    pub async fn send(
        &mut self,
        destination: Destination,
        envelope: &HumanChatV2,
        now: u64,
    ) -> Result<RowId, SendError> {
        let encoded = encode_outbound(envelope, self.config.max_payload_bytes)
            .map_err(|_| SendError::TooLarge)?;
        let app_message_id = AppMessageId::parse(envelope.app_message_id.clone())
            .map_err(|_| SendError::InvalidEnvelope)?;
        let media_type =
            MediaType::parse(encoded.media_type).map_err(|_| SendError::InvalidEnvelope)?;
        let destination = match destination {
            Destination::Direct { peer, endpoint } => {
                OutboundDestination::Direct(interweave_transport_api::DirectDestination {
                    peer,
                    endpoint,
                })
            }
            Destination::Broadcast(channel) => OutboundDestination::Broadcast(channel),
        };
        let new = NewOutbound {
            app_message_id: app_message_id.clone(),
            // Minted once, here, and stored with the row: every retry --
            // after a restart too -- sends under it (HUMAN-CHAT.md
            // §Compression, schema v6).
            transport_message_id: MessageId::from_bytes(rand::random()),
            destination,
            media_type: Some(media_type),
            payload: encoded.bytes,
            created_at: now,
        };
        let row_id = match self.store.commit_pending_outbound(&new) {
            Ok(row_id) => row_id,
            Err(e) if e.is_duplicate() => return Err(SendError::AlreadyPending),
            Err(_) => {
                if self.store.health() == StorageHealth::Degraded {
                    self.enter_degraded(now).await;
                }
                return Err(SendError::StorageUnavailable);
            }
        };
        self.rows.insert(
            row_id,
            Row {
                app_message_id,
                attempts: 0,
                phase: Phase::Sending,
                next_at: Some(now),
                last_problem: None,
                last_code: None,
                may_have_reached: false,
            },
        );
        self.report(row_id);
        if matches!(self.state, SessionState::Ready { .. }) {
            let pending = self.pending_row(row_id);
            if let Some(pending) = pending {
                self.attempt(&pending, now).await;
            }
        }
        Ok(row_id)
    }

    /// Retry a row now, whatever its state: the person's "try again".
    ///
    /// # Errors
    /// [`RowError::NoSuchRow`] for a row that is not pending.
    pub async fn retry(&mut self, row_id: RowId, now: u64) -> Result<(), RowError> {
        let row = self.rows.get_mut(&row_id).ok_or(RowError::NoSuchRow)?;
        row.phase = Phase::Sending;
        row.next_at = Some(now);
        self.report(row_id);
        if matches!(self.state, SessionState::Ready { .. })
            && let Some(pending) = self.pending_row(row_id)
        {
            self.attempt(&pending, now).await;
        }
        Ok(())
    }

    /// Cancel a row: it is transport-terminal and its pending copy goes.
    ///
    /// # Errors
    /// [`RowError::NoSuchRow`] for a row that is not pending, or
    /// [`RowError::StorageUnavailable`] if the store could not remove it
    /// (the row stays pending).
    pub fn cancel(&mut self, row_id: RowId) -> Result<(), RowError> {
        let row = self.rows.get(&row_id).ok_or(RowError::NoSuchRow)?;
        let may_have_reached = row.may_have_reached;
        self.store
            .transport_terminal(row_id, TerminalCause::Cancelled)
            .map_err(|_| RowError::StorageUnavailable)?;
        self.finish(row_id, OutboundStatus::Cancelled { may_have_reached });
        Ok(())
    }

    /// Take up to `max` inbound messages: each is committed as unread
    /// BEFORE it is returned (`STATE.md`), so a message the caller shows
    /// is one the store holds.
    ///
    /// A message whose commit meets one already held is not returned (a
    /// duplicate is one message); one that cannot be decoded is
    /// discarded and counted, never stored (`HUMAN-CHAT.md` §Consumers);
    /// and if the store cannot commit, nothing more is returned and the
    /// session goes [`SessionState::StorageDegraded`].
    pub async fn drain(&mut self, max: usize, now: u64) -> Vec<Received> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        let events = match session.events(max).await {
            Ok(events) => events,
            Err(e) => {
                if ends_session(e) {
                    self.lose_session(now).await;
                }
                return Vec::new();
            }
        };
        let mut received = Vec::new();
        let mut lease_lost = false;
        let mut degraded = false;
        for event in events {
            if degraded {
                if !matches!(event, SessionEvent::Local(_)) {
                    self.diagnostics.dropped_unstored += 1;
                }
                continue;
            }
            match event {
                SessionEvent::Local(LocalSessionEvent::EndpointLeaseChanged { .. }) => {
                    lease_lost = true;
                }
                SessionEvent::Local(LocalSessionEvent::PeerDisconnected { peer, .. }) => {
                    self.queue.push(ClientEvent::PeerDisconnected { peer });
                }
                SessionEvent::Direct(direct) => {
                    let origin = Origin::Direct {
                        peer: direct.source_peer.clone(),
                        endpoint: direct.source_endpoint.clone(),
                    };
                    let stored = InboundOrigin {
                        peer: direct.source_peer,
                        endpoint: Some(direct.source_endpoint),
                        channel: None,
                    };
                    match self.commit(&direct.payload, origin, stored, now) {
                        Committed::Yes(r) => received.push(r),
                        Committed::Skipped => {}
                        Committed::StoreFailed => degraded = true,
                    }
                }
                SessionEvent::Broadcast(broadcast) => {
                    let origin = Origin::Channel {
                        channel: broadcast.channel.clone(),
                        publisher: broadcast.source_peer.clone(),
                    };
                    let stored = InboundOrigin {
                        peer: broadcast.source_peer,
                        endpoint: None,
                        channel: Some(broadcast.channel),
                    };
                    match self.commit(&broadcast.payload, origin, stored, now) {
                        Committed::Yes(r) => received.push(r),
                        Committed::Skipped => {}
                        Committed::StoreFailed => degraded = true,
                    }
                }
            }
        }
        if degraded {
            self.enter_degraded(now).await;
        } else if lease_lost {
            // The lease went; the facade re-claims it (agreed item 4b).
            self.lose_session(now).await;
        }
        received
    }

    /// Leave [`SessionState::Refused`]: the person's "try again". Re-open
    /// at the next tick.
    pub fn reopen(&mut self, now: u64) {
        if matches!(self.state, SessionState::Refused { .. }) {
            self.reopen_attempt = 0;
            self.set_state(SessionState::Reconnecting {
                attempt: 0,
                next_at: now,
            });
        }
    }

    /// Re-check a degraded store now, as well as on the tick's schedule.
    pub fn recheck(&mut self, now: u64) {
        if self.state == SessionState::StorageDegraded {
            self.next_recheck_at = now;
            self.tick_storage(now);
        }
    }

    /// End the session; the facade does nothing more until dropped.
    pub async fn close(&mut self) {
        if let Some(session) = self.session.take() {
            // A close that fails leaves nothing to undo: the binding ends
            // the session either way.
            let _ = session.close().await;
        }
        self.admin = None;
        self.set_state(SessionState::Closed);
    }

    // --- the tick's parts ------------------------------------------------

    fn tick_storage(&mut self, now: u64) {
        if self.state != SessionState::StorageDegraded || now < self.next_recheck_at {
            return;
        }
        match self.store.recheck_health() {
            Ok(StorageHealth::Healthy) => {
                self.recheck_attempt = 0;
                self.reopen_attempt = 0;
                self.set_state(SessionState::Reconnecting {
                    attempt: 0,
                    next_at: now,
                });
            }
            Ok(StorageHealth::Degraded) | Err(_) => {
                self.recheck_attempt = self.recheck_attempt.saturating_add(1);
                self.next_recheck_at = now + RECHECK.delay(self.recheck_attempt, 0);
            }
        }
    }

    async fn tick_session(&mut self, now: u64) {
        let SessionState::Reconnecting { next_at, .. } = self.state else {
            return;
        };
        if now < next_at {
            return;
        }
        match self.open().await {
            Ok(session) => {
                self.session = Some(session);
                self.reopen_attempt = 0;
                let endpoint = self.config.endpoint.clone();
                self.set_state(SessionState::Ready { endpoint });
            }
            Err(e) => match classify_open(e) {
                OpenFailure::Refused(problem) => {
                    self.set_state(SessionState::Refused { problem });
                }
                OpenFailure::Retry => {
                    self.reopen_attempt = self.reopen_attempt.saturating_add(1);
                    let attempt = self.reopen_attempt;
                    self.set_state(SessionState::Reconnecting {
                        attempt,
                        next_at: now + REOPEN.delay(attempt, 1),
                    });
                }
            },
        }
    }

    /// Open a session and make its joins; a join that fails closes it.
    async fn open(&self) -> Result<B::Session, TransportError> {
        let request = SessionRequest::new(
            self.config.client_kind.clone(),
            self.config.endpoint.clone(),
            [DataCapability::Commands, DataCapability::Events],
        )
        .map_err(|_| TransportError::InvalidArgument)?;
        let session = self.binding.open(request).await?;
        for channel in &self.config.channels {
            if let Err(e) = session.join(channel.clone()).await {
                let _ = session.close().await;
                return Err(e);
            }
        }
        Ok(session)
    }

    async fn tick_connectivity(&mut self, now: u64) {
        if now < self.next_status_at || self.state == SessionState::Closed {
            return;
        }
        if self.admin.is_none() {
            let Ok(admin) = self
                .admin_binding
                .admin(BTreeSet::from([AdminCapability::Status]))
                .await
            else {
                self.admin_failed(now);
                return;
            };
            self.admin = Some(admin);
        }
        let Some(admin) = self.admin.as_ref() else {
            return;
        };
        let Ok(status) = admin.status().await else {
            self.admin = None;
            self.admin_failed(now);
            return;
        };
        self.admin_attempt = 0;
        self.next_status_at = now + STATUS_INTERVAL_MS;
        let normalized = match status.health {
            Health::Unavailable => Connectivity::Offline,
            Health::Healthy | Health::Degraded => match (
                status.connectivity.direct_inbound,
                status.connectivity.relay_inbound,
            ) {
                (DirectInboundState::VerifiedPublic, _) => Connectivity::OnlineDirect,
                (_, PathReadiness::Ready) => Connectivity::OnlineRelay,
                _ => Connectivity::OnlinePartial,
            },
        };
        self.set_connectivity(normalized);
    }

    fn admin_failed(&mut self, now: u64) {
        self.admin_attempt = self.admin_attempt.saturating_add(1);
        self.next_status_at = now + REOPEN.delay(self.admin_attempt, 2);
        self.set_connectivity(Connectivity::Unknown);
    }

    async fn retry_due(&mut self, now: u64) {
        let is_due = |row: &Row| {
            !matches!(row.phase, Phase::NeedsAttention(_))
                && row.next_at.is_some_and(|at| at <= now)
        };
        if !self.rows.values().any(is_due) {
            return;
        }
        // One read of the pending rows per tick, not one per due row.
        let Ok(pending) = self.all_pending() else {
            return;
        };
        for row in pending {
            if !matches!(self.state, SessionState::Ready { .. }) {
                return;
            }
            if self.rows.get(&row.row_id).is_some_and(is_due) {
                self.attempt(&row, now).await;
            }
        }
    }

    // --- one attempt -------------------------------------------------------

    async fn attempt(&mut self, pending: &PendingOutbound, now: u64) {
        let row_id = pending.row_id;
        if self.store.record_attempt(row_id, now).is_err()
            && self.store.health() == StorageHealth::Degraded
        {
            self.enter_degraded(now).await;
            return;
        }
        if let Some(row) = self.rows.get_mut(&row_id) {
            row.attempts = row.attempts.saturating_add(1);
            row.next_at = None;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let Ok(payload) = Payload::new(
            pending.media_type.clone(),
            pending.payload.clone(),
            self.config.max_payload_bytes,
        ) else {
            self.failed(row_id, TransportError::PayloadTooLarge, now);
            return;
        };
        let outcome = match &pending.destination {
            OutboundDestination::Direct(destination) => session
                .send_direct(destination.clone(), pending.transport_message_id, payload)
                .await
                .map(|endpoint| (TerminalCause::Accepted, Some(endpoint))),
            OutboundDestination::Broadcast(channel) => session
                .broadcast(
                    channel.clone(),
                    BroadcastMessageV1 {
                        message_id: pending.transport_message_id,
                        sent_at_ms: pending.created_at,
                        payload,
                    },
                )
                .await
                .map(|()| (TerminalCause::Published, None)),
        };
        match outcome {
            Ok((cause, endpoint)) => {
                // Terminal whether or not the store can delete the copy
                // now: if it cannot, the row is sent again after a
                // restart, under the same transport id, and the
                // receiver's dedup absorbs it.
                if self.store.transport_terminal(row_id, cause).is_err()
                    && self.store.health() == StorageHealth::Degraded
                {
                    self.enter_degraded(now).await;
                }
                let status = endpoint.map_or(OutboundStatus::Published, |endpoint| {
                    OutboundStatus::Accepted { endpoint }
                });
                self.finish(row_id, status);
            }
            Err(e) => {
                self.failed(row_id, e, now);
                if ends_session(e) {
                    self.lose_session(now).await;
                }
            }
        }
    }

    fn failed(&mut self, row_id: RowId, error: TransportError, now: u64) {
        let Some(row) = self.rows.get_mut(&row_id) else {
            return;
        };
        row.last_code = Some(error);
        let key = u64::from_ne_bytes(row_id.get().to_ne_bytes());
        match classify_send(error) {
            AttemptFailure::Transient(problem) => {
                row.last_problem = Some(problem);
                if row.phase != Phase::Unconfirmed {
                    row.phase = Phase::Sending;
                }
                row.next_at = Some(now + SEND.delay(row.attempts, key));
            }
            AttemptFailure::Unconfirmed => {
                row.phase = Phase::Unconfirmed;
                row.may_have_reached = true;
                row.next_at = Some(now + SEND.delay(row.attempts, key));
            }
            AttemptFailure::NeedsAttention(problem) => {
                row.phase = Phase::NeedsAttention(problem);
                row.next_at = None;
            }
        }
        self.report(row_id);
    }

    fn finish(&mut self, row_id: RowId, status: OutboundStatus) {
        if let Some(row) = self.rows.remove(&row_id) {
            self.queue.push(ClientEvent::Outbound(OutboundUpdate {
                row: row_id,
                app_message_id: row.app_message_id,
                status,
                last_code: row.last_code,
            }));
        }
    }

    fn report(&mut self, row_id: RowId) {
        let Some(row) = self.rows.get(&row_id) else {
            return;
        };
        let status = match row.phase {
            Phase::Sending => OutboundStatus::Sending {
                attempts: row.attempts,
                next_retry_at: row.next_at,
                last_problem: row.last_problem,
            },
            Phase::Unconfirmed => OutboundStatus::Unconfirmed {
                next_retry_at: row.next_at,
                last_problem: row.last_problem,
            },
            Phase::NeedsAttention(problem) => OutboundStatus::NeedsAttention { problem },
        };
        self.queue.push(ClientEvent::Outbound(OutboundUpdate {
            row: row_id,
            app_message_id: row.app_message_id.clone(),
            status,
            last_code: row.last_code,
        }));
    }

    // --- inbound -----------------------------------------------------------

    fn commit(
        &mut self,
        payload: &Payload,
        origin: Origin,
        stored: InboundOrigin,
        now: u64,
    ) -> Committed {
        let Some(envelope) = self.decode(payload) else {
            return Committed::Skipped;
        };
        let Ok(app_message_id) = AppMessageId::parse(envelope.app_message_id.clone()) else {
            self.diagnostics.malformed_invalid_envelope += 1;
            return Committed::Skipped;
        };
        let new = NewInbound {
            app_message_id,
            origin: stored,
            media_type: payload.media_type().cloned(),
            payload: payload.bytes().to_vec(),
            received_at: now,
        };
        match self.store.commit_unread_inbound(&new) {
            Ok(row) => Committed::Yes(Received {
                row,
                origin,
                envelope,
                received_at: now,
            }),
            Err(e) if e.is_duplicate() => Committed::Skipped,
            Err(_) => {
                self.diagnostics.dropped_unstored += 1;
                Committed::StoreFailed
            }
        }
    }

    /// The envelope, or `None` having counted why (`HUMAN-CHAT.md`
    /// §Consumers: store nothing, present nothing, count it).
    fn decode(&mut self, payload: &Payload) -> Option<HumanChatV2> {
        let Some(info) = payload
            .media_type()
            .and_then(|m| parse_media_type(m.as_str()).ok())
        else {
            self.diagnostics.malformed_unknown_media_type += 1;
            return None;
        };
        let encoding: ContentEncoding = info.encoding;
        let Ok(text) = decode_envelope_bytes(payload.bytes(), encoding) else {
            self.diagnostics.malformed_undecodable += 1;
            return None;
        };
        let Ok(envelope) = HumanChatV2::parse(&text) else {
            self.diagnostics.malformed_invalid_envelope += 1;
            return None;
        };
        Some(envelope)
    }

    // --- session transitions -----------------------------------------------

    /// The session ended under us: re-open with backoff.
    async fn lose_session(&mut self, now: u64) {
        if let Some(session) = self.session.take() {
            let _ = session.close().await;
        }
        self.reopen_attempt = self.reopen_attempt.saturating_add(1);
        let attempt = self.reopen_attempt;
        self.set_state(SessionState::Reconnecting {
            attempt,
            next_at: now + REOPEN.delay(attempt, 1),
        });
    }

    /// The store cannot hold unread content: release the lease and the
    /// joins by closing the session, and re-check on a schedule
    /// (ADR-0044, `STATE.md` "Store health").
    async fn enter_degraded(&mut self, now: u64) {
        if let Some(session) = self.session.take() {
            let _ = session.close().await;
        }
        self.recheck_attempt = 0;
        self.next_recheck_at = now + RECHECK.delay(1, 0);
        self.set_state(SessionState::StorageDegraded);
    }

    fn set_state(&mut self, state: SessionState) {
        if self.state != state {
            self.state = state.clone();
            self.queue.push(ClientEvent::Session(state));
        }
    }

    fn set_connectivity(&mut self, connectivity: Connectivity) {
        if self.connectivity != connectivity {
            self.connectivity = connectivity;
            self.queue.push(ClientEvent::Connectivity(connectivity));
        }
    }

    // --- the store's pending rows -------------------------------------------

    fn all_pending(&self) -> Result<Vec<PendingOutbound>, StoreError> {
        let mut out = Vec::new();
        let mut after = None;
        loop {
            let page = self
                .store
                .pending_outbound_page(after, PageLimits::default())?;
            out.extend(page.items);
            match page.next {
                Some(next) => after = Some(next),
                None => return Ok(out),
            }
        }
    }

    fn pending_row(&self, row_id: RowId) -> Option<PendingOutbound> {
        self.all_pending()
            .ok()?
            .into_iter()
            .find(|p| p.row_id == row_id)
    }
}

enum Committed {
    Yes(Received),
    Skipped,
    StoreFailed,
}
