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

/// Inbound reachability for one path class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathReadiness {
    /// No usable inbound path of this class.
    Unavailable,
    /// Partially established.
    Partial,
    /// Established and usable.
    Ready,
}

/// Which path the manager prefers when both are usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreferredPathPolicy {
    /// Prefer a direct path.
    PreferDirect,
    /// Prefer a relayed path.
    PreferRelay,
}

/// Backend-neutral connectivity summary.
///
/// Every member is required. An absent counter and a zero counter mean
/// different things — "this daemon did not say" versus "there are none" —
/// and a status API that blurs them cannot be branched on safely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectivitySummary {
    /// Direct inbound reachability.
    pub direct_inbound: PathReadiness,
    /// Relayed inbound reachability.
    pub relay_inbound: PathReadiness,
    /// Reservations currently held.
    pub active_relay_reservations: u32,
    /// Reservations the policy is trying to hold.
    pub target_relay_reservations: u32,
    /// Peer paths currently traversing a relay.
    pub active_relayed_peer_paths: u32,
    /// Hole-punch attempts in flight.
    pub hole_punch_inflight: u32,
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

impl TransportError {
    /// Whether this category is safe to reveal to a remote peer.
    ///
    /// The endpoint-existence categories are not: distinguishing them on
    /// the wire would let a probing peer enumerate configured endpoints,
    /// which is exactly what the coarse `no_route` class prevents.
    #[must_use]
    pub const fn is_remote_safe(self) -> bool {
        !matches!(
            self,
            Self::EndpointUnknown
                | Self::EndpointDisabled
                | Self::EndpointClientKindDenied
                | Self::EndpointNotRegistered
                | Self::RemoteEndpointUnavailable
        )
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
    fn health_and_readiness_use_their_contract_spellings() {
        assert_eq!(
            serde_json::to_string(&Health::Healthy).expect("ser"),
            "\"healthy\""
        );
        assert_eq!(
            serde_json::to_string(&Health::Degraded).expect("ser"),
            "\"degraded\""
        );
        assert_eq!(
            serde_json::to_string(&PathReadiness::Unavailable).expect("ser"),
            "\"unavailable\""
        );
        assert_eq!(
            serde_json::to_string(&PreferredPathPolicy::PreferDirect).expect("ser"),
            "\"prefer-direct\""
        );
    }

    #[test]
    fn endpoint_existence_categories_are_not_remote_safe() {
        for e in [
            TransportError::EndpointUnknown,
            TransportError::EndpointDisabled,
            TransportError::EndpointClientKindDenied,
        ] {
            assert!(!e.is_remote_safe(), "{e:?} would be an endpoint oracle");
        }
        assert!(TransportError::Overloaded.is_remote_safe());
        assert!(TransportError::ProtocolViolation.is_remote_safe());
    }
}
