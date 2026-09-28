// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The data-plane binding surface (`contracts/LOCAL-CLIENT.md` §2, §7):
//! what a platform binding offers a local application, in neutral types.
//!
//! One surface for every binding. The in-process one (Stage 12, the
//! Android embedded adapter's core) and the desktop IPC one (Stage 13)
//! implement the same two traits, and `tests/local-client-conformance`
//! runs one suite against each, which is what LOCAL-CLIENT.md §7 means by
//! "shared conformance tests".
//!
//! No backend type crosses: the received events are this crate's, and a
//! direct send names a destination and never a source -- the source is
//! the session's lease (§2: "No application API accepts a caller-supplied
//! source endpoint"), so the trait has no parameter to supply one.

use std::collections::BTreeSet;
use std::future::Future;

use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointId, MessageId, Payload,
    TransportError, TransportIdentity,
};

use crate::{
    DataCapability, LocalDataSession, LocalSessionEvent, MAX_CLIENT_KIND_BYTES,
    MAX_GRANTED_CAPABILITIES, SessionError,
};

/// What a local application asks for when it opens a session.
///
/// The runtime decides the rest -- the session id, the lease's epoch, the
/// event queue's bound -- so none of those is a field here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRequest {
    client_kind: String,
    endpoint: Option<EndpointId>,
    capabilities: BTreeSet<DataCapability>,
}

impl SessionRequest {
    /// A request for a session, direct-capable when `endpoint` names the
    /// configured endpoint to lease.
    ///
    /// # Errors
    /// [`SessionError`] for an out-of-range client kind or more than
    /// [`MAX_GRANTED_CAPABILITIES`] -- the bounds [`LocalDataSession::new`]
    /// enforces, refused here before anything is claimed.
    pub fn new(
        client_kind: impl Into<String>,
        endpoint: Option<EndpointId>,
        capabilities: impl IntoIterator<Item = DataCapability>,
    ) -> Result<Self, SessionError> {
        let client_kind = client_kind.into();
        if client_kind.is_empty() || client_kind.len() > MAX_CLIENT_KIND_BYTES {
            return Err(SessionError::InvalidClientKind {
                got: client_kind.len(),
            });
        }
        let capabilities: BTreeSet<_> = capabilities.into_iter().collect();
        if capabilities.len() > MAX_GRANTED_CAPABILITIES {
            return Err(SessionError::TooManyCapabilities {
                got: capabilities.len(),
            });
        }
        Ok(Self {
            client_kind,
            endpoint,
            capabilities,
        })
    }

    /// The local label, a hygiene label and never authority.
    #[must_use]
    pub fn client_kind(&self) -> &str {
        &self.client_kind
    }

    /// The configured endpoint to lease, if any.
    #[must_use]
    pub const fn endpoint(&self) -> Option<&EndpointId> {
        self.endpoint.as_ref()
    }

    /// The capabilities asked for.
    #[must_use]
    pub const fn capabilities(&self) -> &BTreeSet<DataCapability> {
        &self.capabilities
    }
}

/// A direct message admitted to this session's endpoint queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedDirect {
    /// The authenticated remote peer: Noise proved it.
    pub source_peer: TransportIdentity,
    /// The endpoint the remote says produced it. PEER-ASSERTED metadata,
    /// never authorization (ADR-0030).
    pub source_endpoint: EndpointId,
    /// The local endpoint it resolved to.
    pub destination_endpoint: EndpointId,
    /// The sender's idempotency key.
    pub message_id: MessageId,
    /// The application bytes and their advisory media type.
    pub payload: Payload,
    /// Local receipt time, taken at admission, in Unix-epoch milliseconds.
    pub received_at_ms: u64,
}

/// A broadcast admitted to this session's queue on a joined channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedBroadcast {
    /// The authenticated original publisher: the signature proved it.
    pub source_peer: TransportIdentity,
    /// The channel it arrived on, from the topic rather than the envelope.
    pub channel: ChannelId,
    /// The publisher's application identity for this message.
    pub message_id: MessageId,
    /// The application bytes and their advisory media type.
    pub payload: Payload,
    /// Local admission time, in Unix-epoch milliseconds.
    pub received_at_ms: u64,
}

/// One event delivered to exactly one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// A direct message for this session's leased endpoint.
    Direct(ReceivedDirect),
    /// A broadcast on a channel this session joined.
    Broadcast(ReceivedBroadcast),
    /// A session-scoped notice (lease revoked, peer disconnected).
    Local(LocalSessionEvent),
}

/// A platform binding: opens data-plane sessions.
pub trait DataSessionBinding {
    /// The session this binding opens.
    type Session: DataSessionPort;

    /// Open a session, claiming the requested endpoint's lease if one is
    /// named. A refused claim opens nothing.
    ///
    /// # Errors
    /// The lease refusal mapped as `ENDPOINTS.md` says
    /// (`EndpointUnknown`, `EndpointDisabled`, `EndpointClientKindDenied`,
    /// `EndpointInUse`), or `Stopped` once the runtime has stopped.
    fn open(
        &self,
        request: SessionRequest,
    ) -> impl Future<Output = Result<Self::Session, TransportError>> + Send;
}

/// One open data-plane session.
///
/// There is no method that yields administrative authority, and no
/// parameter through which a caller could name a source endpoint.
pub trait DataSessionPort {
    /// The session's immutable creation context.
    fn session(&self) -> &LocalDataSession;

    /// Take this session's join reference on `channel`.
    ///
    /// # Errors
    /// `CapabilityDenied` without `commands`, or the runtime's refusal.
    fn join(&self, channel: ChannelId) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Release this session's join reference on `channel`; idempotent.
    ///
    /// # Errors
    /// `CapabilityDenied` without `commands`, or `Stopped`.
    fn leave(&self, channel: ChannelId) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Publish on a channel this session joined. Success is local
    /// acceptance only (`PUBSUB.md`).
    ///
    /// # Errors
    /// `CapabilityDenied`, `ChannelNotJoined`, or the runtime's refusal.
    fn broadcast(
        &self,
        channel: ChannelId,
        message: BroadcastMessageV1,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Send one direct message from this session's lease. Returns the
    /// remote endpoint that accepted it (`AcceptedV2`: bounded remote
    /// queue admission, not processing).
    ///
    /// # Errors
    /// `EndpointNotRegistered` without a lease, `CapabilityDenied`
    /// without `commands`, or the exchange's own outcome.
    fn send_direct(
        &self,
        destination: DirectDestination,
        message_id: MessageId,
        payload: Payload,
    ) -> impl Future<Output = Result<EndpointId, TransportError>> + Send;

    /// Take what waits for this session, oldest first.
    ///
    /// # Errors
    /// `CapabilityDenied` without `events`, or `Stopped`.
    fn events(&self) -> impl Future<Output = Result<Vec<SessionEvent>, TransportError>> + Send;

    /// End the session: its lease is released at once and its joins
    /// dropped (§3: "session closure/revocation releases the lease
    /// immediately").
    ///
    /// # Errors
    /// `Stopped` once the runtime has stopped.
    fn close(self) -> impl Future<Output = Result<(), TransportError>> + Send;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::SessionRequest;
    use crate::{DataCapability, MAX_CLIENT_KIND_BYTES, SessionError};

    #[test]
    fn a_request_enforces_the_session_bounds_before_anything_is_claimed() {
        assert!(matches!(
            SessionRequest::new("", None, []),
            Err(SessionError::InvalidClientKind { got: 0 })
        ));
        assert!(matches!(
            SessionRequest::new("k".repeat(MAX_CLIENT_KIND_BYTES + 1), None, []),
            Err(SessionError::InvalidClientKind { .. })
        ));
        let ok = SessionRequest::new("human", None, [DataCapability::Events]).expect("in bounds");
        assert_eq!(ok.client_kind(), "human");
        assert!(ok.endpoint().is_none());
    }
}
