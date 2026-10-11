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
    LocalSessionEvent, MAX_CLIENT_KIND_CHARS, MAX_GRANTED_CAPABILITIES, SessionError,
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
    route_notices: bool,
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
        let chars = client_kind.chars().count();
        if chars == 0 || chars > MAX_CLIENT_KIND_CHARS {
            return Err(SessionError::InvalidClientKind { got: chars });
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
            route_notices: true,
        })
    }

    /// The same request for a session that takes no `PeerPathChanged`
    /// without a `previous` -- no route-begin and no return notice
    /// (`LOCAL-CLIENT.md` item 12, A 2026-10-09). A binding opens such a
    /// session for a client that cannot read that shape, the IPC server
    /// for a connection below minor 2.4: owed none, it is owed every path
    /// change as it was before them, `previous` included, rather than a
    /// change merged into a notice the binding then has to drop.
    #[must_use]
    pub const fn without_route_notices(mut self) -> Self {
        self.route_notices = false;
        self
    }

    /// Whether the session takes route-begin and return notices (true
    /// unless [`Self::without_route_notices`] said otherwise).
    #[must_use]
    pub const fn route_notices(&self) -> bool {
        self.route_notices
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
    /// A session-scoped notice (lease revoked, the runtime's state, peer
    /// disconnected).
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
    /// At most `max` are taken, in that order, and the rest stay with the
    /// binding for the next call, still under their queues' bounds: a
    /// caller with room for `max` never holds more than it can pass on,
    /// so none is taken and then lost (architect-cto's ruling, relay seq
    /// 9709). `max == 0` takes nothing: on a live session it returns an
    /// empty list, and it answers the end as soon as the session has
    /// ended, whatever remains buffered; a positive `max` delivers the
    /// buffered events first, then the end (architect-cto's ruling
    /// 01a11be2; the conformance suite's
    /// `an_ended_session_delivers_what_waited_then_its_end`).
    ///
    /// # Errors
    /// `CapabilityDenied` without `events`, or `BackendUnavailable`.
    fn events(
        &self,
        max: usize,
    ) -> impl Future<Output = Result<Vec<SessionEvent>, TransportError>> + Send;

    /// Wait until something is owed to this session, without taking it:
    /// resolves once at least one event waits for [`events`](Self::events),
    /// or once the session has ended (the next `events` then answers how).
    /// A caller loops `ready` then `events` and never polls.
    ///
    /// Taking nothing, it moves nothing out of the queues' bounds and
    /// adds no delivery guarantee (`LOCAL-CLIENT.md`, A 2026-10-03; §7
    /// item 9).
    ///
    /// # Errors
    /// `CapabilityDenied` without `events`.
    fn ready(&self) -> impl Future<Output = Result<(), TransportError>> + Send;

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
    /// `BackendUnavailable` once the runtime has stopped. A binding over a
    /// connection (IPC) answers with the code the connection ended with
    /// when it had already ended, and `Timeout` when the release was not
    /// confirmed in time.
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
    /// own session records: BINDING-LOCAL (`LOCAL-CLIENT.md`, A
    /// 2026-09-30). The in-process binding knows it; over IPC it never
    /// crosses the wire and is `None`. Across bindings a grant is named by
    /// its `epoch`. A string, not a [`Generation`]: a generation's bounds
    /// are for values that cross a wire opaque to the other side, and this
    /// one never does (architect-cto, relay seq 9772).
    pub session_id: Option<String>,
}

/// One configured endpoint and its runtime state
/// (`ipc/endpoint-list.schema.json`'s row).
///
/// `enabled` and `default` are the effective state: `config.yaml` with
/// the endpoint overlay composed over it (ADR-0028 A 2026-10-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointAdminView {
    /// The endpoint.
    pub endpoint: EndpointId,
    /// Whether it accepts traffic and may be claimed.
    pub enabled: bool,
    /// Whether it receives directed sends that name no endpoint.
    pub default: bool,
    /// The row's `enabled` and `default` survive a restart: what an
    /// administrator set is kept by the endpoint overlay. False only
    /// where the runtime keeps no overlay, which no production binding
    /// is (a test construction), and on a row read over IPC below 2.5,
    /// whose row says nothing else.
    pub persisted: bool,
    /// Its live lease, if one is held.
    pub lease: Option<LeaseRecord>,
}

/// The profile's peer trust policy as it stands (`ipc/trust-list`): the
/// allowed remote peers and this profile's own identity.
///
/// Deny-by-default is the policy's shape, not a setting, so there is no
/// default here to report; and no per-peer decision, which is a local
/// diagnostic (ADR-0032). What `admin.trust.set` changed is kept in the
/// state directory's trust overlay and survives a restart (ADR-0028 A
/// 2026-10-07); each row says where it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustAdminView {
    /// This profile's identity, which is never a remote to trust; `None`
    /// for a policy that was never bound to one.
    pub local_peer: Option<TransportIdentity>,
    /// The allowlisted remote peers, in order; every other peer is denied.
    pub allowed: Vec<TrustedPeer>,
}

impl TrustAdminView {
    /// The allowlisted peers' identities, in the rows' order.
    #[must_use]
    pub fn peers(&self) -> impl ExactSizeIterator<Item = &TransportIdentity> {
        self.allowed.iter().map(|row| &row.peer)
    }
}

/// One allowlisted peer (`ipc/trust-list`'s row): the peer, whether the
/// row survives a restart, and where it comes from. Ordered by peer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TrustedPeer {
    /// The allowed peer.
    pub peer: TransportIdentity,
    /// The row survives a restart: configured rows by `config.yaml`,
    /// administered rows by the trust overlay. False only where the
    /// runtime keeps no overlay, which no production binding is (a test
    /// construction, `LOCAL-CLIENT.md` §7 item 11).
    pub persisted: bool,
    /// Where the row comes from.
    pub source: TrustSource,
}

/// Where an allowlisted peer comes from (ADR-0028 A 2026-10-07).
///
/// A peer is never both: an administered peer that `config.yaml` comes
/// to list is dropped from the overlay when it is next loaded, so it
/// reports `Configured` -- and revoking it adds it to the overlay's
/// revocations rather than removing an administered entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrustSource {
    /// `config.yaml`'s `trust.allowed_peers`, not revoked.
    Configured,
    /// Added over `admin.trust.set` and kept in the trust overlay.
    Administered,
}

/// How the last dial to, or connection with, a peer ended
/// (`CONNECTIVITY.md` §19's `last_outcome`): a bounded class, never an
/// address. Every refusal folds to `Denied`; the connectivity log line
/// names the finer class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerOutcome {
    /// A connection to it came up.
    Connected,
    /// A dial to it failed in the network.
    DialFailed,
    /// An address answered with another identity.
    IdentityMismatch,
    /// The dial gate, or this node's own handler, refused the dial.
    Denied,
}

impl PeerOutcome {
    /// The class as the wire names it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::DialFailed => "dial_failed",
            Self::IdentityMismatch => "identity_mismatch",
            Self::Denied => "denied",
        }
    }
}

/// The dial gate's state for one allowlisted peer (`admin.peers.list`,
/// `CONNECTIVITY.md` §19): what the operator reads to answer "why can B
/// not reach A". Times are milliseconds since the Unix epoch. Never an
/// address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerGateView {
    /// The peer.
    pub peer: TransportIdentity,
    /// Whether any connection to it is open.
    pub connected: bool,
    /// Dials to it are refused until then.
    pub backoff_until: Option<u64>,
    /// Every known address of it is quarantined until then, the earliest
    /// release; absent while any is dialable (`CONNECTIVITY.md` §19).
    pub quarantined_until: Option<u64>,
    /// How the last dial or connection ended; `None` until the first one
    /// since the runtime started.
    pub last_outcome: Option<PeerOutcome>,
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
    /// The pre-authentication funnel's counters, when the binding's
    /// runtime has one to read.
    pub pre_auth: Option<PreAuthCounts>,
    /// The post-authentication ingress limiters' state, when the
    /// binding's runtime has them to read.
    pub ingress: Option<IngressCounts>,
}

/// The pre-authentication funnel at one instant (`resource-limits.md`
/// §pre-auth): what an unauthenticated party is holding of it. Counts
/// only -- never a source, which is a remote address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreAuthCounts {
    /// Sources with pre-authentication state accounted.
    pub tracked_sources: usize,
    /// Handshakes in flight.
    pub pending: usize,
}

/// The post-authentication ingress rate limiters at one instant
/// (`resource-limits.md`): how many authenticated peers each lane's
/// limiter holds state for. Counts only -- never a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressCounts {
    /// Peers the direct lane's limiter tracks.
    pub direct_tracked_peers: usize,
    /// Peers the broadcast lane's limiter tracks.
    pub broadcast_tracked_peers: usize,
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
/// No mutation is written to the profile. A trust change is kept in the
/// state directory's trust overlay (ADR-0028 A 2026-10-07), and an
/// endpoint's enabled state and the default in its endpoint overlay
/// (ADR-0028 A 2026-10-11); both survive a restart. Revoking a lease
/// is a session's matter and does not.
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

    /// The profile's peer trust policy.
    ///
    /// # Errors
    /// `CapabilityDenied` without `admin.trust`, or `BackendUnavailable`.
    fn trust(&self) -> impl Future<Output = Result<TrustAdminView, TransportError>> + Send;

    /// The dial gate's state for each allowlisted peer, one row each
    /// (`admin.peers.list`, IPC 2.2, under `admin.status`).
    ///
    /// Answered `ProtocolUnsupported` by a port that does not serve the
    /// 2.2 read -- which is what the method is for a daemon that does
    /// not speak it -- unless the port implements it.
    ///
    /// # Errors
    /// `CapabilityDenied` without `admin.status`, `ProtocolUnsupported`,
    /// or `BackendUnavailable`.
    fn peers(&self) -> impl Future<Output = Result<Vec<PeerGateView>, TransportError>> + Send {
        std::future::ready(Err(TransportError::ProtocolUnsupported))
    }

    /// Allow `peer` on the data plane, or revoke it. Revoking closes every
    /// connection the peer holds at once, drops its cached endpoint
    /// directory, and each session holding `events` is told
    /// `PeerDisconnected` with the `policy` reason (ADR-0012). Allowing a
    /// listed peer or revoking an unlisted one changes nothing and
    /// succeeds.
    ///
    /// # Errors
    /// `CapabilityDenied` without `admin.trust`; `InvalidArgument` for
    /// this profile's own identity, for a new peer once the allowlist
    /// holds its ceiling (4096), or -- while the trust overlay is ahead of
    /// the runtime -- for a new peer that would put the overlay past that
    /// ceiling at the next start; `Internal` when the trust overlay could
    /// not be written (nothing changed) or was left ahead of the runtime
    /// (it takes effect at the next start); or `BackendUnavailable`.
    fn set_trust(
        &self,
        peer: TransportIdentity,
        allowed: bool,
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
    use crate::{DataCapability, MAX_CLIENT_KIND_CHARS, SessionError};

    #[test]
    fn a_request_enforces_the_session_bounds_before_anything_is_claimed() {
        assert!(matches!(
            SessionRequest::new("", None, []),
            Err(SessionError::InvalidClientKind { got: 0 })
        ));
        assert!(matches!(
            SessionRequest::new("k".repeat(MAX_CLIENT_KIND_CHARS + 1), None, []),
            Err(SessionError::InvalidClientKind { .. })
        ));
        // Characters, not bytes: 64 two-byte characters (128 bytes) is
        // at the bound and admitted; one more is not.
        assert!(SessionRequest::new("é".repeat(MAX_CLIENT_KIND_CHARS), None, []).is_ok());
        assert!(matches!(
            SessionRequest::new("é".repeat(MAX_CLIENT_KIND_CHARS + 1), None, []),
            Err(SessionError::InvalidClientKind { got: 65 })
        ));
        let ok = SessionRequest::new("human", None, [DataCapability::Events]).expect("in bounds");
        assert_eq!(ok.client_kind(), "human");
        assert!(ok.endpoint().is_none());
    }
}
