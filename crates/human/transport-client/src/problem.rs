// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Every [`TransportError`] mapped onto the client's vocabulary
//! (`interweave-human-client-api`'s `SendProblem` and `SessionProblem`,
//! `human-client-ui.md` §12) by an exhaustive match, so a variant added
//! to the transport fails to compile here rather than reaching a view
//! unclassified.

use interweave_human_client_api::{SendProblem, SessionProblem, TrustProblem};
use interweave_transport_api::TransportError;

/// What one send attempt's failure means for its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptFailure {
    /// Retry on a timer, with the problem if the error names one.
    Retry(Option<SendProblem>),
    /// Do not retry until the person acts.
    NeedsAttention(SendProblem),
}

/// Classify a send attempt's error.
pub(crate) const fn classify_send(error: TransportError) -> AttemptFailure {
    use AttemptFailure::{NeedsAttention, Retry};
    match error {
        TransportError::Timeout | TransportError::CancellationRaced => Retry(None),
        // `PeerUnknown` is no address known yet for an authorized target
        // (`TRANSPORT.md` §send_direct) -- a daemon just started, before
        // its static routes seed the address book -- so it is a missing
        // path, retried, never the person's to act on as trust is.
        TransportError::PeerUnreachable | TransportError::PeerUnknown => {
            Retry(Some(SendProblem::NoNetworkPath))
        }
        TransportError::Overloaded => Retry(Some(SendProblem::Busy)),
        TransportError::RemoteEndpointUnavailable => Retry(Some(SendProblem::RouteUnavailable)),
        // The session's runtime went, or the session lost its lease or a
        // join (the facade checks the configuration BEFORE the transport,
        // so from the transport these mean the session, never the
        // configuration): it re-opens and the send is retried after.
        // `NotConfigured` comes from that configuration check alone.
        TransportError::BackendUnavailable
        | TransportError::ShuttingDown
        | TransportError::EndpointNotRegistered
        | TransportError::ChannelNotJoined
        | TransportError::CancelledBeforeDispatch => Retry(Some(SendProblem::ServiceUnavailable)),
        TransportError::UnauthorizedPeer => NeedsAttention(SendProblem::PeerUntrusted),
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

/// Whether, after this error, the remote MAY have accepted the message:
/// `TRANSPORT.md` §Error model's "outcome unknown" class (dispatch state,
/// A 2026-10-02), which places each fieldless category in the most
/// conservative class it can occur in. The bindings return these after a
/// request may already have left: a deadline, a raced cancel, a
/// connection that ended with the call pending, an exchange that timed
/// out or closed after the request was written, a response that did not
/// parse, or an internal failure. Every other category is "not
/// dispatched" or "dispatched and refused": the remote did not take it.
pub(crate) const fn may_have_reached(error: TransportError) -> bool {
    matches!(
        error,
        TransportError::Timeout
            | TransportError::CancellationRaced
            | TransportError::BackendUnavailable
            | TransportError::ShuttingDown
            | TransportError::PeerUnreachable
            | TransportError::ProtocolViolation
            | TransportError::Internal
    )
}

/// Whether a send's error says the SESSION is gone or lost its lease or a
/// join, so the facade must re-open before anything else is sent on it.
/// The re-open commits what the session holds before closing it, so
/// nothing already accepted is lost to it.
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

/// Classify a trust read's or change's failure (`human-client-ui.md` §8).
/// `InvalidArgument` is the port's refusal of this profile's own identity
/// or of a new peer past the allowlist's ceiling (`LOCAL-CLIENT.md` §7
/// item 11), a refusal the person can act on. A daemon that does not speak
/// the version trust needs -- one negotiating IPC below 2.3, whose trust
/// read `ipc-client` refuses `ProtocolUnsupported` -- is `Incompatible`,
/// as a send's is: the person is told the cause, which no retry mends.
pub(crate) const fn classify_trust(error: TransportError) -> TrustProblem {
    match error {
        TransportError::BackendUnavailable
        | TransportError::ShuttingDown
        | TransportError::Timeout
        | TransportError::Overloaded
        | TransportError::CancelledBeforeDispatch
        | TransportError::CancellationRaced => TrustProblem::Unavailable,
        TransportError::CapabilityDenied => TrustProblem::NotPermitted,
        TransportError::InvalidArgument => TrustProblem::Refused,
        TransportError::ProtocolUnsupported
        | TransportError::VersionIncompatible
        | TransportError::ProtocolViolation => TrustProblem::Incompatible,
        TransportError::PayloadTooLarge
        | TransportError::ChannelNotJoined
        | TransportError::EndpointNotRegistered
        | TransportError::EndpointUnknown
        | TransportError::EndpointInUse
        | TransportError::EndpointDisabled
        | TransportError::EndpointClientKindDenied
        | TransportError::UnauthorizedPeer
        | TransportError::PeerUnknown
        | TransportError::PeerUnreachable
        | TransportError::RemoteEndpointUnavailable
        | TransportError::Internal => TrustProblem::Internal,
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
    fn a_retried_failure_carries_a_transient_problem_and_attention_does_not() {
        for error in ALL {
            match classify_send(error) {
                AttemptFailure::Retry(Some(p)) => assert!(p.is_transient(), "{error:?}"),
                AttemptFailure::Retry(None) => {}
                AttemptFailure::NeedsAttention(p) => assert!(!p.is_transient(), "{error:?}"),
            }
        }
    }

    #[test]
    /// `TRANSPORT.md` §Error model, "outcome unknown", exactly.
    fn exactly_the_post_dispatch_errors_may_have_reached() {
        let maybe: Vec<_> = ALL.into_iter().filter(|e| may_have_reached(*e)).collect();
        assert_eq!(
            maybe,
            [
                TransportError::PeerUnreachable,
                TransportError::Timeout,
                TransportError::CancellationRaced,
                TransportError::BackendUnavailable,
                TransportError::ProtocolViolation,
                TransportError::ShuttingDown,
                TransportError::Internal,
            ]
        );
    }

    #[test]
    fn a_lost_lease_or_join_is_retried_after_a_reopen_and_never_called_not_configured() {
        for error in [
            TransportError::ChannelNotJoined,
            TransportError::EndpointNotRegistered,
        ] {
            assert_eq!(
                classify_send(error),
                AttemptFailure::Retry(Some(SendProblem::ServiceUnavailable))
            );
            assert!(ends_session(error), "{error:?}");
        }
        for error in ALL {
            assert_ne!(
                classify_send(error),
                AttemptFailure::NeedsAttention(SendProblem::NotConfigured),
                "{error:?}: NotConfigured is the configuration check's alone"
            );
        }
    }

    #[test]
    fn a_trust_failure_is_unavailable_not_permitted_refused_incompatible_or_internal() {
        assert_eq!(
            classify_trust(TransportError::BackendUnavailable),
            TrustProblem::Unavailable
        );
        assert_eq!(
            classify_trust(TransportError::CapabilityDenied),
            TrustProblem::NotPermitted
        );
        assert_eq!(
            classify_trust(TransportError::InvalidArgument),
            TrustProblem::Refused
        );
        for error in ALL {
            // Only the port's own refusal reads as one the person caused.
            assert_eq!(
                classify_trust(error) == TrustProblem::Refused,
                error == TransportError::InvalidArgument,
                "{error:?}"
            );
            // A version problem reads as one wherever a send's does.
            assert_eq!(
                classify_trust(error) == TrustProblem::Incompatible,
                matches!(
                    classify_send(error),
                    AttemptFailure::NeedsAttention(SendProblem::Incompatible)
                ),
                "{error:?}"
            );
        }
    }

    #[test]
    fn a_peer_with_no_address_yet_is_a_missing_path_retried_not_an_untrusted_one() {
        assert_eq!(
            classify_send(TransportError::PeerUnknown),
            AttemptFailure::Retry(Some(SendProblem::NoNetworkPath))
        );
        assert!(
            !may_have_reached(TransportError::PeerUnknown),
            "not dispatched (TRANSPORT.md, dispatch state)"
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
            AttemptFailure::Retry(Some(SendProblem::RouteUnavailable))
        );
        assert_eq!(
            classify_send(TransportError::PeerUnreachable),
            AttemptFailure::Retry(Some(SendProblem::NoNetworkPath))
        );
        assert_eq!(
            classify_send(TransportError::Overloaded),
            AttemptFailure::Retry(Some(SendProblem::Busy))
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
                matches!(classify_send(error), AttemptFailure::Retry(_)),
                "{error:?}"
            );
        }
    }
}
