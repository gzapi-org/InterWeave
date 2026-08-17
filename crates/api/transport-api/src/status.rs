// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Capabilities, health, connectivity, and the error vocabulary.

use serde::{Deserialize, Serialize};

use crate::payload::MAX_PAYLOAD_BYTES;

/// What the active backend and profile actually support.
///
/// Consumers branch on these rather than inferring behaviour from which
/// backend is compiled in. `durable_delivery` and `offline_mailbox` are
/// `bool` fields that are always false today precisely so a consumer must
/// read them: a client that assumed durability because it saw a daemon is
/// the failure this shape prevents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportCapabilities {
    /// Signed GossipSub broadcast is available.
    pub broadcast: bool,
    /// Directed delivery is available.
    pub direct_delivery: bool,
    /// Directed messages may name a remote endpoint.
    pub direct_endpoint_addressing: bool,
    /// The remote endpoint directory is enabled.
    pub endpoint_directory: bool,
    /// AutoNAT/relay Internet reachability is active.
    pub internet_reachability: bool,
    /// Circuit Relay v2 paths are usable.
    pub relayed_connectivity: bool,
    /// DCUtR direct-path upgrade is available.
    pub direct_path_upgrade: bool,
    /// Always false: the transport has no durable delivery (ADR-0018).
    pub durable_delivery: bool,
    /// Always false: the transport has no offline mailbox (ADR-0020).
    pub offline_mailbox: bool,
    /// The profile's EFFECTIVE payload limit, never above the ceiling.
    pub max_payload_bytes: usize,
    /// ChannelId ceiling in bytes.
    pub max_channel_id_bytes: usize,
    /// EndpointId ceiling in bytes.
    pub max_endpoint_id_bytes: usize,
}

impl TransportCapabilities {
    /// Clamp `max_payload_bytes` to the architecture ceiling.
    ///
    /// Reported capabilities are what a client sizes its buffers from, so
    /// a profile misconfigured above the ceiling must not be advertised —
    /// the clamp happens here rather than being trusted from config.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.max_payload_bytes = self.max_payload_bytes.min(MAX_PAYLOAD_BYTES);
        self
    }
}

/// Aggregate operational health. Not an application workflow state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    /// Operating normally.
    Healthy,
    /// Operating with reduced capability.
    Degraded,
    /// Not operating.
    Unavailable,
}

/// Relay inbound readiness.
///
/// Distinct from [`DirectInboundState`] on purpose. Relay reachability is
/// a matter of degree — reservations come up one at a time — while direct
/// reachability is a question of *evidence*, and collapsing the two into
/// one three-state enum loses the distinction that matters most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathReadiness {
    /// No usable relayed inbound path.
    Unavailable,
    /// Partially established.
    Partial,
    /// Established and usable.
    Ready,
}

/// Direct inbound reachability, expressed as **evidence** rather than degree.
///
/// `Unknown` and `NotVerified` are different answers and must stay
/// different: the first means AutoNAT has not concluded, the second means
/// it concluded negatively. Merging them would let a client treat "we have
/// not looked yet" as "we are not reachable" and give up a path it has,
/// or the reverse. `VerifiedPublic` is the only state that may promote an
/// address for advertisement (ADR-0035).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectInboundState {
    /// No sufficient evidence yet; AutoNAT has not concluded.
    Unknown,
    /// AutoNAT v2 verified a publicly reachable direct address.
    VerifiedPublic,
    /// Evidence exists and says the node is not directly reachable.
    NotVerified,
}

/// The path preference in force.
///
/// Standard v1 supports exactly one policy, so this is a single-variant
/// enum rather than a `bool` or a free string. Modelling a second policy
/// the transport does not implement would let a configuration express a
/// mode nothing honours, and the schema pins the value as a `const`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferredPathPolicy {
    /// Prefer a direct path, upgrading from relay when DCUtR succeeds.
    DirectFirst,
}

/// Backend-neutral connectivity summary.
///
/// Every member is required. An absent counter and a zero counter mean
/// different things — "this daemon did not say" versus "there are none" —
/// and a status API that blurs them cannot be branched on safely.
///
/// The counters are `u16` because that is the width the contract and the
/// schema both state. Using a wider integer would accept values here that
/// serialize successfully and are then rejected by the schema, which is
/// the drift this crate's agreement suite exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectivitySummary {
    /// Direct inbound reachability evidence.
    pub direct_inbound: DirectInboundState,
    /// Relayed inbound readiness.
    pub relay_inbound: PathReadiness,
    /// Reservations currently held.
    pub active_relay_reservations: u16,
    /// Reservations the policy is trying to hold.
    pub target_relay_reservations: u16,
    /// Peer paths currently traversing a relay.
    pub active_relayed_peer_paths: u16,
    /// Hole-punch attempts in flight.
    pub hole_punch_inflight: u16,
    /// The active preference.
    pub preferred_path_policy: PreferredPathPolicy,
    /// Local millisecond timestamp of this summary.
    pub updated_at: u64,
}

/// The stable local error vocabulary (`contracts/TRANSPORT.md` §Error model).
///
/// Local errors are deliberately MORE precise than the remote wire: a
/// remote peer never learns `EndpointUnknown` versus `EndpointDisabled`,
/// because those collapse to a coarse `no_route` class so the directed
/// protocol cannot become an endpoint-existence oracle (ADR-0030). Keeping
/// the precise set here and the coarse set on the wire is the asymmetry
/// that makes the privacy property reviewable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportError {
    /// A parameter did not satisfy its contract.
    InvalidArgument,
    /// The payload exceeded the effective limit.
    PayloadTooLarge,
    /// Publish attempted without a caller-owned join.
    ChannelNotJoined,
    /// The caller holds no endpoint lease.
    EndpointNotRegistered,
    /// The named endpoint is not configured.
    EndpointUnknown,
    /// The endpoint is already leased.
    EndpointInUse,
    /// The endpoint is configured but disabled.
    EndpointDisabled,
    /// The endpoint refuses this client kind.
    EndpointClientKindDenied,
    /// The connection lacks the required capability.
    CapabilityDenied,
    /// Trust policy rejected the peer.
    UnauthorizedPeer,
    /// No such peer is known.
    PeerUnknown,
    /// The peer could not be reached.
    PeerUnreachable,
    /// The remote responded with the coarse no-route class.
    RemoteEndpointUnavailable,
    /// The operation exceeded its deadline.
    Timeout,
    /// Cancelled before dispatch; nothing crossed a network boundary.
    CancelledBeforeDispatch,
    /// Cancellation raced completion; the outcome is not determined by this.
    CancellationRaced,
    /// A bounded resource refused admission.
    Overloaded,
    /// The backend is not available.
    BackendUnavailable,
    /// The remote does not support the protocol.
    ProtocolUnsupported,
    /// The remote violated the protocol.
    ProtocolViolation,
    /// Version negotiation failed.
    VersionIncompatible,
    /// The runtime is shutting down.
    ShuttingDown,
    /// An unclassified internal failure.
    Internal,
}

/// The coarse vocabulary a remote peer is allowed to see.
///
/// Deliberately a **separate type** from [`TransportError`], not a subset
/// of it. No local code may be encoded on the wire, so the two vocabularies
/// cannot share a representation: the only way to produce one of these is
/// [`TransportError::to_wire`], and that function is where the collapsing
/// is decided and reviewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectRejectReason {
    /// Collapses endpoint unknown, disabled, unleased, default-missing, and
    /// endpoint-policy denial into one indistinguishable answer.
    NoRoute,
    /// The peer is not admitted by trust policy.
    UnauthorizedPeer,
    /// A bounded resource refused admission.
    Overloaded,
    /// The frame did not parse or violated the protocol.
    Malformed,
    /// The payload exceeded the limit.
    TooLarge,
    /// The runtime is shutting down.
    ShuttingDown,
    /// The protocol or version is not supported.
    Unsupported,
}

impl TransportError {
    /// Map a local error onto the coarse vocabulary a peer may receive.
    ///
    /// This exists instead of an `is_remote_safe` predicate, which was the
    /// wrong shape: a boolean invites a caller to forward the local code
    /// whenever it answers true, and most local codes answer true while
    /// still being things a peer must not learn. `EndpointInUse` is the
    /// clearest case — it reveals both that an endpoint exists and that it
    /// currently holds a lease — but `CapabilityDenied` and
    /// `BackendUnavailable` leak local posture just as surely.
    ///
    /// Five locally-distinct conditions collapse into [`DirectRejectReason::NoRoute`]
    /// so the directed protocol cannot become an endpoint-existence or
    /// policy oracle (ADR-0030). The mapping is total: every local category
    /// has a wire answer, because a missing arm would otherwise be filled
    /// in at the call site under deadline.
    #[must_use]
    pub const fn to_wire(self) -> DirectRejectReason {
        match self {
            // The oracle-prevention set. Everything an attacker could use
            // to enumerate endpoints or probe policy answers identically.
            Self::EndpointUnknown
            | Self::EndpointDisabled
            | Self::EndpointClientKindDenied
            | Self::EndpointNotRegistered
            | Self::EndpointInUse
            | Self::RemoteEndpointUnavailable
            | Self::PeerUnknown
            | Self::CapabilityDenied => DirectRejectReason::NoRoute,

            Self::UnauthorizedPeer => DirectRejectReason::UnauthorizedPeer,
            Self::PayloadTooLarge => DirectRejectReason::TooLarge,
            Self::Overloaded => DirectRejectReason::Overloaded,
            Self::ShuttingDown => DirectRejectReason::ShuttingDown,
            Self::ProtocolUnsupported | Self::VersionIncompatible => {
                DirectRejectReason::Unsupported
            }
            Self::InvalidArgument | Self::ProtocolViolation => DirectRejectReason::Malformed,

            // Local-only conditions with no business being described to a
            // peer. `Overloaded` is the honest answer: the request was not
            // admitted, and why is not the sender's concern.
            Self::ChannelNotJoined
            | Self::PeerUnreachable
            | Self::Timeout
            | Self::CancelledBeforeDispatch
            | Self::CancellationRaced
            | Self::BackendUnavailable
            | Self::Internal => DirectRejectReason::Overloaded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_clamp_a_misconfigured_payload_limit() {
        let caps = TransportCapabilities {
            broadcast: true,
            direct_delivery: true,
            direct_endpoint_addressing: true,
            endpoint_directory: false,
            internet_reachability: true,
            relayed_connectivity: true,
            direct_path_upgrade: true,
            durable_delivery: false,
            offline_mailbox: false,
            max_payload_bytes: usize::MAX,
            max_channel_id_bytes: 128,
            max_endpoint_id_bytes: 64,
        }
        .clamped();
        assert_eq!(caps.max_payload_bytes, MAX_PAYLOAD_BYTES);
        // These two are not aspirational flags to flip later; the transport
        // contract says they are false, and a consumer reads them.
        assert!(!caps.durable_delivery);
        assert!(!caps.offline_mailbox);
    }

    #[test]
    fn state_enums_use_their_contract_spellings() {
        assert_eq!(
            serde_json::to_string(&Health::Healthy).expect("ser"),
            "\"healthy\""
        );
        // direct_inbound is EVIDENCE, not degree: unknown and not_verified
        // are different answers and the schema names both.
        assert_eq!(
            serde_json::to_string(&DirectInboundState::VerifiedPublic).expect("ser"),
            "\"verified_public\""
        );
        assert_eq!(
            serde_json::to_string(&DirectInboundState::NotVerified).expect("ser"),
            "\"not_verified\""
        );
        assert_eq!(
            serde_json::to_string(&PathReadiness::Unavailable).expect("ser"),
            "\"unavailable\""
        );
        assert_eq!(
            serde_json::to_string(&PreferredPathPolicy::DirectFirst).expect("ser"),
            "\"direct_first\""
        );

        for h in [Health::Healthy, Health::Degraded, Health::Unavailable] {
            let j = serde_json::to_string(&h).expect("ser");
            assert_eq!(serde_json::from_str::<Health>(&j).expect("de"), h);
        }
        for d in [
            DirectInboundState::Unknown,
            DirectInboundState::VerifiedPublic,
            DirectInboundState::NotVerified,
        ] {
            let j = serde_json::to_string(&d).expect("ser");
            assert_eq!(
                serde_json::from_str::<DirectInboundState>(&j).expect("de"),
                d
            );
        }
    }

    #[test]
    fn every_local_error_maps_to_the_coarse_wire_vocabulary() {
        // The oracle-prevention set. EndpointInUse belongs here because it
        // would otherwise reveal both that an endpoint exists AND that it
        // currently holds a lease.
        for e in [
            TransportError::EndpointUnknown,
            TransportError::EndpointDisabled,
            TransportError::EndpointClientKindDenied,
            TransportError::EndpointNotRegistered,
            TransportError::EndpointInUse,
            TransportError::RemoteEndpointUnavailable,
            TransportError::PeerUnknown,
            TransportError::CapabilityDenied,
        ] {
            assert_eq!(
                e.to_wire(),
                DirectRejectReason::NoRoute,
                "{e:?} must be indistinguishable on the wire"
            );
        }

        // Local posture is never described to a peer; "not admitted" is
        // the honest and sufficient answer.
        for e in [
            TransportError::BackendUnavailable,
            TransportError::Internal,
            TransportError::Timeout,
            TransportError::ChannelNotJoined,
        ] {
            assert_eq!(
                e.to_wire(),
                DirectRejectReason::Overloaded,
                "{e:?} leaked local state"
            );
        }

        assert_eq!(
            TransportError::UnauthorizedPeer.to_wire(),
            DirectRejectReason::UnauthorizedPeer
        );
        assert_eq!(
            TransportError::PayloadTooLarge.to_wire(),
            DirectRejectReason::TooLarge
        );
        assert_eq!(
            TransportError::ShuttingDown.to_wire(),
            DirectRejectReason::ShuttingDown
        );
        assert_eq!(
            TransportError::ProtocolUnsupported.to_wire(),
            DirectRejectReason::Unsupported
        );
        assert_eq!(
            TransportError::ProtocolViolation.to_wire(),
            DirectRejectReason::Malformed
        );
    }
}
