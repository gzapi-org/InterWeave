// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The binding surface (`contracts/LOCAL-CLIENT.md` §2, §5, §7): what a
//! platform binding offers a local application and its local control
//! code, in neutral types.
//!
//! One surface for every binding. The in-process one (Stage 12, the
//! Android embedded adapter's core) and the desktop IPC one (Stage 13)
//! implement the same traits -- two for the data plane, two for
//! administration (plan §16 (2)) -- and `tests/local-client-conformance`
//! runs one suite against each, which is what LOCAL-CLIENT.md §7 means by
//! "shared conformance tests".
//!
//! No backend type crosses: the received events are this crate's, and a
//! direct send names a destination and never a source -- the source is
//! the session's lease (§2: "No application API accepts a caller-supplied
//! source endpoint"), so the trait has no parameter to supply one.

use std::collections::BTreeSet;
use std::future::Future;
use std::time::Duration;

use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, ConnectivitySummary, DirectDestination, EndpointDirectoryV1,
    EndpointId, Health, MessageId, Payload, TransportError, TransportIdentity,
};

use crate::{
    AdminCapability, DataCapability, Generation, LocalAdminPort, LocalDataSession,
    LocalSessionEvent, MAX_CLIENT_KIND_BYTES, MAX_GRANTED_CAPABILITIES, SessionError,
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
    /// `EndpointInUse`), or `BackendUnavailable` once the runtime has stopped.
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
    /// `CapabilityDenied` without `commands`, or `BackendUnavailable`.
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

    /// Take what waits for this session: the session notices first, then
    /// the direct messages, then the broadcasts, each group oldest first.
    /// Grouped, not interleaved: the three come from separate bounded
    /// queues. A direct message and a broadcast carry their own receipt
    /// time for a caller that wants one order; a session notice carries
    /// none, and precedes both because it changes how they are read.
    ///
    /// # Errors
    /// `CapabilityDenied` without `events`, or `BackendUnavailable`.
    fn events(&self) -> impl Future<Output = Result<Vec<SessionEvent>, TransportError>> + Send;

    /// Ask `peer` which endpoints it advertises to this profile
    /// (`endpoints.query`). Advisory and peer-asserted (ADR-0031): a
    /// listed endpoint may still answer `no_route`, and a send needs no
    /// prior query. `ttl_ms` is what remains of the clamped freshness,
    /// counted from this answer.
    ///
    /// # Errors
    /// `CapabilityDenied` without `endpoints.query`, the query's own
    /// refusal, or `BackendUnavailable`.
    fn query_endpoints(
        &self,
        peer: TransportIdentity,
    ) -> impl Future<Output = Result<EndpointDirectoryV1, TransportError>> + Send;

    /// End the session: its lease is released at once and its joins
    /// dropped (§3: "session closure/revocation releases the lease
    /// immediately").
    ///
    /// # Errors
    /// `BackendUnavailable` once the runtime has stopped.
    fn close(self) -> impl Future<Output = Result<(), TransportError>> + Send;
}

/// One live lease, as administration sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRecord {
    /// The leased endpoint.
    pub endpoint: EndpointId,
    /// The lease's epoch: the value a revocation notice names.
    pub epoch: Generation,
    /// The label the holder claimed under: hygiene, never authority
    /// (ADR-0037).
    pub client_kind: String,
    /// The holding session's opaque id, for correlating with the binding's
    /// own session records.
    pub session_id: String,
}

/// One configured endpoint and its runtime state
/// (`ipc/endpoint-list.schema.json`'s row).
///
/// Every row is a runtime overlay over the profile: an administrative
/// change is lost on restart, so nothing here says `persisted` -- the
/// answer is always no, and the IPC mirror writes that constant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointAdminView {
    /// The endpoint.
    pub endpoint: EndpointId,
    /// Whether it accepts traffic and may be claimed.
    pub enabled: bool,
    /// Whether it receives directed sends that name no endpoint.
    pub default: bool,
    /// Its live lease, if one is held.
    pub lease: Option<LeaseRecord>,
}

/// The read-only administrative view (`admin.status`): the raw detail a
/// data session never sees (ADR-0036). A binding adds its own counters
/// -- connections, cross-domain refusals -- beside these, since only it
/// can count them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminStatus {
    /// The runtime's aggregate health.
    pub health: Health,
    /// This profile's identity.
    pub peer: TransportIdentity,
    /// The full connectivity summary.
    pub connectivity: ConnectivitySummary,
    /// Endpoints currently leased.
    pub active_leases: usize,
}

/// A platform binding's administrative side: opens admin ports.
///
/// A separate trait from [`DataSessionBinding`], and a port is never built
/// from a session (`LOCAL-CLIENT.md` §5): the two authorities meet only in
/// the local control code that holds both bindings.
pub trait AdminBinding {
    /// The port this binding opens.
    type Admin: AdminPort;

    /// Open an admin port holding exactly `capabilities`. It never holds
    /// an endpoint lease.
    ///
    /// # Errors
    /// `CapabilityDenied` if the binding refuses a capability, or
    /// `BackendUnavailable` once the runtime has stopped.
    fn admin(
        &self,
        capabilities: BTreeSet<AdminCapability>,
    ) -> impl Future<Output = Result<Self::Admin, TransportError>> + Send;
}

/// One open administrative port.
///
/// Every mutation is a runtime overlay, never written to the profile:
/// a restart returns to the configured state.
pub trait AdminPort {
    /// The port's identity and authorities.
    fn port(&self) -> &LocalAdminPort;

    /// The administrative status.
    ///
    /// # Errors
    /// `CapabilityDenied` without `admin.status`, or `BackendUnavailable`.
    fn status(&self) -> impl Future<Output = Result<AdminStatus, TransportError>> + Send;

    /// Every configured endpoint, in id order, with its live lease.
    ///
    /// # Errors
    /// `CapabilityDenied` without `admin.endpoints`, or `BackendUnavailable`.
    fn leases(&self)
    -> impl Future<Output = Result<Vec<EndpointAdminView>, TransportError>> + Send;

    /// End `endpoint`'s lease: its holder is told the epoch that ended,
    /// and what waited on its queue is discarded. Idempotent.
    ///
    /// # Errors
    /// `CapabilityDenied` without `admin.endpoints`, or `BackendUnavailable`.
    fn revoke_endpoint(
        &self,
        endpoint: EndpointId,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Enable or disable `endpoint`. Disabling revokes a live lease at once
    /// -- the holder told, as by [`AdminPort::revoke_endpoint`] -- and
    /// never rebinds it: the next claim is a client's own. Disabling the
    /// endpoint that receives omitted destinations clears that default,
    /// and enabling it again restores nothing. Returns the epoch disabling
    /// revoked, if one was live.
    ///
    /// # Errors
    /// `CapabilityDenied` without `admin.endpoints`, `EndpointUnknown` for
    /// an endpoint the profile does not configure, or `BackendUnavailable`.
    fn set_endpoint_enabled(
        &self,
        endpoint: EndpointId,
        enabled: bool,
    ) -> impl Future<Output = Result<Option<Generation>, TransportError>> + Send;

    /// Set the endpoint that receives directed sends naming none, or
    /// clear it with `None`.
    ///
    /// # Errors
    /// `CapabilityDenied` without `admin.endpoints`, `EndpointUnknown` or
    /// `EndpointDisabled` for an endpoint that could not receive, or
    /// `BackendUnavailable`.
    fn set_default_endpoint(
        &self,
        endpoint: Option<EndpointId>,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Ask the runtime's owner to shut down within `grace`. A REQUEST: the
    /// port does not own the runtime, and the owner -- the composition
    /// root -- stops it.
    ///
    /// # Errors
    /// `CapabilityDenied` without `admin.shutdown`, or `BackendUnavailable`.
    fn shutdown(&self, grace: Duration) -> impl Future<Output = Result<(), TransportError>> + Send;
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
