// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The local session boundary, identical on desktop and Android.
//!
//! `contracts/LOCAL-CLIENT.md` exists because the semantic boundary between
//! a local application and the transport runtime must be the same whether
//! it is serialized over a socket or called in-process. So nothing here
//! knows about sockets, processes, JNI, or UI — a type that did would make
//! the two bindings different contracts wearing one name.
//!
//! # Two authority rules, made structural
//!
//! **A data session cannot reach administrative authority.** [`LocalDataSession`]
//! has no method, field, or capability that yields a [`LocalAdminPort`], and
//! `admin.*` capabilities are not representable in a data session's granted
//! set — [`DataCapability`] simply does not contain them. This is not a
//! runtime check that could be forgotten; it is a type that cannot express
//! the wrong thing (ADR-0037).
//!
//! **Source endpoint comes from the lease, never from a caller.** There is
//! no constructor, setter, or parameter anywhere in this crate that accepts
//! a caller-supplied source endpoint. [`LocalDataSession::source_endpoint`]
//! reads it from the lease, which the runtime granted. ADR-0030 puts the
//! non-spoofable source here, and an API that accepted one "for testing"
//! would be the hole.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use interweave_transport_api::{EndpointId, TransportError, TransportIdentity};
use serde::{Deserialize, Serialize};

/// Maximum length of a `client_kind` label.
pub const MAX_CLIENT_KIND_BYTES: usize = 64;
/// Maximum capabilities granted to one session.
pub const MAX_GRANTED_CAPABILITIES: usize = 8;
/// Default bound on a session's event queue.
pub const DEFAULT_EVENT_QUEUE: usize = 256;
/// Hard architectural ceiling on one session's event queue.
///
/// `resource-limits.md` gives the `LocalDataSession event queue` row as
/// 256 default and 1024 ceiling. The ceiling is what this type enforces:
/// a narrower deployment value is configuration, and this is the bound
/// below which the design stops being bounded. Without it the DEFAULT
/// was the only number written down, and a caller could ask for any
/// depth at all.
pub const MAX_EVENT_QUEUE: usize = 1_024;

/// An authority a **data-plane** session may hold.
///
/// The `admin.*` capabilities of the IPC vocabulary are deliberately absent.
/// They are not omitted for brevity: a data session that could name one
/// could be granted one, and ADR-0037 requires that authority to be
/// unreachable from this side of the boundary rather than merely unlikely.
/// See [`AdminCapability`], a separate type reachable only through
/// [`LocalAdminPort`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataCapability {
    /// Receive normalized transport events.
    Events,
    /// Issue ordinary transport commands.
    Commands,
    /// Query the remote endpoint directory, when the profile enables it.
    #[serde(rename = "endpoints.query")]
    EndpointsQuery,
}

/// An authority reachable only through the administrative port.
///
/// A separate enum from [`DataCapability`], so the two cannot be mixed in
/// one collection or passed to one another's APIs. Merging them into a
/// single vocabulary — even a closed one — is how `client_kind` starts
/// looking like it could grant administration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminCapability {
    /// Configure endpoints, including revoking leases.
    #[serde(rename = "admin.endpoints")]
    Endpoints,
    /// Shut the runtime down.
    #[serde(rename = "admin.shutdown")]
    Shutdown,
}

/// An opaque generation value for a session or lease.
///
/// Not a bearer credential. It exists so stale local route and reply state
/// can be invalidated after a reconnect, and it is deliberately not pinned
/// to one encoding: the value never crosses the network, so no two
/// implementations must agree on its bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Generation(String);

impl Generation {
    /// Minimum length in bytes.
    pub const MIN_BYTES: usize = 16;
    /// Maximum length in bytes.
    pub const MAX_BYTES: usize = 64;

    /// Wrap an opaque generation value.
    ///
    /// # Errors
    /// Returns [`SessionError::InvalidGeneration`] outside 16..=64 bytes or
    /// containing a byte outside `[A-Za-z0-9_-]`.
    pub fn parse(value: impl Into<String>) -> Result<Self, SessionError> {
        let value = value.into();
        let ok = (Self::MIN_BYTES..=Self::MAX_BYTES).contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
        if ok {
            Ok(Self(value))
        } else {
            Err(SessionError::InvalidGeneration)
        }
    }

    /// The value as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Generation {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(s).map_err(serde::de::Error::custom)
    }
}

/// Why a session or lease could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// A generation value was outside its bound or alphabet.
    InvalidGeneration,
    /// The `client_kind` label was empty or too long.
    InvalidClientKind {
        /// Bytes supplied.
        got: usize,
    },
    /// More capabilities than a session may hold.
    TooManyCapabilities {
        /// Capabilities supplied.
        got: usize,
    },
    /// The event queue bound was zero.
    ///
    /// A zero-length queue cannot admit anything, so every direct message
    /// to this session would be refused — a configuration that reads like
    /// "unbounded" and behaves like "closed".
    ZeroEventQueue,
    /// An event queue past the architectural ceiling.
    ///
    /// The other half of the bound. Refusing zero and accepting any
    /// depth at all leaves the memory a session can pin unbounded, which
    /// is the same defect the zero check exists to name.
    EventQueueTooDeep {
        /// The depth asked for.
        got: usize,
    },
}

impl core::fmt::Display for SessionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidGeneration => write!(
                f,
                "generation must be {}..={} bytes of [A-Za-z0-9_-]",
                Generation::MIN_BYTES,
                Generation::MAX_BYTES
            ),
            Self::InvalidClientKind { got } => {
                write!(
                    f,
                    "client_kind is {got} bytes; the limit is 1..={MAX_CLIENT_KIND_BYTES}"
                )
            }
            Self::TooManyCapabilities { got } => {
                write!(
                    f,
                    "{got} capabilities exceeds the cap of {MAX_GRANTED_CAPABILITIES}"
                )
            }
            Self::ZeroEventQueue => write!(f, "an event queue of zero admits nothing"),
            Self::EventQueueTooDeep { got } => {
                write!(
                    f,
                    "an event queue of {got} exceeds the ceiling of {MAX_EVENT_QUEUE}"
                )
            }
        }
    }
}

impl core::error::Error for SessionError {}

/// Exclusive ownership of one configured endpoint.
///
/// Created only by the runtime granting it. The epoch changes on every
/// grant, so a reply token minted under a previous lease can be recognised
/// as stale after a reconnect rather than silently routing to the new owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointLease {
    /// The endpoint this session exclusively owns.
    pub endpoint: EndpointId,
    /// Fresh for every grant, across reconnects and daemon restarts.
    pub epoch: Generation,
}

/// A local application's data-plane session.
///
/// Immutable creation context. There is no setter for the lease, the
/// capabilities, or the client kind: a session's authority is decided when
/// the runtime creates it, and a mutable field here would let a client
/// widen its own grant after admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalDataSession {
    session_id: Generation,
    client_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint_lease: Option<EndpointLease>,
    capabilities: BTreeSet<DataCapability>,
    event_queue: usize,
}

/// The JSON shape, deserialized through the validating constructor.
#[derive(Deserialize)]
struct LocalDataSessionRepr {
    session_id: Generation,
    client_kind: String,
    #[serde(default)]
    endpoint_lease: Option<EndpointLease>,
    #[serde(default)]
    capabilities: BTreeSet<DataCapability>,
    event_queue: usize,
}

impl<'de> Deserialize<'de> for LocalDataSession {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Routed through `new` so the invariants hold on the serialized
        // boundary too. A derived impl would build the struct field by
        // field, accepting `event_queue: 0` or an empty `client_kind` —
        // states the constructor exists to make unrepresentable, arriving
        // by the one path that skips it.
        let raw = LocalDataSessionRepr::deserialize(d)?;
        Self::new(
            raw.session_id,
            raw.client_kind,
            raw.endpoint_lease,
            raw.capabilities,
            raw.event_queue,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl LocalDataSession {
    /// Build a session from the context the runtime decided.
    ///
    /// # Errors
    /// Returns [`SessionError`] for an out-of-range client kind, more than
    /// [`MAX_GRANTED_CAPABILITIES`], or a zero-length event queue.
    pub fn new(
        session_id: Generation,
        client_kind: impl Into<String>,
        endpoint_lease: Option<EndpointLease>,
        capabilities: impl IntoIterator<Item = DataCapability>,
        event_queue: usize,
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
        if event_queue == 0 {
            return Err(SessionError::ZeroEventQueue);
        }
        if event_queue > MAX_EVENT_QUEUE {
            return Err(SessionError::EventQueueTooDeep { got: event_queue });
        }
        Ok(Self {
            session_id,
            client_kind,
            endpoint_lease,
            capabilities,
            event_queue,
        })
    }

    /// This session's opaque identifier.
    #[must_use]
    pub const fn session_id(&self) -> &Generation {
        &self.session_id
    }

    /// The local label the client presented.
    ///
    /// A hygiene label only. `client_kind` is not authentication and never
    /// creates authority — a session claiming `admin` is still a data
    /// session, because authority lives in [`Self::capabilities`] and
    /// `admin.*` is not representable there.
    #[must_use]
    pub fn client_kind(&self) -> &str {
        &self.client_kind
    }

    /// The endpoint lease, if this session is direct-capable.
    #[must_use]
    pub const fn endpoint_lease(&self) -> Option<&EndpointLease> {
        self.endpoint_lease.as_ref()
    }

    /// The source endpoint for every direct send from this session.
    ///
    /// Derived from the lease, never from a caller. This crate offers no
    /// way to supply one: ADR-0030 puts the non-spoofable source here, and
    /// an override "for testing" would be the hole it is guarding.
    #[must_use]
    pub fn source_endpoint(&self) -> Option<&EndpointId> {
        self.endpoint_lease.as_ref().map(|l| &l.endpoint)
    }

    /// The granted authorities.
    #[must_use]
    pub const fn capabilities(&self) -> &BTreeSet<DataCapability> {
        &self.capabilities
    }

    /// Whether this session holds a given data capability.
    #[must_use]
    pub fn holds(&self, capability: DataCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// The bound on this session's event queue.
    #[must_use]
    pub const fn event_queue(&self) -> usize {
        self.event_queue
    }

    /// Whether a direct send may proceed, and why not when it may not.
    ///
    /// A session with no lease cannot send at all rather than sending as
    /// nobody: `EndpointNotRegistered` is the contract's answer, and it is
    /// returned before anything reaches the network.
    ///
    /// # Errors
    /// Returns [`TransportError::EndpointNotRegistered`] without a lease,
    /// or [`TransportError::CapabilityDenied`] without `commands`.
    pub fn authorize_direct_send(&self) -> Result<&EndpointId, TransportError> {
        if !self.holds(DataCapability::Commands) {
            return Err(TransportError::CapabilityDenied);
        }
        self.source_endpoint()
            .ok_or(TransportError::EndpointNotRegistered)
    }
}

/// Administrative authority, reachable only on its own binding.
///
/// Deliberately **not** constructible from a [`LocalDataSession`]. There is
/// no `From`, no `TryFrom`, no upgrade method, and no capability a data
/// session can present to obtain one. On desktop this is a separate socket
/// and on Android a separate in-process facade; in both cases the boundary
/// is that network and event handlers are constructed without a handle to
/// this type at all.
///
/// On Android the separation prevents confused-deputy wiring. It is not an
/// OS sandbox, and this documentation says so rather than letting a reader
/// infer a protection that is not there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalAdminPort {
    port_id: Generation,
    capabilities: BTreeSet<AdminCapability>,
}

impl LocalAdminPort {
    /// Build an admin port. Only the local control path calls this.
    #[must_use]
    pub fn new(
        port_id: Generation,
        capabilities: impl IntoIterator<Item = AdminCapability>,
    ) -> Self {
        Self {
            port_id,
            capabilities: capabilities.into_iter().collect(),
        }
    }

    /// This port's opaque identifier.
    #[must_use]
    pub const fn port_id(&self) -> &Generation {
        &self.port_id
    }

    /// Whether this port holds a given administrative authority.
    #[must_use]
    pub fn holds(&self, capability: AdminCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// An admin port never holds an endpoint lease.
    ///
    /// A constant rather than a field: administrative connections do not
    /// obtain application endpoint leases, so there is no state that could
    /// drift from the rule (ADR-0037).
    #[must_use]
    pub const fn endpoint_lease(&self) -> Option<&EndpointLease> {
        None
    }
}

/// Why a lease claim was refused, in local detail.
///
/// These are **local** answers. The remote wire keeps the coarse
/// `no_route` class, because distinguishing "unknown" from "disabled"
/// from "in use" would tell a probing peer which endpoints exist, which
/// are configured, and which are currently occupied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseRefusal {
    /// No such endpoint is configured.
    EndpointUnknown,
    /// The endpoint exists but is disabled.
    EndpointDisabled,
    /// This client kind is not allowed on that endpoint.
    EndpointClientKindDenied,
    /// Another live session already owns it.
    EndpointInUse,
    /// The connection lacks the capability to claim at all.
    CapabilityDenied,
}

impl From<LeaseRefusal> for TransportError {
    fn from(value: LeaseRefusal) -> Self {
        match value {
            LeaseRefusal::EndpointUnknown => Self::EndpointUnknown,
            LeaseRefusal::EndpointDisabled => Self::EndpointDisabled,
            LeaseRefusal::EndpointClientKindDenied => Self::EndpointClientKindDenied,
            LeaseRefusal::EndpointInUse => Self::EndpointInUse,
            LeaseRefusal::CapabilityDenied => Self::CapabilityDenied,
        }
    }
}

/// A normalized event delivered to exactly one local session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum LocalSessionEvent {
    /// This session's lease was revoked by administration or reload.
    ///
    /// Carries the epoch that ended, so a client can tell which routes to
    /// discard rather than discarding all of them.
    EndpointLeaseChanged {
        /// The endpoint whose lease ended.
        endpoint: EndpointId,
        /// The epoch that is no longer valid.
        revoked_epoch: Generation,
    },
    /// A peer's data-plane connection ended.
    PeerDisconnected {
        /// Which peer.
        peer: TransportIdentity,
        /// Coarse class; `policy` when trust revocation caused it.
        reason_class: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(seed: &str) -> Generation {
        Generation::parse(format!("{seed:_<16}")).expect("valid generation")
    }

    fn endpoint(name: &str) -> EndpointId {
        EndpointId::parse(name).expect("valid endpoint")
    }

    fn session(lease: Option<EndpointLease>, caps: &[DataCapability]) -> LocalDataSession {
        LocalDataSession::new(
            generation("sess"),
            "human-client",
            lease,
            caps.iter().copied(),
            DEFAULT_EVENT_QUEUE,
        )
        .expect("valid session")
    }

    fn leased() -> EndpointLease {
        EndpointLease {
            endpoint: endpoint("human"),
            epoch: generation("epoch"),
        }
    }

    #[test]
    fn the_source_endpoint_comes_from_the_lease() {
        let s = session(Some(leased()), &[DataCapability::Commands]);
        assert_eq!(s.source_endpoint(), Some(&endpoint("human")));
        assert_eq!(s.authorize_direct_send(), Ok(&endpoint("human")));
    }

    #[test]
    fn a_session_without_a_lease_cannot_send_at_all() {
        // Not "sends as nobody" — refused before anything reaches the
        // network.
        let s = session(None, &[DataCapability::Commands]);
        assert_eq!(s.source_endpoint(), None);
        assert_eq!(
            s.authorize_direct_send(),
            Err(TransportError::EndpointNotRegistered)
        );
    }

    #[test]
    fn commands_are_required_even_with_a_lease() {
        let s = session(Some(leased()), &[DataCapability::Events]);
        assert_eq!(
            s.authorize_direct_send(),
            Err(TransportError::CapabilityDenied)
        );
    }

    #[test]
    fn client_kind_creates_no_authority() {
        // A session may call itself anything. Authority is the capability
        // set, and admin.* is not representable in it.
        let s = session(Some(leased()), &[DataCapability::Events]);
        assert_eq!(s.client_kind(), "human-client");

        let pretender = LocalDataSession::new(
            generation("sess"),
            "admin",
            Some(leased()),
            [DataCapability::Commands, DataCapability::Events],
            DEFAULT_EVENT_QUEUE,
        )
        .expect("valid session");
        assert_eq!(pretender.client_kind(), "admin");
        // Every capability it holds is a data capability, by construction.
        for c in pretender.capabilities() {
            assert!(matches!(
                c,
                DataCapability::Events | DataCapability::Commands | DataCapability::EndpointsQuery
            ));
        }
    }

    #[test]
    fn admin_capabilities_do_not_deserialize_into_a_data_session() {
        // The structural claim, checked rather than asserted in prose: a
        // handshake response naming an admin capability cannot produce a
        // data session that holds it.
        let json = serde_json::json!({
            "session_id": "sess____________",
            "client_kind": "human-client",
            "capabilities": ["admin.shutdown"],
            "event_queue": 256
        });
        assert!(serde_json::from_value::<LocalDataSession>(json).is_err());

        let json = serde_json::json!({
            "session_id": "sess____________",
            "client_kind": "human-client",
            "capabilities": ["events", "admin.endpoints"],
            "event_queue": 256
        });
        assert!(serde_json::from_value::<LocalDataSession>(json).is_err());
    }

    #[test]
    fn an_admin_port_never_holds_a_lease() {
        let port = LocalAdminPort::new(generation("port"), [AdminCapability::Shutdown]);
        assert!(port.holds(AdminCapability::Shutdown));
        assert!(!port.holds(AdminCapability::Endpoints));
        assert_eq!(port.endpoint_lease(), None);
    }

    #[test]
    fn deserialization_cannot_bypass_the_session_invariants() {
        // The serialized boundary is where untrusted input arrives, so it
        // is the path that most needs the constructor's checks.
        for bad in [
            serde_json::json!({
                "session_id": "sess____________",
                "client_kind": "human-client",
                "capabilities": ["events"],
                "event_queue": 0
            }),
            serde_json::json!({
                "session_id": "sess____________",
                "client_kind": "",
                "capabilities": ["events"],
                "event_queue": 256
            }),
            serde_json::json!({
                "session_id": "short",
                "client_kind": "human-client",
                "capabilities": ["events"],
                "event_queue": 256
            }),
        ] {
            assert!(
                serde_json::from_value::<LocalDataSession>(bad.clone()).is_err(),
                "{bad} should not deserialize"
            );
        }

        // A well-formed session still round-trips.
        let s = session(Some(leased()), &[DataCapability::Events]);
        let json = serde_json::to_value(&s).expect("ser");
        assert_eq!(
            serde_json::from_value::<LocalDataSession>(json).expect("de"),
            s
        );
    }

    #[test]
    fn a_zero_length_event_queue_is_refused() {
        // It reads like "unbounded" and behaves like "closed": every direct
        // message to the session would be refused.
        assert_eq!(
            LocalDataSession::new(generation("sess"), "k", None, [], 0),
            Err(SessionError::ZeroEventQueue)
        );
    }

    #[test]
    fn an_event_queue_past_the_ceiling_is_refused() {
        assert_eq!(
            LocalDataSession::new(generation("sess"), "k", None, [], MAX_EVENT_QUEUE + 1),
            Err(SessionError::EventQueueTooDeep {
                got: MAX_EVENT_QUEUE + 1
            }),
            "the memory one session can pin is bounded at both ends"
        );
    }

    #[test]
    fn the_ceiling_itself_is_permitted() {
        // A CEILING REACHED, not one approached — otherwise the test
        // above would pass against an off-by-one that also refuses the
        // largest legal depth.
        assert!(LocalDataSession::new(generation("sess"), "k", None, [], MAX_EVENT_QUEUE).is_ok());
    }

    #[test]
    fn generations_are_bounded_and_opaque() {
        assert!(Generation::parse("a".repeat(16)).is_ok());
        assert!(Generation::parse("a".repeat(64)).is_ok());
        assert_eq!(
            Generation::parse("a".repeat(15)),
            Err(SessionError::InvalidGeneration)
        );
        assert_eq!(
            Generation::parse("a".repeat(65)),
            Err(SessionError::InvalidGeneration)
        );
        // Not pinned to one encoding, but the alphabet is bounded so it
        // cannot smuggle structure through a diagnostic.
        assert!(Generation::parse("has spaces here!").is_err());
    }

    #[test]
    fn client_kind_is_bounded() {
        assert!(matches!(
            LocalDataSession::new(generation("s"), "", None, [], 8),
            Err(SessionError::InvalidClientKind { got: 0 })
        ));
        assert!(matches!(
            LocalDataSession::new(generation("s"), "k".repeat(65), None, [], 8),
            Err(SessionError::InvalidClientKind { got: 65 })
        ));
    }

    #[test]
    fn lease_refusals_map_onto_local_transport_errors() {
        // Precise locally; the wire keeps the coarse class, which is why
        // this mapping stops at TransportError and does not reach the wire
        // vocabulary.
        assert_eq!(
            TransportError::from(LeaseRefusal::EndpointInUse),
            TransportError::EndpointInUse
        );
        assert_eq!(
            TransportError::from(LeaseRefusal::EndpointUnknown),
            TransportError::EndpointUnknown
        );
        // And every one of them is no_route once it crosses the boundary.
        for r in [
            LeaseRefusal::EndpointUnknown,
            LeaseRefusal::EndpointDisabled,
            LeaseRefusal::EndpointClientKindDenied,
            LeaseRefusal::EndpointInUse,
            LeaseRefusal::CapabilityDenied,
        ] {
            assert_eq!(
                TransportError::from(r).to_wire(),
                interweave_transport_api::DirectRejectReason::NoRoute
            );
        }
    }

    #[test]
    fn capability_names_use_their_contract_spellings() {
        assert_eq!(
            serde_json::to_value(DataCapability::EndpointsQuery).expect("ser"),
            serde_json::json!("endpoints.query")
        );
        assert_eq!(
            serde_json::to_value(AdminCapability::Shutdown).expect("ser"),
            serde_json::json!("admin.shutdown")
        );
    }
}
