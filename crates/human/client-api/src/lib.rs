// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The human client's caller-facing vocabulary: what the transport facade
//! (`crates/human/transport-client`) says and what the UI model
//! (`crates/human/ui-model`) reads. Types only -- no I/O, no store, no
//! transport (plan §17 P2: it reaches no `rusqlite`, so the UI model can
//! name it). The contract is the facade's README, "The contract", agreed
//! with the client's role (relay seqs 10522, 10534, 10540, amended 10561,
//! 10567, 10570, A5 10582, 10585); architect-cto placed it here (10633).
//!
//! Nothing here claims more than the transport proved: `Accepted` is
//! bounded remote queue admission, `Published` is local publication, and
//! neither is read, seen or delivered (`human-client-ui.md` §5).

#![forbid(unsafe_code)]

use interweave_human_chat_protocol::HumanChatV2;
use interweave_human_core::{AppMessageId, RowId};
use interweave_transport_api::{
    ChannelId, EndpointId, PeerPath, TransportError, TransportIdentity,
};

/// Why a send has not (yet) reached a terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SendProblem {
    /// The peer is not trusted for this profile (`UnauthorizedPeer`).
    PeerUntrusted,
    /// The remote answered with the coarse no-route class: the selected
    /// route is currently unavailable. Kept coarse on purpose (ADR-0030).
    RouteUnavailable,
    /// No usable network path to the peer, or no address known for it yet.
    NoNetworkPath,
    /// The remote or the local transport is temporarily busy.
    Busy,
    /// The local transport is unavailable or shutting down, or this
    /// session lost its endpoint lease or a channel join: the facade
    /// re-opens and retries.
    ServiceUnavailable,
    /// The two sides do not speak a common protocol version.
    Incompatible,
    /// The message is over the transport's payload limit.
    TooLarge,
    /// The route or channel is no longer configured for this client: a
    /// row that survived a restart into a configuration that cannot send
    /// it (agreed amendment A2). The person can cancel it.
    NotConfigured,
    /// Anything else: a defect, carried with its raw code for diagnostics.
    Internal,
}

impl SendProblem {
    /// Whether the facade retries on its own after this problem.
    ///
    /// The four that can clear without anyone acting. The rest stay
    /// pending and durable as `NeedsAttention` until the person retries
    /// or cancels: retention has no "failed" terminal state.
    #[must_use]
    pub const fn is_transient(self) -> bool {
        matches!(
            self,
            Self::NoNetworkPath | Self::Busy | Self::ServiceUnavailable | Self::RouteUnavailable
        )
    }
}

/// Why a session could not be opened, when re-trying on a timer would
/// not help.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionProblem {
    /// The endpoint is already owned by another client or session
    /// (`human-client-ui.md` §12 names it).
    EndpointInUse,
    /// The endpoint is unknown, disabled, refuses this client kind, or
    /// the connection lacks a capability: not available to this client.
    NotAvailableToThisClient,
    /// Anything else.
    Internal,
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
    /// The envelope is not one a receiver would accept (`HumanChatV2`'s
    /// own validation).
    InvalidEnvelope,
    /// This client cannot send there: a broadcast to a channel it is not
    /// configured to join, or a direct send from a client with no
    /// endpoint (agreed amendment A2).
    NotConfigured,
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

/// Where one pending outbound row is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundStatus {
    /// The facade is sending it and retries on its own, and nothing that
    /// went out for it may have reached the remote.
    Sending {
        /// Attempts made so far.
        attempts: u32,
        /// When the next attempt is due, if one is scheduled.
        next_retry_at: Option<u64>,
        /// Why the last attempt did not finish it, if one failed.
        last_problem: Option<SendProblem>,
    },
    /// Retried on its own, and an earlier attempt MAY have reached the
    /// remote: one failed in a way that does not say the request never
    /// left (agreed amendment A1). Once a row is here it never returns to
    /// `Sending`. Retried under the same transport id, which the
    /// receiver's dedup makes safe. Show "not confirmed", never "failed".
    Unconfirmed {
        /// When the next attempt is due.
        next_retry_at: Option<u64>,
        /// The last problem the transport named, if any.
        last_problem: Option<SendProblem>,
    },
    /// The facade will not retry it on its own. It stays pending and
    /// durable until the person calls `retry` or `cancel`.
    NeedsAttention {
        /// Why.
        problem: SendProblem,
        /// Whether an earlier attempt may have reached the remote.
        may_have_reached: bool,
    },
    /// Terminal: the remote endpoint's bounded queue admitted it
    /// (`AcceptedV2`). Not read, not seen, not processed.
    Accepted {
        /// The endpoint that admitted it.
        endpoint: EndpointId,
    },
    /// Terminal: published locally. Not delivered to anyone in particular.
    Published,
    /// Terminal: the person cancelled it.
    Cancelled {
        /// Whether an earlier attempt may have reached the remote: one
        /// failed in a way that does not say the request never left, or
        /// the row was attempted before a restart. That last is derived
        /// as `attempts > 0` on load, and the store records an attempt
        /// BEFORE the transport call, so it errs only toward "may"
        /// (agreed amendment A1b).
        may_have_reached: bool,
    },
}

impl OutboundStatus {
    /// Whether transport will do no more with the row.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Accepted { .. } | Self::Published | Self::Cancelled { .. }
        )
    }
}

/// One row's status, as it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundUpdate {
    /// The row.
    pub row: RowId,
    /// Its application id.
    pub app_message_id: AppMessageId,
    /// Where it is now.
    pub status: OutboundStatus,
    /// The raw code of the last failure, for a diagnostics view only.
    pub last_code: Option<TransportError>,
}

/// The data session, as one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// Open, its lease (if it asked for one) held and its joins active.
    Ready {
        /// The endpoint it holds.
        endpoint: Option<EndpointId>,
    },
    /// The binding ended; the facade re-opens on its own.
    Reconnecting {
        /// Re-open attempts so far.
        attempt: u32,
        /// When the next is due.
        next_at: u64,
    },
    /// Opening was refused, and a timer would only repeat the refusal:
    /// only `reopen` leaves this state.
    Refused {
        /// Why.
        problem: SessionProblem,
    },
    /// The store cannot hold unread content: the lease is released and
    /// the joins suspended (ADR-0044) until a re-check finds it healthy.
    StorageDegraded,
    /// Closed by the caller.
    Closed,
}

/// The transport's reachability, normalized (`human-client-ui.md` §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    /// Online, with verified direct inbound reachability.
    OnlineDirect,
    /// Online, reachable inbound through a relay.
    OnlineRelay,
    /// Online, outbound or partial reachability only.
    OnlinePartial,
    /// The transport reports itself unavailable.
    Offline,
    /// Not known: no status read yet, or the status connection is down.
    /// Never shown as `Offline`.
    Unknown,
}

/// Who a received message came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A direct message.
    Direct {
        /// The authenticated sender.
        peer: TransportIdentity,
        /// The endpoint the sender ASSERTS it came from: a route label,
        /// never an identity.
        endpoint: EndpointId,
    },
    /// A broadcast.
    Channel {
        /// The channel it arrived on.
        channel: ChannelId,
        /// The authenticated publisher.
        publisher: TransportIdentity,
    },
}

/// A message committed as unread, ready to present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    /// Its unread row.
    pub row: RowId,
    /// Who sent it.
    pub origin: Origin,
    /// The parsed envelope. Its `sent_at_ms` is PEER-ASSERTED: shown as
    /// "sent", never an order (agreed amendment A3).
    pub envelope: HumanChatV2,
    /// When it was committed, on the caller's WALL clock in Unix ms: the
    /// order inbound is shown in.
    pub received_at: u64,
}

/// Something the caller should re-render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    /// A pending row's status changed.
    Outbound(OutboundUpdate),
    /// The session's state changed.
    Session(SessionState),
    /// Connectivity changed.
    Connectivity(Connectivity),
    /// A peer's connection ended.
    PeerDisconnected {
        /// The peer.
        peer: TransportIdentity,
    },
    /// The path to a peer this session has a route to changed: the route
    /// indicator's, never a reconnect or a message (`human-client-ui.md`
    /// §7). Its newest value only, per peer; one queued before the same
    /// peer's [`ClientEvent::PeerDisconnected`] is dropped by it.
    PeerPath {
        /// The peer.
        peer: TransportIdentity,
        /// The path now.
        path: PeerPath,
    },
    /// Messages were committed as unread but will never come out of
    /// `drain`: a session the facade closed held more than the hand-over
    /// queue's cap. They are in the store; re-list `unread_inbound` to
    /// show them (agreed amendment A5).
    UnreadInStore {
        /// How many, cumulative since the facade started.
        not_handed_over: u64,
    },
}

/// Counts of what was discarded, for a diagnostics view. Counts only:
/// no payload, no identifier (`observability.md`,
/// `human_inbound_malformed_total{reason}`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// Inbound with no human-chat media type, or one this build does not
    /// read.
    pub malformed_unknown_media_type: u64,
    /// Inbound whose bytes would not decode: a failed `ce` decode, not
    /// UTF-8, or past the decoded ceiling.
    pub malformed_undecodable: u64,
    /// Inbound that decoded and is not a valid envelope.
    pub malformed_invalid_envelope: u64,
    /// Inbound taken from the binding and lost because the store could
    /// not hold it: the bounded handoff window `STATE.md` names.
    pub dropped_unstored: u64,
    /// Reads of the pending rows that failed: retries were skipped that
    /// time, and are counted rather than stalling silently.
    pub pending_unreadable: u64,
    /// Committed inbound past the hand-over queue's cap: unread in the
    /// store, shown from there rather than from `drain`.
    pub held_overflow: u64,
}

/// The profile's trust allowlist as the daemon holds it now
/// (`human-client-ui.md` §8): every peer not listed is denied. A change
/// made in the settings lasts until it is changed again (ADR-0028 A
/// 2026-10-07: the daemon keeps it in its state); each row says where it
/// comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustList {
    /// This profile's own identity, never a peer to trust; `None` when
    /// the daemon reports none.
    pub local_peer: Option<TransportIdentity>,
    /// The allowed remote peers, in the daemon's order.
    pub allowed: Vec<TrustRow>,
}

impl TrustList {
    /// Whether `peer` is allowed.
    #[must_use]
    pub fn allows(&self, peer: &TransportIdentity) -> bool {
        self.allowed.iter().any(|row| &row.peer == peer)
    }

    /// The allowed peers, in the daemon's order.
    pub fn peers(&self) -> impl Iterator<Item = &TransportIdentity> {
        self.allowed.iter().map(|row| &row.peer)
    }
}

/// One allowed peer. Whether it survives a restart is not carried: over
/// IPC 2.3 `ipc-client` refuses a row that is not persisted, so every row
/// the desktop client reads is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustRow {
    /// The peer.
    pub peer: TransportIdentity,
    /// Where the row comes from.
    pub origin: TrustOrigin,
}

impl TrustRow {
    /// A row from the profile's configuration.
    #[must_use]
    pub const fn configured(peer: TransportIdentity) -> Self {
        Self {
            peer,
            origin: TrustOrigin::Configured,
        }
    }

    /// A row added in the settings.
    #[must_use]
    pub const fn added_here(peer: TransportIdentity) -> Self {
        Self {
            peer,
            origin: TrustOrigin::AddedHere,
        }
    }
}

/// Where an allowed peer comes from. A peer both configured and added is
/// `Configured`: the daemon drops the added entry when it loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustOrigin {
    /// The profile's configuration lists it.
    Configured,
    /// Added in the trust settings.
    AddedHere,
}

/// Why reading or changing trust failed. A class, for a settings view;
/// the raw code is not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustProblem {
    /// The daemon cannot be reached, or is stopping.
    Unavailable,
    /// This client may not administer trust on this daemon.
    NotPermitted,
    /// The daemon refused the change: this profile's own identity, or a
    /// new peer past the allowlist's ceiling.
    Refused,
    /// Anything else.
    Internal,
}

/// Why a trust change's answer is not the allowlist after it: whether the
/// change was made is what a person must be told truly, and the transport
/// says it only for some failures (`TRANSPORT.md` §Error model, dispatch
/// state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustSetFailure {
    /// Not made: refused, or never sent.
    NotMade(TrustProblem),
    /// May have been made: the request may have reached the daemon before
    /// the failure, which does not say whether it took effect.
    Unconfirmed(TrustProblem),
    /// Made, and the allowlist could not be read back after it.
    MadeNotReadBack(TrustProblem),
}
