// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What the facade tells its caller: the agreed contract's types (relay
//! seqs 10522, 10534, 10540). Nothing here is libp2p-shaped, and no
//! state claims more than the transport proved: `Accepted` is bounded
//! remote queue admission, `Published` is local publication, and neither
//! is read, seen or delivered (`human-client-ui.md` §5).

use interweave_human_chat_protocol::HumanChatV2;
use interweave_human_store::{AppMessageId, RowId};
use interweave_transport_api::{ChannelId, EndpointId, TransportError, TransportIdentity};

use crate::problem::{SendProblem, SessionProblem};

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
}
