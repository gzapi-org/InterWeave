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
use interweave_transport_api::{
    DirectRejectReason, EndpointId, MAX_DIRECTORY_ENTRIES, TransportError, TransportIdentity,
};
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
    /// Whether it may appear in the directory (ADR-0031).
    ///
    /// Listing only. An unadvertised endpoint still accepts a direct
    /// message addressed to it, and `false` is the safe default because
    /// advertisement is the information-disclosure surface.
    pub advertise: bool,
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
            advertise: false,
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
    /// The generation, distinct across every live lease.
    ///
    /// [`EndpointRegistry::claim`] refuses an epoch another live lease
    /// already carries. Freshness ACROSS RESTARTS is the minting side's
    /// obligation and is not enforced here; see that method.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimFailure {
    /// No such endpoint is configured.
    EndpointUnknown,
    /// The endpoint is disabled.
    EndpointDisabled,
    /// This client kind may not lease this endpoint.
    EndpointClientKindDenied,
    /// Another live session already owns it.
    EndpointInUse,
    /// This session already holds a lease on another endpoint.
    ///
    /// A session owns **at most one** lease. A second would make its
    /// authoritative outbound `source_endpoint` ambiguous, and that value
    /// is the whole of ADR-0030's non-spoofable source.
    SessionAlreadyLeased {
        /// The endpoint it already holds.
        held: EndpointId,
    },
    /// A live lease already carries this epoch.
    ///
    /// `LOCAL-CLIENT.md` requires a "fresh 128-bit lease epoch for every
    /// grant" and `ENDPOINTS.md` that "the value must not repeat". The
    /// epoch is how a client is told WHICH routes to discard when a lease
    /// ends -- [`EndpointRegistry::revoke`] returns it for exactly that
    /// reason -- so two live leases sharing one makes that answer wrong:
    /// revoking either tells the client to discard routes still served by
    /// the other.
    EpochInUse {
        /// The endpoint whose live lease already carries it.
        held: EndpointId,
    },
}

impl From<ClaimFailure> for TransportError {
    fn from(value: ClaimFailure) -> Self {
        match value {
            ClaimFailure::EndpointUnknown => Self::EndpointUnknown,
            ClaimFailure::EndpointDisabled => Self::EndpointDisabled,
            ClaimFailure::EndpointClientKindDenied => Self::EndpointClientKindDenied,
            ClaimFailure::EndpointInUse => Self::EndpointInUse,
            // Locally precise; the caller's own mistake rather than a
            // statement about the endpoint, so it is InvalidArgument
            // rather than an endpoint-existence answer.
            ClaimFailure::SessionAlreadyLeased { .. } | ClaimFailure::EpochInUse { .. } => {
                Self::InvalidArgument
            }
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
    /// closed to this client kind, already leased, or when `epoch` is one
    /// a live lease already carries.
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
        // And one lease per SESSION, not merely one session per endpoint.
        // A session holding two leases has no single authoritative
        // source_endpoint, which is the value ADR-0030 derives from the
        // lease precisely so a caller cannot choose it.
        if let Some((held, _)) = self.leases.iter().find(|(_, l)| l.owner == session) {
            return Err(ClaimFailure::SessionAlreadyLeased { held: held.clone() });
        }
        // AND THE EPOCH IS DISTINCT ACROSS LIVE LEASES.
        //
        // What is enforced here is the bounded half of the contract's
        // rule: no two CURRENTLY HELD leases share an epoch. That is the
        // half `revoke` depends on, and it costs a scan of the lease map,
        // which is bounded by the configured endpoint count.
        //
        // The other half -- that a value never repeats across daemon
        // restarts -- is NOT enforceable here and is deliberately not
        // claimed: it would need a record of every epoch ever issued,
        // which is exactly the unbounded map the resource rules forbid.
        // That half belongs to whoever mints the value at session
        // establishment.
        if let Some((held, _)) = self.leases.iter().find(|(_, l)| l.epoch == epoch) {
            return Err(ClaimFailure::EpochInUse { held: held.clone() });
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

    /// The lease a session holds, if any: the endpoint and the lease.
    ///
    /// This is where a sender's `source_endpoint` comes from (ADR-0030):
    /// the SESSION is the input and the endpoint is the answer, so there
    /// is no shape in which a caller supplies the endpoint and has it
    /// believed — `a_sessions_lease_is_found_by_session_not_by_claim`.
    /// One lease per session is enforced by `claim`, so at most one
    /// answer exists.
    #[must_use]
    pub fn lease_of(&self, session: &LocalSessionId) -> Option<(&EndpointId, &ActiveLease)> {
        self.leases.iter().find(|(_, l)| &l.owner == session)
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

    /// The endpoints this node advertises to `peer` (ADR-0031).
    ///
    /// An endpoint is listed only when it is simultaneously enabled,
    /// `advertise: true`, actively leased, and admissible for `peer` under
    /// its inbound narrowing policy — one test per conjunct in this
    /// module, each named for the endpoint it must NOT list. The list is
    /// sorted because the map is, and is cut at `min(cap,
    /// MAX_DIRECTORY_ENTRIES)`: `more_than_the_cap_is_cut_not_refused`
    /// and `the_cap_never_exceeds_the_wire_bound`.
    ///
    /// Profile trust is applied inside the narrowing decision, so a peer
    /// the profile does not trust sees an empty list even if a caller
    /// forgot to gate the query — `an_untrusted_querier_is_shown_nothing`.
    /// That is defence in depth, not the admission check: the caller
    /// refuses an untrusted query before this is reached, and a refusal
    /// is not an empty list.
    ///
    /// Takes `&self` for the same reason `resolve_inbound` does: a remote
    /// query must not be able to change any state.
    #[must_use]
    pub fn advertised_for(
        &self,
        peer: &TransportIdentity,
        profile: &PeerTrustPolicy,
        cap: usize,
    ) -> Vec<EndpointId> {
        self.endpoints
            .iter()
            .filter(|(_, configured)| configured.enabled && configured.advertise)
            .filter(|(endpoint, _)| self.leases.contains_key(*endpoint))
            .filter(|(_, configured)| {
                profile
                    .decide_for_endpoint(peer, &configured.inbound)
                    .is_allowed()
            })
            .map(|(endpoint, _)| endpoint.clone())
            .take(cap.min(MAX_DIRECTORY_ENTRIES))
            .collect()
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
    fn a_session_may_hold_only_one_lease() {
        // Not merely one session per endpoint: one endpoint per session.
        // Two leases would leave the session with no single authoritative
        // source_endpoint, which is the value ADR-0030 derives from the
        // lease precisely so a caller cannot choose it.
        let mut r = registry();
        r.claim(&ep("human"), session("a"), "k", epoch("e1"))
            .expect("claimed");
        assert_eq!(
            r.claim(&ep("claude"), session("a"), "k", epoch("e2")),
            Err(ClaimFailure::SessionAlreadyLeased { held: ep("human") })
        );
        // Releasing frees the session to claim a different endpoint.
        r.release_session(&session("a"));
        assert!(
            r.claim(&ep("claude"), session("a"), "k", epoch("e3"))
                .is_ok()
        );
    }

    #[test]
    fn a_sessions_lease_is_found_by_session_not_by_claim() {
        let mut r = registry();
        r.claim(&ep("human"), session("a"), "human-client", epoch("e1"))
            .expect("claims");
        r.claim(&ep("claude"), session("b"), "claude-channel", epoch("e2"))
            .expect("claims");
        assert_eq!(
            r.lease_of(&session("a")).map(|(e, _)| e),
            Some(&ep("human"))
        );
        assert_eq!(
            r.lease_of(&session("b")).map(|(e, _)| e),
            Some(&ep("claude"))
        );
        // A session that holds nothing gets nothing — not a default, not
        // the first lease in the map.
        assert_eq!(r.lease_of(&session("c")), None);
        // And after release the answer changes with the fact.
        r.release_session(&session("a"));
        assert_eq!(r.lease_of(&session("a")), None);
    }

    #[test]
    fn session_teardown_releases_the_lease_it_held() {
        let mut r = registry();
        r.claim(&ep("human"), session("a"), "k", epoch("e1"))
            .expect("claimed");
        let released = r.release_session(&session("a"));
        assert_eq!(released, vec![ep("human")]);
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
    fn two_live_leases_may_not_share_an_epoch() {
        // LOCAL-CLIENT.md: "fresh 128-bit lease epoch for every grant".
        // The epoch is what `revoke` returns so a client can discard the
        // routes of the lease that ended; if two live leases carry the
        // same value, revoking one names routes the other still serves.
        let mut r = registry();
        r.claim(&ep("human"), session("a"), "k", epoch("shared"))
            .expect("the first grant takes it");

        assert_eq!(
            r.claim(&ep("claude"), session("b"), "k", epoch("shared")),
            Err(ClaimFailure::EpochInUse { held: ep("human") }),
            "a second live lease may not reuse the epoch"
        );
    }

    #[test]
    fn an_epoch_is_reusable_once_the_lease_holding_it_has_ended() {
        // The rule is about LIVE leases. Refusing forever would need a
        // record of every epoch ever issued, which is the unbounded map
        // the resource rules forbid — so what is enforced is exactly what
        // is bounded, and this pins that boundary.
        let mut r = registry();
        r.claim(&ep("human"), session("a"), "k", epoch("recycled"))
            .expect("granted");
        assert_eq!(r.revoke(&ep("human")), Some(epoch("recycled")));

        r.claim(&ep("claude"), session("b"), "k", epoch("recycled"))
            .expect("the epoch is free once no live lease carries it");
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

    // --- the directory snapshot -------------------------------------------

    fn advertised() -> RegisteredEndpoint {
        RegisteredEndpoint {
            advertise: true,
            ..RegisteredEndpoint::default()
        }
    }
    fn trusting(peers: &[&str]) -> PeerTrustPolicy {
        PeerTrustPolicy::new(peers.iter().map(|p| peer(p)).collect::<Vec<_>>())
            .expect("valid policy")
    }
    fn names(list: &[EndpointId]) -> Vec<&str> {
        list.iter().map(EndpointId::as_str).collect()
    }

    #[test]
    fn a_leased_advertised_admissible_endpoint_is_listed() {
        let mut endpoints = BTreeMap::new();
        endpoints.insert(ep("human"), advertised());
        let mut r = EndpointRegistry::new(endpoints, None);
        r.claim(&ep("human"), session("a"), "human-client", epoch("e1"))
            .expect("claims");
        assert_eq!(
            names(&r.advertised_for(&peer(P1), &trusting(&[P1]), 32)),
            ["human"]
        );
    }

    #[test]
    fn an_unleased_advertised_endpoint_is_not_listed() {
        // Configured, enabled, advertised — and nobody is holding it. The
        // directory lists routes that WORK, and an unleased endpoint
        // answers no_route.
        let mut endpoints = BTreeMap::new();
        endpoints.insert(ep("human"), advertised());
        endpoints.insert(ep("claude"), advertised());
        let mut r = EndpointRegistry::new(endpoints, None);
        r.claim(&ep("human"), session("a"), "human-client", epoch("e1"))
            .expect("claims");
        assert_eq!(
            names(&r.advertised_for(&peer(P1), &trusting(&[P1]), 32)),
            ["human"]
        );
        // And releasing it removes it: the snapshot follows the lease.
        r.release_session(&session("a"));
        assert!(r.advertised_for(&peer(P1), &trusting(&[P1]), 32).is_empty());
    }

    #[test]
    fn a_leased_unadvertised_endpoint_is_not_listed() {
        let mut endpoints = BTreeMap::new();
        endpoints.insert(ep("human"), RegisteredEndpoint::default());
        let mut r = EndpointRegistry::new(endpoints, None);
        r.claim(&ep("human"), session("a"), "human-client", epoch("e1"))
            .expect("claims");
        assert!(r.advertised_for(&peer(P1), &trusting(&[P1]), 32).is_empty());
    }

    #[test]
    fn an_endpoint_whose_policy_excludes_the_querier_is_not_listed() {
        // Both peers are profile-trusted; `claude` narrows to P2 only.
        // P1 must not be told about a route that would answer it no_route.
        let mut endpoints = BTreeMap::new();
        endpoints.insert(ep("human"), advertised());
        endpoints.insert(
            ep("claude"),
            RegisteredEndpoint {
                inbound: EndpointTrustPolicy::StaticSubset {
                    allowed_peers: [peer(P2)].into_iter().collect(),
                },
                ..advertised()
            },
        );
        let mut r = EndpointRegistry::new(endpoints, None);
        r.claim(&ep("human"), session("a"), "human-client", epoch("e1"))
            .expect("claims");
        r.claim(&ep("claude"), session("b"), "claude-channel", epoch("e2"))
            .expect("claims");
        let profile = trusting(&[P1, P2]);
        assert_eq!(names(&r.advertised_for(&peer(P1), &profile, 32)), ["human"]);
        assert_eq!(
            names(&r.advertised_for(&peer(P2), &profile, 32)),
            ["claude", "human"]
        );
    }

    #[test]
    fn an_untrusted_querier_is_shown_nothing() {
        // Narrowing never widens: an endpoint's inbound policy inheriting
        // profile trust admits nobody the profile refused.
        let mut endpoints = BTreeMap::new();
        endpoints.insert(ep("human"), advertised());
        let mut r = EndpointRegistry::new(endpoints, None);
        r.claim(&ep("human"), session("a"), "human-client", epoch("e1"))
            .expect("claims");
        assert!(r.advertised_for(&peer(P2), &trusting(&[P1]), 32).is_empty());
    }

    #[test]
    fn the_list_is_sorted_regardless_of_claim_order() {
        let mut endpoints = BTreeMap::new();
        for name in ["zeta", "alpha", "mid"] {
            endpoints.insert(ep(name), advertised());
        }
        let mut r = EndpointRegistry::new(endpoints, None);
        for (i, name) in ["zeta", "mid", "alpha"].iter().enumerate() {
            r.claim(
                &ep(name),
                session(name),
                "human-client",
                epoch(&format!("e{i}")),
            )
            .expect("claims");
        }
        assert_eq!(
            names(&r.advertised_for(&peer(P1), &trusting(&[P1]), 32)),
            ["alpha", "mid", "zeta"]
        );
    }

    #[test]
    fn more_than_the_cap_is_cut_not_refused() {
        let mut endpoints = BTreeMap::new();
        for i in 0..5 {
            endpoints.insert(ep(&format!("e{i}")), advertised());
        }
        let mut r = EndpointRegistry::new(endpoints, None);
        for i in 0..5 {
            r.claim(
                &ep(&format!("e{i}")),
                session(&format!("s{i}")),
                "human-client",
                epoch(&format!("g{i}")),
            )
            .expect("claims");
        }
        assert_eq!(
            names(&r.advertised_for(&peer(P1), &trusting(&[P1]), 3)),
            ["e0", "e1", "e2"]
        );
    }

    #[test]
    fn the_cap_never_exceeds_the_wire_bound() {
        let mut endpoints = BTreeMap::new();
        for i in 0..40 {
            endpoints.insert(ep(&format!("e{i:02}")), advertised());
        }
        let mut r = EndpointRegistry::new(endpoints, None);
        for i in 0..40 {
            r.claim(
                &ep(&format!("e{i:02}")),
                session(&format!("s{i}")),
                "human-client",
                epoch(&format!("g{i}")),
            )
            .expect("claims");
        }
        // A caller asking for more than the wire carries gets the wire.
        assert_eq!(
            r.advertised_for(&peer(P1), &trusting(&[P1]), 1000).len(),
            MAX_DIRECTORY_ENTRIES
        );
    }
}
