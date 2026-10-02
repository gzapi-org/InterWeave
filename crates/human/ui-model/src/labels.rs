// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What a view may say: closed enums of stable keys, never English text,
//! so a translation is a table and a new label is a compile error in
//! every exhaustive match below (agreed items 5, 6).

use interweave_human_client_api::{OutboundStatus, SendError, SendProblem, SessionProblem};

/// One list makes the enum, [`LabelKey::ALL`] and the keys, so a label
/// cannot exist outside the list P6's test walks (review F4): there is no
/// second list to forget.
macro_rules! labels {
    ($($(#[$doc:meta])* $variant:ident => $key:literal,)+) => {
        /// Every status label a view shows: closed, and P6's test walks
        /// every one of them.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum LabelKey {
            $($(#[$doc])* $variant,)+
        }

        impl LabelKey {
            /// Every label, from the same list as the enum.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// The label's stable key, which a translation table maps to
            /// text.
            #[must_use]
            pub const fn key(self) -> &'static str {
                match self {
                    $(Self::$variant => $key,)+
                }
            }
        }
    };
}

labels! {
    /// Outbound, being sent.
    Sending => "status.sending",
    /// Outbound, the last attempt may have been received: "not
    /// confirmed", never "failed".
    NotConfirmed => "status.not_confirmed",
    /// Outbound, waiting for the person, nothing went out that may have
    /// reached the remote.
    NeedsAttention => "status.needs_attention",
    /// Outbound, waiting for the person, and an earlier attempt may
    /// already have been received.
    NeedsAttentionMayHaveBeenReceived => "status.needs_attention.may_have_been_received",
    /// Outbound, the remote transport's queue accepted it (`AcceptedV2`):
    /// not read, not seen, not processed.
    AcceptedByRemoteTransport => "status.accepted_by_remote_transport",
    /// Outbound, published by the local transport: not delivered to any
    /// recipient in particular.
    PublishedLocally => "status.published_locally",
    /// Outbound, cancelled; nothing that went out may have been received.
    Cancelled => "status.cancelled",
    /// Outbound, cancelled after an attempt that may have been received.
    CancelledMayHaveBeenReceived => "status.cancelled.may_have_been_received",
    /// Inbound, not yet read here.
    Unread => "status.unread",
    /// Inbound, read here and not kept: gone after a restart.
    ReadNotKept => "status.read_not_kept",
    /// Inbound, read here and kept.
    Kept => "status.kept",
}

impl LabelKey {
    /// Whether the label describes an outbound message's delivery: the
    /// labels P6 holds to "never read, seen or processed".
    #[must_use]
    pub const fn is_delivery(self) -> bool {
        !matches!(self, Self::Unread | Self::ReadNotKept | Self::Kept)
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
    fn every_label_appears_once_in_all() {
        let mut keys: Vec<&str> = LabelKey::ALL.iter().map(|l| l.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), LabelKey::ALL.len(), "no label listed twice");
    }

    /// P6: no delivery label reads as read, seen or processed. Checked on
    /// the stable key AND the variant's name, since either is what a
    /// translator starts from.
    #[test]
    fn no_delivery_label_reads_as_read_seen_or_processed() {
        for label in LabelKey::ALL.iter().copied().filter(|l| l.is_delivery()) {
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
