// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Deny-by-default peer trust for the application data plane.
//!
//! Authenticating a PeerId proves who a peer is. It does not decide
//! whether they may do anything, and ADR-0012 keeps those two questions
//! apart: this crate answers only the second, for the **application data
//! plane**, and it answers `Denied` unless something explicitly said
//! otherwise.
//!
//! Three properties are encoded in the types rather than left to callers:
//!
//! - **Deny by default.** [`PeerTrustPolicy::default`] is an empty
//!   allowlist that admits nobody. A policy that fails to load is not a
//!   policy that admits everyone.
//! - **Narrowing only.** [`EndpointTrustPolicy`] can subtract from profile
//!   trust and can never add to it. The intersection happens inside
//!   [`PeerTrustPolicy::decide_for_endpoint`], so no call site can invert
//!   the order and widen (ADR-0012, `contracts/ENDPOINTS.md`).
//! - **Infrastructure is not data-plane trust.** [`InfrastructureSet`] is a
//!   separate type, not a flag on the same one, because a relay or AutoNAT
//!   server is authorized for reachability control and nothing else
//!   (ADR-0036). Merging the two sets is the confused-deputy bug that
//!   separation exists to prevent.
//!
//! Discovery never mutates any of this. Nothing here dials, connects, or
//! knows what a connection is.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use interweave_transport_api::TransportIdentity;
use serde::{Deserialize, Serialize};

/// The answer to "may this peer use the application data plane?".
///
/// Deliberately not a `bool`. A boolean at a security boundary reads the
/// same whether it means "allowed" or "denied", and the reason travels
/// with the decision here so a diagnostic does not have to reconstruct it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision", content = "reason")]
pub enum TrustDecision {
    /// The peer is admitted to the application data plane.
    Allowed,
    /// The peer is refused, with the reason it was refused.
    Denied(DenyReason),
}

impl TrustDecision {
    /// Whether this decision admits the peer.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// Why a peer was refused.
///
/// These are **local** diagnostics. They must not be encoded on the direct
/// wire, where the coarse vocabulary applies: distinguishing "not on the
/// profile allowlist" from "excluded by this endpoint's policy" would tell
/// a probing peer which endpoints exist and how they are configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// Not present in the profile allowlist. The default answer.
    NotAllowlisted,
    /// On the profile allowlist, but excluded by endpoint narrowing.
    NarrowedByEndpoint,
    /// The local profile's own identity, which is never a remote peer.
    SelfIdentity,
}

/// The profile-wide data-plane allowlist.
///
/// Static and deny-by-default (ADR-0012). Discovery, Identify observations,
/// and bootstrap configuration never mutate it — a bootstrap entry is
/// reachability input, not authority, and this type gives them no way in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerTrustPolicy {
    allowed_peers: BTreeSet<TransportIdentity>,
    /// The local profile identity, self-authorized and never remote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_peer: Option<TransportIdentity>,
}

impl PeerTrustPolicy {
    /// Configuration ceiling on allowlist size.
    pub const MAX_ALLOWED_PEERS: usize = 4096;

    /// Build a policy from an explicit allowlist.
    ///
    /// There is deliberately no `allow_all` constructor. ADR-0012 rejected
    /// that default, and a convenience constructor is how a rejected
    /// default returns — first in a test, then in a fixture, then in a
    /// shipped profile.
    #[must_use]
    pub fn new(allowed_peers: impl IntoIterator<Item = TransportIdentity>) -> Self {
        Self {
            allowed_peers: allowed_peers.into_iter().collect(),
            local_peer: None,
        }
    }

    /// Record the local profile identity.
    ///
    /// The local peer is intrinsically self-authorized for runtime identity
    /// checks and need not appear in the allowlist. That does **not** make
    /// self-directed messaging meaningful: a send to the local identity is
    /// `InvalidArgument`, which is why [`Self::decide`] answers
    /// [`DenyReason::SelfIdentity`] rather than `Allowed`.
    #[must_use]
    pub fn with_local_peer(mut self, local: TransportIdentity) -> Self {
        self.local_peer = Some(local);
        self
    }

    /// Number of allowlisted remote peers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.allowed_peers.len()
    }

    /// Whether the allowlist is empty — the default, admitting nobody.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allowed_peers.is_empty()
    }

    /// Whether the allowlist is within its configured ceiling.
    #[must_use]
    pub fn within_bounds(&self) -> bool {
        self.allowed_peers.len() <= Self::MAX_ALLOWED_PEERS
    }

    /// Decide profile-level data-plane trust for one peer.
    #[must_use]
    pub fn decide(&self, peer: &TransportIdentity) -> TrustDecision {
        if self.local_peer.as_ref() == Some(peer) {
            return TrustDecision::Denied(DenyReason::SelfIdentity);
        }
        if self.allowed_peers.contains(peer) {
            TrustDecision::Allowed
        } else {
            TrustDecision::Denied(DenyReason::NotAllowlisted)
        }
    }

    /// Decide trust for a peer at one endpoint, applying narrowing.
    ///
    /// Profile trust is consulted **first** and an endpoint can only
    /// subtract from the result. Doing the intersection here rather than
    /// leaving it to callers is what makes "narrow but never widen"
    /// structural: there is no ordering a caller can choose that lets an
    /// endpoint policy admit a peer the profile refused.
    #[must_use]
    pub fn decide_for_endpoint(
        &self,
        peer: &TransportIdentity,
        endpoint: &EndpointTrustPolicy,
    ) -> TrustDecision {
        match self.decide(peer) {
            TrustDecision::Denied(reason) => TrustDecision::Denied(reason),
            TrustDecision::Allowed => {
                if endpoint.admits(peer) {
                    TrustDecision::Allowed
                } else {
                    TrustDecision::Denied(DenyReason::NarrowedByEndpoint)
                }
            }
        }
    }
}

/// One endpoint's narrowing filter over profile trust.
///
/// `Inherit` is the default and takes the profile's answer unchanged.
/// `StaticSubset` restricts it further. Neither can admit a peer the
/// profile refused — [`PeerTrustPolicy::decide_for_endpoint`] guarantees
/// that structurally, and [`Self::is_subset_of`] lets configuration
/// validation reject a subset naming peers outside profile trust before it
/// ever reaches a decision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "policy")]
pub enum EndpointTrustPolicy {
    /// Take the profile decision unchanged. The default: an endpoint that
    /// says nothing narrows nothing.
    #[default]
    InheritProfileTrust,
    /// Admit only these peers, all of which must also be profile-trusted.
    StaticSubset {
        /// The narrowed set.
        allowed_peers: BTreeSet<TransportIdentity>,
    },
}

impl EndpointTrustPolicy {
    /// Whether this filter admits the peer, ignoring profile trust.
    ///
    /// Private in effect: callers should use
    /// [`PeerTrustPolicy::decide_for_endpoint`], which cannot be ordered
    /// wrongly. It is exposed only for configuration validation.
    #[must_use]
    pub fn admits(&self, peer: &TransportIdentity) -> bool {
        match self {
            Self::InheritProfileTrust => true,
            Self::StaticSubset { allowed_peers } => allowed_peers.contains(peer),
        }
    }

    /// Whether this policy is a genuine subset of the profile allowlist.
    ///
    /// A configuration whose endpoint subset names a peer outside profile
    /// trust is rejected at load rather than silently having no effect:
    /// the operator wrote something that reads like an authorization and
    /// is not one, and discovering that at load beats discovering it when
    /// a peer is unexpectedly refused.
    #[must_use]
    pub fn is_subset_of(&self, profile: &PeerTrustPolicy) -> bool {
        match self {
            Self::InheritProfileTrust => true,
            Self::StaticSubset { allowed_peers } => allowed_peers.is_subset(&profile.allowed_peers),
        }
    }
}

/// Peers authorized for reachability control only (ADR-0036).
///
/// A **separate type** from [`PeerTrustPolicy`], not a flag on it. A relay
/// or AutoNAT server may establish the protocol-scoped control connection
/// and nothing else: no GossipSub, no direct v2, no endpoint directory, no
/// Kademlia routing, no Channel delivery. Sharing one set — or one type
/// with a boolean — is precisely how an infrastructure peer acquires
/// data-plane authority by accident.
///
/// A peer may legitimately appear in both sets; then it is data-plane
/// trusted because the *data-plane* policy says so, never because this one
/// does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfrastructureSet {
    allowed_peers: BTreeSet<TransportIdentity>,
}

impl InfrastructureSet {
    /// Build the infrastructure authorization set.
    #[must_use]
    pub fn new(allowed_peers: impl IntoIterator<Item = TransportIdentity>) -> Self {
        Self {
            allowed_peers: allowed_peers.into_iter().collect(),
        }
    }

    /// Whether this peer may open a reachability-control connection.
    ///
    /// Note the return type: a plain `bool`, and deliberately NOT a
    /// [`TrustDecision`]. The two answers must not be interchangeable at a
    /// call site, because passing this one where a data-plane decision was
    /// expected is the confused deputy this separation prevents.
    #[must_use]
    pub fn permits_control_connection(&self, peer: &TransportIdentity) -> bool {
        self.allowed_peers.contains(peer)
    }

    /// Number of authorized infrastructure peers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.allowed_peers.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allowed_peers.is_empty()
    }
}

/// Emitted when the policy changes, so consumers can re-evaluate.
///
/// Revocation is not merely a future-effect change: removing a peer must
/// evict its active data-plane connectivity (ADR-0012). This event carries
/// the revision so a consumer can tell it missed one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustPolicyChanged {
    /// Monotonic revision of the policy after the change.
    pub revision: u64,
    /// Local millisecond timestamp of the change.
    pub at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";
    const P3: &str = "QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N";

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid test identity")
    }

    #[test]
    fn the_default_policy_admits_nobody() {
        let policy = PeerTrustPolicy::default();
        assert!(policy.is_empty());
        assert_eq!(
            policy.decide(&peer(P1)),
            TrustDecision::Denied(DenyReason::NotAllowlisted)
        );
    }

    #[test]
    fn an_allowlisted_peer_is_admitted_and_others_are_not() {
        let policy = PeerTrustPolicy::new([peer(P1)]);
        assert_eq!(policy.decide(&peer(P1)), TrustDecision::Allowed);
        assert_eq!(
            policy.decide(&peer(P2)),
            TrustDecision::Denied(DenyReason::NotAllowlisted)
        );
    }

    #[test]
    fn the_local_identity_is_never_a_remote_peer() {
        // Self-authorized for identity checks, but a send to self is
        // InvalidArgument and never a self-dial — so the data-plane answer
        // is a denial with its own reason, not Allowed.
        let policy = PeerTrustPolicy::new([peer(P1)]).with_local_peer(peer(P2));
        assert_eq!(
            policy.decide(&peer(P2)),
            TrustDecision::Denied(DenyReason::SelfIdentity)
        );
        // Even if someone also allowlists it, self stays self.
        let policy = PeerTrustPolicy::new([peer(P2)]).with_local_peer(peer(P2));
        assert_eq!(
            policy.decide(&peer(P2)),
            TrustDecision::Denied(DenyReason::SelfIdentity)
        );
    }

    #[test]
    fn an_endpoint_policy_narrows_and_cannot_widen() {
        let profile = PeerTrustPolicy::new([peer(P1)]);

        // Narrowing works: P1 is profile-trusted but excluded here.
        let narrowed = EndpointTrustPolicy::StaticSubset {
            allowed_peers: BTreeSet::new(),
        };
        assert_eq!(
            profile.decide_for_endpoint(&peer(P1), &narrowed),
            TrustDecision::Denied(DenyReason::NarrowedByEndpoint)
        );

        // Widening does not: P2 is named by the endpoint but is not
        // profile-trusted, and the profile answer wins.
        let widening = EndpointTrustPolicy::StaticSubset {
            allowed_peers: [peer(P2)].into_iter().collect(),
        };
        assert_eq!(
            profile.decide_for_endpoint(&peer(P2), &widening),
            TrustDecision::Denied(DenyReason::NotAllowlisted)
        );

        // Inheriting passes the profile answer through unchanged.
        let inherit = EndpointTrustPolicy::default();
        assert_eq!(
            profile.decide_for_endpoint(&peer(P1), &inherit),
            TrustDecision::Allowed
        );
        assert_eq!(
            profile.decide_for_endpoint(&peer(P2), &inherit),
            TrustDecision::Denied(DenyReason::NotAllowlisted)
        );
    }

    #[test]
    fn a_widening_subset_is_detectable_at_configuration_load() {
        let profile = PeerTrustPolicy::new([peer(P1)]);
        let good = EndpointTrustPolicy::StaticSubset {
            allowed_peers: [peer(P1)].into_iter().collect(),
        };
        let bad = EndpointTrustPolicy::StaticSubset {
            allowed_peers: [peer(P1), peer(P2)].into_iter().collect(),
        };
        assert!(good.is_subset_of(&profile));
        assert!(!bad.is_subset_of(&profile));
        assert!(EndpointTrustPolicy::InheritProfileTrust.is_subset_of(&profile));
    }

    #[test]
    fn infrastructure_authorization_is_not_data_plane_trust() {
        let data_plane = PeerTrustPolicy::new([peer(P1)]);
        let infra = InfrastructureSet::new([peer(P3)]);

        // The relay may open a control connection and is still refused by
        // the data plane. This is the whole point of ADR-0036.
        assert!(infra.permits_control_connection(&peer(P3)));
        assert_eq!(
            data_plane.decide(&peer(P3)),
            TrustDecision::Denied(DenyReason::NotAllowlisted)
        );

        // And a data-plane peer is not thereby infrastructure.
        assert!(!infra.permits_control_connection(&peer(P1)));

        // Membership in both is legitimate; the data-plane answer comes
        // from the data-plane policy, never from this set.
        let both = InfrastructureSet::new([peer(P1), peer(P3)]);
        assert!(both.permits_control_connection(&peer(P1)));
        assert_eq!(data_plane.decide(&peer(P1)), TrustDecision::Allowed);
    }

    #[test]
    fn the_allowlist_bound_is_checkable() {
        assert!(PeerTrustPolicy::default().within_bounds());
        assert_eq!(PeerTrustPolicy::MAX_ALLOWED_PEERS, 4096);
    }

    #[test]
    fn decisions_serialize_with_their_reason() {
        let denied = TrustDecision::Denied(DenyReason::NarrowedByEndpoint);
        let json = serde_json::to_value(denied).expect("ser");
        assert_eq!(json["decision"], "denied");
        assert_eq!(json["reason"], "narrowed_by_endpoint");
        assert_eq!(
            serde_json::from_value::<TrustDecision>(json).expect("de"),
            denied
        );
    }
}
