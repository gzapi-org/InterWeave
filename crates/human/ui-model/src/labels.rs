// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What a view may say: closed enums of stable keys, never English text,
//! so a translation is a table and a new label is a compile error in
//! every exhaustive match below (agreed items 5, 6).

use interweave_human_client_api::{OutboundStatus, SendError, SendProblem, SessionProblem};

/// Every status label a view shows. Closed: P6's test enumerates this
/// enum, not a list beside it, so a new label cannot skip the test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LabelKey {
    /// Outbound, being sent.
    Sending,
    /// Outbound, the last attempt may have been received: "not
    /// confirmed", never "failed".
    NotConfirmed,
    /// Outbound, waiting for the person, nothing went out that may have
    /// reached the remote.
    NeedsAttention,
    /// Outbound, waiting for the person, and an earlier attempt may
    /// already have been received.
    NeedsAttentionMayHaveBeenReceived,
    /// Outbound, the remote transport's queue accepted it (`AcceptedV2`):
    /// not read, not seen, not processed.
    AcceptedByRemoteTransport,
    /// Outbound, published by the local transport: not delivered to any
    /// recipient in particular.
    PublishedLocally,
    /// Outbound, cancelled; nothing that went out may have been received.
    Cancelled,
    /// Outbound, cancelled after an attempt that may have been received.
    CancelledMayHaveBeenReceived,
    /// Inbound, not yet read here.
    Unread,
    /// Inbound, read here and not kept: gone after a restart.
    ReadNotKept,
    /// Inbound, read here and kept.
    Kept,
}

impl LabelKey {
    /// Every label, by an exhaustive match: a variant added above fails
    /// to compile in [`LabelKey::index`] until it is listed here too.
    pub const ALL: [Self; 11] = [
        Self::Sending,
        Self::NotConfirmed,
        Self::NeedsAttention,
        Self::NeedsAttentionMayHaveBeenReceived,
        Self::AcceptedByRemoteTransport,
        Self::PublishedLocally,
        Self::Cancelled,
        Self::CancelledMayHaveBeenReceived,
        Self::Unread,
        Self::ReadNotKept,
        Self::Kept,
    ];

    /// The label's place in [`LabelKey::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Sending => 0,
            Self::NotConfirmed => 1,
            Self::NeedsAttention => 2,
            Self::NeedsAttentionMayHaveBeenReceived => 3,
            Self::AcceptedByRemoteTransport => 4,
            Self::PublishedLocally => 5,
            Self::Cancelled => 6,
            Self::CancelledMayHaveBeenReceived => 7,
            Self::Unread => 8,
            Self::ReadNotKept => 9,
            Self::Kept => 10,
        }
    }

    /// Whether the label describes an outbound message's delivery: the
    /// labels P6 holds to "never read, seen or processed".
    #[must_use]
    pub const fn is_delivery(self) -> bool {
        !matches!(self, Self::Unread | Self::ReadNotKept | Self::Kept)
    }

    /// The label's stable key, which a translation table maps to text.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Sending => "status.sending",
            Self::NotConfirmed => "status.not_confirmed",
            Self::NeedsAttention => "status.needs_attention",
            Self::NeedsAttentionMayHaveBeenReceived => {
                "status.needs_attention.may_have_been_received"
            }
            Self::AcceptedByRemoteTransport => "status.accepted_by_remote_transport",
            Self::PublishedLocally => "status.published_locally",
            Self::Cancelled => "status.cancelled",
            Self::CancelledMayHaveBeenReceived => "status.cancelled.may_have_been_received",
            Self::Unread => "status.unread",
            Self::ReadNotKept => "status.read_not_kept",
            Self::Kept => "status.kept",
        }
    }
}

/// The label for an outbound status.
#[must_use]
pub const fn outbound_label(status: &OutboundStatus) -> LabelKey {
    match status {
        OutboundStatus::Sending { .. } => LabelKey::Sending,
        OutboundStatus::Unconfirmed { .. } => LabelKey::NotConfirmed,
        OutboundStatus::NeedsAttention {
            may_have_reached: false,
            ..
        } => LabelKey::NeedsAttention,
        OutboundStatus::NeedsAttention {
            may_have_reached: true,
            ..
        } => LabelKey::NeedsAttentionMayHaveBeenReceived,
        OutboundStatus::Accepted { .. } => LabelKey::AcceptedByRemoteTransport,
        OutboundStatus::Published => LabelKey::PublishedLocally,
        OutboundStatus::Cancelled {
            may_have_reached: false,
        } => LabelKey::Cancelled,
        OutboundStatus::Cancelled {
            may_have_reached: true,
        } => LabelKey::CancelledMayHaveBeenReceived,
    }
}

/// A user-actionable error class: one stable key per message
/// `human-client-ui.md` §12 asks for. The raw code stays in diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorClass {
    /// The peer is not trusted for this profile.
    PeerNotTrusted,
    /// The selected route is currently unavailable.
    RouteUnavailable,
    /// No usable network path.
    NoNetworkPath,
    /// The remote or local transport is temporarily busy.
    Busy,
    /// The local transport is unavailable.
    TransportUnavailable,
    /// The two sides do not speak a common protocol version.
    Incompatible,
    /// The message is too large to send.
    TooLarge,
    /// This route or channel is not configured for this client.
    NotConfigured,
    /// The message is not one a receiver would accept.
    InvalidMessage,
    /// Local storage cannot hold it.
    StorageUnavailable,
    /// The same message is already pending.
    AlreadyPending,
    /// This local endpoint is already owned by another client or session.
    EndpointInUse,
    /// The endpoint is not available to this client.
    EndpointNotAvailable,
    /// Anything else; the raw code is in diagnostics.
    Internal,
}

/// The class of a send problem a row reports.
#[must_use]
pub const fn send_problem_class(problem: SendProblem) -> ErrorClass {
    match problem {
        SendProblem::PeerUntrusted => ErrorClass::PeerNotTrusted,
        SendProblem::RouteUnavailable => ErrorClass::RouteUnavailable,
        SendProblem::NoNetworkPath => ErrorClass::NoNetworkPath,
        SendProblem::Busy => ErrorClass::Busy,
        SendProblem::ServiceUnavailable => ErrorClass::TransportUnavailable,
        SendProblem::Incompatible => ErrorClass::Incompatible,
        SendProblem::TooLarge => ErrorClass::TooLarge,
        SendProblem::NotConfigured => ErrorClass::NotConfigured,
        SendProblem::Internal => ErrorClass::Internal,
    }
}

/// The class of a send the facade refused with no row.
#[must_use]
pub const fn send_error_class(error: &SendError) -> ErrorClass {
    match error {
        SendError::TooLarge => ErrorClass::TooLarge,
        SendError::InvalidEnvelope => ErrorClass::InvalidMessage,
        SendError::NotConfigured => ErrorClass::NotConfigured,
        SendError::StorageUnavailable => ErrorClass::StorageUnavailable,
        SendError::AlreadyPending => ErrorClass::AlreadyPending,
    }
}

/// The class of a session refusal.
#[must_use]
pub const fn session_problem_class(problem: SessionProblem) -> ErrorClass {
    match problem {
        SessionProblem::EndpointInUse => ErrorClass::EndpointInUse,
        SessionProblem::NotAvailableToThisClient => ErrorClass::EndpointNotAvailable,
        SessionProblem::Internal => ErrorClass::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_list_is_every_label_once() {
        let mut seen: Vec<usize> = LabelKey::ALL.into_iter().map(LabelKey::index).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..LabelKey::ALL.len()).collect::<Vec<_>>());
    }

    /// P6: no delivery label reads as read, seen or processed. Checked on
    /// the stable key AND the variant's name, since either is what a
    /// translator starts from.
    #[test]
    fn no_delivery_label_reads_as_read_seen_or_processed() {
        for label in LabelKey::ALL.into_iter().filter(|l| l.is_delivery()) {
            let words = format!("{label:?} {}", label.key()).to_ascii_lowercase();
            for forbidden in ["read", "seen", "processed", "delivered"] {
                assert!(!words.contains(forbidden), "{label:?}: {forbidden}");
            }
        }
    }

    #[test]
    fn may_have_reached_gets_its_own_labels() {
        assert_ne!(
            outbound_label(&OutboundStatus::Cancelled {
                may_have_reached: true
            }),
            outbound_label(&OutboundStatus::Cancelled {
                may_have_reached: false
            })
        );
        assert_ne!(
            outbound_label(&OutboundStatus::NeedsAttention {
                problem: SendProblem::Busy,
                may_have_reached: true
            }),
            outbound_label(&OutboundStatus::NeedsAttention {
                problem: SendProblem::Busy,
                may_have_reached: false
            })
        );
    }

    #[test]
    fn endpoint_in_use_keeps_its_own_class() {
        assert_eq!(
            session_problem_class(SessionProblem::EndpointInUse),
            ErrorClass::EndpointInUse
        );
        assert_ne!(
            session_problem_class(SessionProblem::NotAvailableToThisClient),
            ErrorClass::EndpointInUse
        );
    }
}
