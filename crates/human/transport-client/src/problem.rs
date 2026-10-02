// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The only error vocabulary a client sees (the facade contract agreed
//! with rust-ui-dev, relay seqs 10522, 10534 and 10540;
//! `human-client-ui.md` §12): every [`TransportError`] maps onto a
//! [`SendProblem`] or a [`SessionProblem`] by an exhaustive match, so a
//! variant added to the transport fails to compile here rather than
//! reaching a view unclassified.

use interweave_transport_api::TransportError;

/// Why a send has not (yet) reached a terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SendProblem {
    /// The peer is not trusted for this profile, or not known to it.
    PeerUntrusted,
    /// The remote answered with the coarse no-route class: the selected
    /// route is currently unavailable. Kept coarse on purpose (ADR-0030).
    RouteUnavailable,
    /// No usable network path to the peer.
    NoNetworkPath,
    /// The remote or the local transport is temporarily busy.
    Busy,
    /// The local transport is unavailable or shutting down.
    ServiceUnavailable,
    /// The two sides do not speak a common protocol version.
    Incompatible,
    /// The message is over the transport's payload limit.
    TooLarge,
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

/// What one send attempt's failure means for its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptFailure {
    /// Retry on a timer.
    Transient(SendProblem),
    /// The remote MAY have accepted: retry under the same transport id,
    /// which the receiver's dedup makes safe (ADR-0019).
    Unconfirmed,
    /// Do not retry until the person acts.
    NeedsAttention(SendProblem),
}

/// Classify a send attempt's error.
pub(crate) const fn classify_send(error: TransportError) -> AttemptFailure {
    use AttemptFailure::{NeedsAttention, Transient, Unconfirmed};
    match error {
        TransportError::Timeout | TransportError::CancellationRaced => Unconfirmed,
        TransportError::PeerUnreachable => Transient(SendProblem::NoNetworkPath),
        TransportError::Overloaded => Transient(SendProblem::Busy),
        TransportError::RemoteEndpointUnavailable => Transient(SendProblem::RouteUnavailable),
        // The session's own state: its lease or a join went away, or its
        // runtime did. The facade re-opens; the send is retried after.
        TransportError::BackendUnavailable
        | TransportError::ShuttingDown
        | TransportError::EndpointNotRegistered
        | TransportError::ChannelNotJoined
        | TransportError::CancelledBeforeDispatch => Transient(SendProblem::ServiceUnavailable),
        TransportError::UnauthorizedPeer | TransportError::PeerUnknown => {
            NeedsAttention(SendProblem::PeerUntrusted)
        }
        TransportError::ProtocolUnsupported
        | TransportError::VersionIncompatible
        | TransportError::ProtocolViolation => NeedsAttention(SendProblem::Incompatible),
        TransportError::PayloadTooLarge => NeedsAttention(SendProblem::TooLarge),
        TransportError::InvalidArgument
        | TransportError::EndpointUnknown
        | TransportError::EndpointInUse
        | TransportError::EndpointDisabled
        | TransportError::EndpointClientKindDenied
        | TransportError::CapabilityDenied
        | TransportError::Internal => NeedsAttention(SendProblem::Internal),
    }
}

/// Whether a send's error says the SESSION is gone, so the facade must
/// re-open before anything else is sent on it.
pub(crate) const fn ends_session(error: TransportError) -> bool {
    matches!(
        error,
        TransportError::BackendUnavailable
            | TransportError::ShuttingDown
            | TransportError::EndpointNotRegistered
            | TransportError::ChannelNotJoined
    )
}

/// What an `open` failure means for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenFailure {
    /// Re-open on a timer: the runtime may come back.
    Retry,
    /// Do not re-open until the person asks: a timer would loop.
    Refused(SessionProblem),
}

/// Classify an `open` (or a join made while opening) failure.
pub(crate) const fn classify_open(error: TransportError) -> OpenFailure {
    use OpenFailure::{Refused, Retry};
    match error {
        TransportError::EndpointInUse => Refused(SessionProblem::EndpointInUse),
        TransportError::EndpointUnknown
        | TransportError::EndpointDisabled
        | TransportError::EndpointClientKindDenied
        | TransportError::CapabilityDenied
        | TransportError::UnauthorizedPeer => Refused(SessionProblem::NotAvailableToThisClient),
        TransportError::BackendUnavailable
        | TransportError::ShuttingDown
        | TransportError::Timeout
        | TransportError::Overloaded
        | TransportError::PeerUnreachable
        | TransportError::CancelledBeforeDispatch
        | TransportError::CancellationRaced => Retry,
        TransportError::InvalidArgument
        | TransportError::PayloadTooLarge
        | TransportError::ChannelNotJoined
        | TransportError::EndpointNotRegistered
        | TransportError::PeerUnknown
        | TransportError::RemoteEndpointUnavailable
        | TransportError::ProtocolUnsupported
        | TransportError::ProtocolViolation
        | TransportError::VersionIncompatible
        | TransportError::Internal => Refused(SessionProblem::Internal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant, so the tables below are checked against all of them.
    const ALL: [TransportError; 23] = [
        TransportError::InvalidArgument,
        TransportError::PayloadTooLarge,
        TransportError::ChannelNotJoined,
        TransportError::EndpointNotRegistered,
        TransportError::EndpointUnknown,
        TransportError::EndpointInUse,
        TransportError::EndpointDisabled,
        TransportError::EndpointClientKindDenied,
        TransportError::CapabilityDenied,
        TransportError::UnauthorizedPeer,
        TransportError::PeerUnknown,
        TransportError::PeerUnreachable,
        TransportError::RemoteEndpointUnavailable,
        TransportError::Timeout,
        TransportError::CancelledBeforeDispatch,
        TransportError::CancellationRaced,
        TransportError::Overloaded,
        TransportError::BackendUnavailable,
        TransportError::ProtocolUnsupported,
        TransportError::ProtocolViolation,
        TransportError::VersionIncompatible,
        TransportError::ShuttingDown,
        TransportError::Internal,
    ];

    /// An exhaustive match: a variant added to [`TransportError`] fails
    /// to compile here, and the test below then fails until [`ALL`]
    /// lists it.
    const fn index(error: TransportError) -> usize {
        match error {
            TransportError::InvalidArgument => 0,
            TransportError::PayloadTooLarge => 1,
            TransportError::ChannelNotJoined => 2,
            TransportError::EndpointNotRegistered => 3,
            TransportError::EndpointUnknown => 4,
            TransportError::EndpointInUse => 5,
            TransportError::EndpointDisabled => 6,
            TransportError::EndpointClientKindDenied => 7,
            TransportError::CapabilityDenied => 8,
            TransportError::UnauthorizedPeer => 9,
            TransportError::PeerUnknown => 10,
            TransportError::PeerUnreachable => 11,
            TransportError::RemoteEndpointUnavailable => 12,
            TransportError::Timeout => 13,
            TransportError::CancelledBeforeDispatch => 14,
            TransportError::CancellationRaced => 15,
            TransportError::Overloaded => 16,
            TransportError::BackendUnavailable => 17,
            TransportError::ProtocolUnsupported => 18,
            TransportError::ProtocolViolation => 19,
            TransportError::VersionIncompatible => 20,
            TransportError::ShuttingDown => 21,
            TransportError::Internal => 22,
        }
    }

    #[test]
    fn the_variant_list_is_every_variant_once() {
        let mut seen: Vec<usize> = ALL.into_iter().map(index).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..ALL.len()).collect::<Vec<_>>());
    }

    #[test]
    fn a_transient_failure_carries_a_transient_problem_and_attention_does_not() {
        for error in ALL {
            match classify_send(error) {
                AttemptFailure::Transient(p) => assert!(p.is_transient(), "{error:?}"),
                AttemptFailure::NeedsAttention(p) => assert!(!p.is_transient(), "{error:?}"),
                AttemptFailure::Unconfirmed => {}
            }
        }
    }

    #[test]
    fn only_timeout_and_a_raced_cancel_are_unconfirmed() {
        // Every other failure says the remote did NOT accept, which is
        // what lets `Cancelled { may_have_reached }` be false for them.
        let unconfirmed: Vec<_> = ALL
            .into_iter()
            .filter(|e| classify_send(*e) == AttemptFailure::Unconfirmed)
            .collect();
        assert_eq!(
            unconfirmed,
            [TransportError::Timeout, TransportError::CancellationRaced]
        );
    }

    #[test]
    fn the_section_12_examples_map_as_written() {
        assert_eq!(
            classify_send(TransportError::UnauthorizedPeer),
            AttemptFailure::NeedsAttention(SendProblem::PeerUntrusted)
        );
        assert_eq!(
            classify_send(TransportError::RemoteEndpointUnavailable),
            AttemptFailure::Transient(SendProblem::RouteUnavailable)
        );
        assert_eq!(
            classify_send(TransportError::PeerUnreachable),
            AttemptFailure::Transient(SendProblem::NoNetworkPath)
        );
        assert_eq!(
            classify_send(TransportError::Overloaded),
            AttemptFailure::Transient(SendProblem::Busy)
        );
        assert_eq!(
            classify_open(TransportError::EndpointInUse),
            OpenFailure::Refused(SessionProblem::EndpointInUse)
        );
    }

    #[test]
    fn every_lease_refusal_is_refused_and_never_retried_on_a_timer() {
        // Reconnecting must not loop on a refusal (agreed item 4a).
        for error in [
            TransportError::EndpointUnknown,
            TransportError::EndpointDisabled,
            TransportError::EndpointClientKindDenied,
            TransportError::EndpointInUse,
            TransportError::CapabilityDenied,
        ] {
            assert!(
                matches!(classify_open(error), OpenFailure::Refused(_)),
                "{error:?}"
            );
        }
    }

    #[test]
    fn an_error_that_ends_the_session_is_retried_not_held() {
        for error in ALL.into_iter().filter(|e| ends_session(*e)) {
            assert!(
                matches!(classify_send(error), AttemptFailure::Transient(_)),
                "{error:?}"
            );
        }
    }
}
