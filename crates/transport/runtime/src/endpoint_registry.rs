// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The EndpointRegistry: who owns which endpoint, and where a directed
//! message lands.
//!
//! Pure state. No socket, no Swarm, no clock — a caller supplies the
//! session identity and the registry answers, which is what lets every
//! rule below be tested by enumeration rather than orchestration.
//!
//! # The rule the whole module exists for
//!
//! **Ordinary remote messages can never create, steal, transfer, or
//! enable a lease.** Every mutating operation here takes a
//! [`LocalSessionId`], and there is no path from an inbound message to
//! one. Resolution — the only thing an inbound message drives — is `&self`
//! and cannot mutate anything (`contracts/ENDPOINTS.md`).
//!
//! # Local precision, coarse wire
//!
//! [`ResolveFailure`] distinguishes unknown, disabled, unleased and
//! default-missing. All four become `no_route` on the wire, and
//! [`ResolveFailure::to_wire`] is the only way to cross that boundary.
//! Keeping the precise reason locally is what makes a diagnostic useful;
//! collapsing it on the wire is what stops the protocol becoming an
//! endpoint-existence oracle (ADR-0030).

use std::collections::BTreeMap;

use interweave_local_client_api::Generation;
use interweave_transport_api::{DirectRejectReason, EndpointId, TransportError};
use interweave_trust_api::{EndpointTrustPolicy, PeerTrustPolicy, TrustDecision};

/// Identifies one local data-plane session.
///
/// A newtype rather than a bare string so a session identity cannot be
/// confused with an endpoint name or a lease epoch at a call site — the
/// three are all opaque strings and all appear in the same functions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalSessionId(pub String);

/// One endpoint as the registry holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredEndpoint {
    /// Whether the endpoint accepts traffic.
    pub enabled: bool,
    /// Client kinds permitted to lease it. Empty means no restriction.
    ///
    /// A misbinding guard, not authentication: `client_kind` is a label a
    /// local client chooses, so this stops an accident, not an attacker.
    pub allowed_client_kinds: Vec<String>,
    /// Inbound narrowing filter over profile trust.
    pub inbound: EndpointTrustPolicy,
    /// Outbound narrowing filter over profile trust.
    pub outbound: EndpointTrustPolicy,
}

impl Default for RegisteredEndpoint {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_client_kinds: Vec::new(),
            inbound: EndpointTrustPolicy::default(),
            outbound: EndpointTrustPolicy::default(),
        }
    }
}

/// A live exclusive lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveLease {
    /// The session that owns it.
    pub owner: LocalSessionId,
    /// The generation, fresh for every grant.
    pub epoch: Generation,
}

/// Why resolving a directed message to a local endpoint failed.
///
/// Every variant is a **local** diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveFailure {
    /// No such endpoint is configured.
    EndpointUnknown,
    /// Configured, but disabled.
    EndpointDisabled,
    /// Configured and enabled, but nothing holds its lease.
    EndpointOffline,
    /// No destination was given and no default is configured.
    NoDefaultConfigured,
    /// Endpoint inbound policy excluded the sender.
    EndpointPolicyDenied,
}

impl ResolveFailure {
    /// The single coarse code every failure becomes on the wire.
    ///
    /// A `const fn` returning one value, deliberately: there is no
    /// mapping table to get wrong, and a future variant cannot acquire a
    /// distinct wire code by being added. Distinguishing these remotely
    /// would let a probing peer enumerate configured endpoints and infer
    /// policy (ADR-0030).
    #[must_use]
    pub const fn to_wire(self) -> DirectRejectReason {
        DirectRejectReason::NoRoute
    }
}

/// Why a lease claim was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimFailure {
    /// No such endpoint is configured.
    EndpointUnknown,
    /// The endpoint is disabled.
    EndpointDisabled,
    /// This client kind may not lease this endpoint.
    EndpointClientKindDenied,
    /// Another live session already owns it.
    EndpointInUse,
}

impl From<ClaimFailure> for TransportError {
    fn from(value: ClaimFailure) -> Self {
        match value {
            ClaimFailure::EndpointUnknown => Self::EndpointUnknown,
            ClaimFailure::EndpointDisabled => Self::EndpointDisabled,
            ClaimFailure::EndpointClientKindDenied => Self::EndpointClientKindDenied,
            ClaimFailure::EndpointInUse => Self::EndpointInUse,
        }
    }
}

/// The configured endpoints, their leases, and the default route.
#[derive(Debug, Clone, Default)]
pub struct EndpointRegistry {
    endpoints: BTreeMap<EndpointId, RegisteredEndpoint>,
    leases: BTreeMap<EndpointId, ActiveLease>,
    default_direct_endpoint: Option<EndpointId>,
}

impl EndpointRegistry {
    /// Build a registry from configuration.
    #[must_use]
    pub fn new(
        endpoints: BTreeMap<EndpointId, RegisteredEndpoint>,
        default_direct_endpoint: Option<EndpointId>,
    ) -> Self {
        Self {
            endpoints,
            leases: BTreeMap::new(),
            default_direct_endpoint,
        }
    }

    /// Claim an endpoint exclusively for one session.
    ///
    /// # Errors
    /// Returns [`ClaimFailure`] when the endpoint is unknown, disabled,
    /// closed to this client kind, or already leased.
    pub fn claim(
        &mut self,
        endpoint: &EndpointId,
        session: LocalSessionId,
        client_kind: &str,
        epoch: Generation,
    ) -> Result<&ActiveLease, ClaimFailure> {
        let Some(configured) = self.endpoints.get(endpoint) else {
            return Err(ClaimFailure::EndpointUnknown);
        };
        if !configured.enabled {
            return Err(ClaimFailure::EndpointDisabled);
        }
        if !configured.allowed_client_kinds.is_empty()
            && !configured
                .allowed_client_kinds
                .iter()
                .any(|k| k == client_kind)
        {
            return Err(ClaimFailure::EndpointClientKindDenied);
        }
        // Exclusive. A second claim is refused rather than displacing the
        // incumbent: taking a lease from a live session would silently
        // redirect its traffic to whoever asked most recently.
        if self.leases.contains_key(endpoint) {
            return Err(ClaimFailure::EndpointInUse);
        }
        let lease = ActiveLease {
            owner: session,
            epoch,
        };
        Ok(self.leases.entry(endpoint.clone()).or_insert(lease))
    }

    /// Release every lease owned by a session.
    ///
    /// Returns the endpoints released. Teardown is immediate and total:
    /// a session that disappears cannot leave a lease behind, or the
    /// endpoint would be permanently unclaimable.
    pub fn release_session(&mut self, session: &LocalSessionId) -> Vec<EndpointId> {
        let released: Vec<EndpointId> = self
            .leases
            .iter()
            .filter(|(_, l)| &l.owner == session)
            .map(|(e, _)| e.clone())
            .collect();
        for e in &released {
            self.leases.remove(e);
        }
        released
    }

    /// Revoke one endpoint's lease administratively.
    ///
    /// Returns the epoch that ended, so a caller can tell clients which
    /// routes to discard rather than discarding all of them.
    pub fn revoke(&mut self, endpoint: &EndpointId) -> Option<Generation> {
        self.leases.remove(endpoint).map(|l| l.epoch)
    }

    /// Disable an endpoint, revoking any live lease.
    ///
    /// Disabling without revoking would leave a session believing it still
    /// owns a route that no longer accepts traffic.
    pub fn set_enabled(&mut self, endpoint: &EndpointId, enabled: bool) -> Option<Generation> {
        let e = self.endpoints.get_mut(endpoint)?;
        e.enabled = enabled;
        if enabled { None } else { self.revoke(endpoint) }
    }

    /// Change the configured default route.
    pub fn set_default(&mut self, endpoint: Option<EndpointId>) {
        self.default_direct_endpoint = endpoint;
    }

    /// The configured default, if any.
    #[must_use]
    pub const fn default_endpoint(&self) -> Option<&EndpointId> {
        self.default_direct_endpoint.as_ref()
    }

    /// The live lease for an endpoint, if one exists.
    #[must_use]
    pub fn lease(&self, endpoint: &EndpointId) -> Option<&ActiveLease> {
        self.leases.get(endpoint)
    }

    /// Resolve an inbound directed message to exactly one local endpoint.
    ///
    /// Takes `&self`: resolution is the only thing an inbound message
    /// drives, and it must not be able to change any state. That is the
    /// type-level half of "remote messages never create or steal a lease".
    ///
    /// `requested` of `None` means the configured default — never fan-out.
    /// The signature enforces that too, since a single `EndpointId` is the
    /// only thing it can return.
    ///
    /// # Errors
    /// Returns [`ResolveFailure`] with the local reason; every one becomes
    /// `no_route` on the wire.
    pub fn resolve_inbound(
        &self,
        requested: Option<&EndpointId>,
        source_peer_allowed: impl Fn(&EndpointTrustPolicy) -> bool,
    ) -> Result<(EndpointId, &ActiveLease), ResolveFailure> {
        let target = match requested {
            Some(e) => e.clone(),
            None => self
                .default_direct_endpoint
                .clone()
                .ok_or(ResolveFailure::NoDefaultConfigured)?,
        };
        let Some(configured) = self.endpoints.get(&target) else {
            return Err(ResolveFailure::EndpointUnknown);
        };
        if !configured.enabled {
            return Err(ResolveFailure::EndpointDisabled);
        }
        // Endpoint inbound policy is a NARROWING filter applied after
        // profile trust, which the caller has already applied. The closure
        // shape is what keeps that ordering out of this module's hands.
        if !source_peer_allowed(&configured.inbound) {
            return Err(ResolveFailure::EndpointPolicyDenied);
        }
        let Some(lease) = self.leases.get(&target) else {
            // Configured, enabled, policy-permitted — and nobody is
            // listening. No buffering is created here: an unleased
            // endpoint drops the message rather than accumulating one.
            return Err(ResolveFailure::EndpointOffline);
        };
        Ok((target, lease))
    }

    /// Decide whether a peer may be addressed from this endpoint.
    ///
    /// Profile trust first, endpoint narrowing second — the ordering that
    /// makes widening impossible, delegated to `trust-api` rather than
    /// re-implemented here.
    #[must_use]
    pub fn authorize_outbound(
        &self,
        from: &EndpointId,
        peer: &interweave_transport_api::TransportIdentity,
        profile: &PeerTrustPolicy,
    ) -> TrustDecision {
        let Some(configured) = self.endpoints.get(from) else {
            // An endpoint that is not configured narrows nothing, because
            // it cannot send at all.
            return profile.decide_for_endpoint(
                peer,
                &EndpointTrustPolicy::StaticSubset {
                    allowed_peers: std::collections::BTreeSet::new(),
                },
            );
        };
        profile.decide_for_endpoint(peer, &configured.outbound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interweave_transport_api::TransportIdentity;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    fn ep(name: &str) -> EndpointId {
        EndpointId::parse(name).expect("valid endpoint")
    }
    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }
    fn session(name: &str) -> LocalSessionId {
        LocalSessionId(name.to_owned())
    }
    fn epoch(seed: &str) -> Generation {
        Generation::parse(format!("{seed:_<16}")).expect("valid generation")
    }
    fn allow_all(_: &EndpointTrustPolicy) -> bool {
        true
    }

    fn registry() -> EndpointRegistry {
        let mut endpoints = BTreeMap::new();
        endpoints.insert(ep("human"), RegisteredEndpoint::default());
        endpoints.insert(ep("claude"), RegisteredEndpoint::default());
        EndpointRegistry::new(endpoints, Some(ep("human")))
    }

    #[test]
    fn a_lease_is_exclusive_and_a_duplicate_claim_is_refused() {
        let mut r = registry();
        assert!(
            r.claim(&ep("human"), session("a"), "human-client", epoch("e1"))
                .is_ok()
        );
        // Refused rather than displacing: taking a lease from a live
        // session would silently redirect its traffic.
        assert_eq!(
            r.claim(&ep("human"), session("b"), "human-client", epoch("e2")),
            Err(ClaimFailure::EndpointInUse)
        );
        assert_eq!(r.lease(&ep("human")).map(|l| &l.owner), Some(&session("a")));
    }

    #[test]
    fn claim_failures_match_the_contract_mapping() {
        let mut r = registry();
        assert_eq!(
            r.claim(&ep("absent"), session("a"), "k", epoch("e")),
            Err(ClaimFailure::EndpointUnknown)
        );
        r.set_enabled(&ep("claude"), false);
        assert_eq!(
            r.claim(&ep("claude"), session("a"), "k", epoch("e")),
            Err(ClaimFailure::EndpointDisabled)
        );

        let mut endpoints = BTreeMap::new();
        endpoints.insert(
            ep("human"),
            RegisteredEndpoint {
                allowed_client_kinds: vec!["human-client".to_owned()],
                ..RegisteredEndpoint::default()
            },
        );
        let mut r = EndpointRegistry::new(endpoints, None);
        assert_eq!(
            r.claim(&ep("human"), session("a"), "claude-channel", epoch("e")),
            Err(ClaimFailure::EndpointClientKindDenied)
        );
        assert!(
            r.claim(&ep("human"), session("a"), "human-client", epoch("e"))
                .is_ok()
        );
    }

    #[test]
    fn an_omitted_destination_resolves_to_the_default_never_fan_out() {
        let mut r = registry();
        r.claim(&ep("human"), session("a"), "k", epoch("e1"))
            .expect("claimed");
        r.claim(&ep("claude"), session("b"), "k", epoch("e2"))
            .expect("claimed");

        let (resolved, lease) = r.resolve_inbound(None, allow_all).expect("resolves");
        // ONE endpoint, not both. The signature cannot express fan-out.
        assert_eq!(resolved, ep("human"));
        assert_eq!(lease.owner, session("a"));
    }

    #[test]
    fn every_resolve_failure_is_indistinguishable_on_the_wire() {
        // The local reasons are precise; the wire answer is one value.
        for f in [
            ResolveFailure::EndpointUnknown,
            ResolveFailure::EndpointDisabled,
            ResolveFailure::EndpointOffline,
            ResolveFailure::NoDefaultConfigured,
            ResolveFailure::EndpointPolicyDenied,
        ] {
            assert_eq!(f.to_wire(), DirectRejectReason::NoRoute);
        }
    }

    #[test]
    fn resolution_distinguishes_locally_what_the_wire_collapses() {
        let mut r = registry();
        assert_eq!(
            r.resolve_inbound(Some(&ep("absent")), allow_all),
            Err(ResolveFailure::EndpointUnknown)
        );
        // Configured and enabled, but unleased: no buffering is created.
        assert_eq!(
            r.resolve_inbound(Some(&ep("human")), allow_all),
            Err(ResolveFailure::EndpointOffline)
        );
        r.set_enabled(&ep("human"), false);
        assert_eq!(
            r.resolve_inbound(Some(&ep("human")), allow_all),
            Err(ResolveFailure::EndpointDisabled)
        );
        let empty = EndpointRegistry::new(BTreeMap::new(), None);
        assert_eq!(
            empty.resolve_inbound(None, allow_all),
            Err(ResolveFailure::NoDefaultConfigured)
        );
    }

    #[test]
    fn endpoint_policy_denial_happens_before_the_lease_is_consulted() {
        // Otherwise the presence of a lease would be observable through
        // timing or through which error surfaced locally.
        let mut r = registry();
        r.claim(&ep("human"), session("a"), "k", epoch("e"))
            .expect("claimed");
        assert_eq!(
            r.resolve_inbound(Some(&ep("human")), |_| false),
            Err(ResolveFailure::EndpointPolicyDenied)
        );
    }

    #[test]
    fn session_teardown_releases_every_lease_it_held() {
        let mut r = registry();
        r.claim(&ep("human"), session("a"), "k", epoch("e1"))
            .expect("claimed");
        r.claim(&ep("claude"), session("a"), "k", epoch("e2"))
            .expect("claimed");
        let released = r.release_session(&session("a"));
        assert_eq!(released, vec![ep("claude"), ep("human")]);
        assert!(r.lease(&ep("human")).is_none());
        // And the endpoint is claimable again, rather than stuck.
        assert!(
            r.claim(&ep("human"), session("b"), "k", epoch("e3"))
                .is_ok()
        );
    }

    #[test]
    fn disabling_an_endpoint_revokes_its_lease() {
        // Leaving the lease would have a session believing it owns a route
        // that no longer accepts traffic.
        let mut r = registry();
        r.claim(&ep("human"), session("a"), "k", epoch("live"))
            .expect("claimed");
        let ended = r.set_enabled(&ep("human"), false);
        assert_eq!(ended, Some(epoch("live")));
        assert!(r.lease(&ep("human")).is_none());
    }

    #[test]
    fn a_fresh_claim_gets_a_fresh_epoch_so_stale_routes_are_recognisable() {
        let mut r = registry();
        r.claim(&ep("human"), session("a"), "k", epoch("first"))
            .expect("claimed");
        r.release_session(&session("a"));
        r.claim(&ep("human"), session("a"), "k", epoch("second"))
            .expect("reclaimed");
        assert_eq!(
            r.lease(&ep("human")).map(|l| &l.epoch),
            Some(&epoch("second"))
        );
        assert_ne!(epoch("first"), epoch("second"));
    }

    #[test]
    fn outbound_authorization_narrows_and_never_widens() {
        let profile = PeerTrustPolicy::new([peer(P1)]).expect("within ceiling");
        let mut endpoints = BTreeMap::new();
        endpoints.insert(
            ep("narrow"),
            RegisteredEndpoint {
                outbound: EndpointTrustPolicy::StaticSubset {
                    allowed_peers: std::collections::BTreeSet::new(),
                },
                ..RegisteredEndpoint::default()
            },
        );
        endpoints.insert(
            ep("widen"),
            RegisteredEndpoint {
                outbound: EndpointTrustPolicy::StaticSubset {
                    allowed_peers: [peer(P2)].into_iter().collect(),
                },
                ..RegisteredEndpoint::default()
            },
        );
        endpoints.insert(ep("inherit"), RegisteredEndpoint::default());
        let r = EndpointRegistry::new(endpoints, None);

        assert!(
            r.authorize_outbound(&ep("inherit"), &peer(P1), &profile)
                .is_allowed()
        );
        assert!(
            !r.authorize_outbound(&ep("narrow"), &peer(P1), &profile)
                .is_allowed()
        );
        // The endpoint names P2, the profile does not: still refused.
        assert!(
            !r.authorize_outbound(&ep("widen"), &peer(P2), &profile)
                .is_allowed()
        );
    }

    #[test]
    fn an_unconfigured_source_endpoint_can_send_to_nobody() {
        let profile = PeerTrustPolicy::new([peer(P1)]).expect("within ceiling");
        let r = EndpointRegistry::new(BTreeMap::new(), None);
        assert!(
            !r.authorize_outbound(&ep("ghost"), &peer(P1), &profile)
                .is_allowed()
        );
    }
}
