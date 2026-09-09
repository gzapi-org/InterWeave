// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Direct-inbound reachability evidence (AutoNAT v2 client policy).
//!
//! `AUTONAT.md` §2 splits the client in two: the Swarm owns the libp2p
//! behaviour, and a `ReachabilityManager` owns POLICY and EVIDENCE. This
//! is that manager. It decides which servers this profile DIALS and which
//! addresses count as its own, folds the probe results the behaviour
//! reports into evidence, and derives the [`ReachabilityVerdict`] that
//! `contracts/CONNECTIVITY.md` §5 and the `connectivity-summary` schema
//! describe. It opens no socket and reads no clock: every method that can
//! expire anything takes `now_ms`, so the transitions are testable by
//! enumeration.
//!
//! # What it does NOT do, and where each of those things lives instead
//!
//! `AUTONAT.md`'s Amendment 2026-09-09 settled that the pinned client
//! (`libp2p-autonat` 0.15.0) chooses its own probes: which of its
//! candidate addresses to test, which of the servers this profile DIALLED
//! to ask, and when. It announces no probe start and exposes no hook to
//! rank or veto a server. An earlier version of this module planned
//! probes anyway -- per-pair in-flight accounting with a timeout that
//! synthesised a failure -- and so could time out a probe the crate had
//! never issued and back a server off for it. Review finding on PR #84.
//! That half is gone, and its concerns are held where the lever is:
//!
//! - **The tick and the per-cycle candidate ceiling** are the crate's own
//!   two knobs, `with_probe_interval` and `with_max_candidates`; the
//!   adapter sets them from `refresh_interval` and
//!   `max_candidate_addresses_per_cycle`. The interval is NOT a refresh:
//!   the tick sweeps only candidates the crate has never tested
//!   (`v2/client/behaviour.rs:319-321`), and a tested candidate is never
//!   swept again. Until ADR-0051's `retest` lands, which address is
//!   re-probed and when is a decision nothing can yet act on; once it
//!   lands, that decision is this manager's, keyed on the evidence below.
//! - **Backoff for a server that will not CONNECT** is the dial gate's:
//!   `ConnectionManager::retry_delay_ms` is already `AUTONAT.md` §4's
//!   "30 s, bounded exponential, 5 min", and `ConnectionPolicy` scopes it
//!   to the address before the peer. A static server is reached through
//!   `attempt_dial`, so it inherits that for free. A server that connects
//!   and then never ANSWERS is a different case: the crate maps a stream
//!   timeout to `Io`, resets the candidate and re-issues on the next tick
//!   (`behaviour.rs:223`), and no gate sees it. ADR-0051 closes that by
//!   handing the retry decision here. Review finding on PR #84.
//! - **The inbound dial-back** is retained by the adapter on the basis
//!   of which servers IT dialled, not of anything recorded here.
//!
//! # Evidence is keyed by `(address, server)`, and only DISTINCT servers count
//!
//! `AUTONAT.md` §4: "Do not count repeated probes from one server as
//! distinct observers." A server that answers twice is one observer, and
//! `verified_public` needs `required_distinct_successes` of them for ONE
//! address with fresh evidence. The key makes that structural rather than
//! a check: a second success from the same server overwrites the first.
//!
//! # A server speaks once, and one contradiction is not an invalidation
//!
//! Each server counts with its LATEST word on an address: a fresh failure
//! makes it an observer saying unreachable, otherwise a fresh success
//! makes it one saying reachable, never both. `AUTONAT.md` §5 then gives
//! the hysteresis: "Two fresh independent failures may invalidate a
//! previously verified address before TTL." So an address that WAS
//! verified stays verified while the servers that said reachable -- still
//! saying it, or REVERSED, a fresh success now under a fresh failure --
//! still make the threshold, at least one still says it, and fewer than
//! two in total say unreachable. The verdict then ends when the
//! threshold-th newest of those successes lapses, which is what
//! `evidence_until_ms` reports. A DISSENTER that never said reachable
//! counts toward that two and toward nothing else. Three earlier versions
//! each missed a piece: one let a failure overwrite the success it
//! contradicted, so one report from a counting observer unverified the
//! address; the next counted the server in both sets, so at a threshold
//! of one the address stayed verified for a full TTL while its only
//! observer said unreachable; the third let a dissenter make up the
//! quorum, so de-authorising a verifying server left the verdict standing
//! on the word of the server calling the address unreachable. Review
//! findings on PR #84. Expiry is not a contradiction: a success that ages
//! out leaves the observer silent and out of the quorum, and a verdict
//! short of its threshold lapses with it -- §5's "must not survive beyond
//! its evidence TTL".
//!
//! # A private address is never a candidate
//!
//! `AUTONAT.md` §6: "Private/LAN addresses are never promoted to
//! Internet-public solely because they were configured or echoed by a
//! peer." This module refuses them one step earlier -- they are not
//! COUNTED -- because sending a server a loopback or RFC 1918 address is
//! the SSRF-shaped request `AUTONAT.md` §7 makes the server refuse, and a
//! client that never sends one cannot be the reason a server had to.
//! [`is_probeable_address`] is the rule, and it is literal-IP only.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr};

use interweave_transport_api::{DirectInboundState, TransportIdentity};

/// `AUTONAT.md` §4 default, the one the schema pins as a default too.
pub const DEFAULT_REQUIRED_DISTINCT_SUCCESSES: u32 = 2;
/// Evidence lifetime: 15 minutes.
pub const DEFAULT_SUCCESS_EVIDENCE_TTL_MS: u64 = 15 * 60 * 1000;
/// How many probeable addresses the manager will TRACK.
///
/// Bounds the address dimension of the evidence map, which the address
/// registry is not this module's to fill unchecked. It equals the libp2p
/// crate's DEFAULT `max_active_listeners`, 64 -- a smaller value silently
/// dropped listeners a profile was allowed to bind. That crate depends on
/// this one and not the reverse, so the two numbers are pinned together
/// THERE, by `SubstrateConfig`'s test against this constant; the test in
/// this file pins only this side. An operator who raises the listener
/// ceiling above the default still overflows, and
/// [`ReachabilityManager::truncated_candidates`] is what says so. Review
/// findings on PR #84.
pub const MAX_TRACKED_CANDIDATES: usize = 64;

/// The policy knobs this manager owns.
///
/// `AUTONAT.md` §4 names more; see the module note for where the others
/// are honoured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachabilityConfig {
    /// Distinct authorized servers that must agree before an address is
    /// verified.
    pub required_distinct_successes: u32,
    /// How long one observation stands -- a success, and a failure
    /// alike. The schema names it for successes; a failure is held fresh
    /// for the same span because the two are weighed against each other,
    /// and giving failures a shorter life was what let a success outlive
    /// the contradiction that should have counted against it.
    pub success_evidence_ttl_ms: u64,
}

impl Default for ReachabilityConfig {
    fn default() -> Self {
        Self {
            required_distinct_successes: DEFAULT_REQUIRED_DISTINCT_SUCCESSES,
            success_evidence_ttl_ms: DEFAULT_SUCCESS_EVIDENCE_TTL_MS,
        }
    }
}

/// Why a [`ReachabilityConfig`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReachabilityError {
    /// A zero where the policy needs at least one: the field is named.
    ZeroBound(&'static str),
}

impl std::fmt::Display for ReachabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroBound(field) => write!(f, "reachability: `{field}` must be at least 1"),
        }
    }
}

impl std::error::Error for ReachabilityError {}

/// `contracts/CONNECTIVITY.md` §5's state, with the evidence behind it.
///
/// NOT the wire type. `interweave_transport_api::DirectInboundState` is
/// the neutral three-word enum the `connectivity-summary` schema pins
/// (`transport-api/tests/schema_agreement.rs`); this carries the same
/// three answers plus what they rest on, and [`ReachabilityVerdict::
/// state`] is the one place the two are mapped. An earlier version of
/// this module declared its own `DirectInboundState` with a hand-written
/// `summary_word` returning string literals, which is a second copy of a
/// normative vocabulary with nothing comparing them -- CLAUDE.md §7's
/// "avoid duplicating normative constants … unless there is a drift
/// check". Review finding on PR #84.
///
/// `NotVerified` deliberately claims no NAT type: it means the current
/// evidence does not verify direct inbound reachability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReachabilityVerdict {
    /// Startup, or insufficient or indeterminate evidence.
    Unknown,
    /// At least one address has fresh successes from enough distinct
    /// servers.
    VerifiedPublic {
        /// Every address currently meeting the threshold.
        verified_addresses: Vec<String>,
        /// When the verdict ends if no further evidence arrives: the
        /// expiry of the threshold-th newest fresh success for the
        /// address, since below that the quorum is short.
        ///
        /// Contradicted members count, because their successes are what
        /// hold the count up under the hysteresis. Two earlier versions
        /// were wrong in opposite directions: one took the earliest
        /// expiry of any counted success, which understates whenever more
        /// than the threshold agree; the next kept that after a reversed
        /// server's success had become load-bearing, which overstated by
        /// up to a whole TTL -- publishing a horizon while the verdict
        /// was about to end. Review findings on PR #84.
        evidence_until_ms: u64,
    },
    /// Evidence is sufficient to say the threshold is not met.
    NotVerified {
        /// The most recent fresh failure, if any.
        last_failure_at_ms: Option<u64>,
    },
}

impl ReachabilityVerdict {
    /// The neutral state this verdict reports, for
    /// `connectivity-summary`.
    ///
    /// The schema words come from `DirectInboundState`'s own serde,
    /// which `transport-api`'s schema-agreement test pins against the
    /// contract -- so this crate carries no copy of them.
    #[must_use]
    pub const fn state(&self) -> DirectInboundState {
        match self {
            Self::Unknown => DirectInboundState::Unknown,
            Self::VerifiedPublic { .. } => DirectInboundState::VerifiedPublic,
            Self::NotVerified { .. } => DirectInboundState::NotVerified,
        }
    }

    /// The addresses ADR-0035's advertisement rule reads; empty unless
    /// verified.
    #[must_use]
    pub fn verified_addresses(&self) -> &[String] {
        match self {
            Self::VerifiedPublic {
                verified_addresses, ..
            } => verified_addresses,
            Self::Unknown | Self::NotVerified { .. } => &[],
        }
    }
}

/// What one probe told us.
///
/// As of `libp2p-autonat-0.15.0` the client emits `Event` for two results
/// (`v2/client/behaviour.rs:200-243`): `Ok(())`, and
/// `Err(AddressNotReachable { .. })` after the server tried and failed to
/// dial back. `Ok(())` is [`Reachable`](Self::Reachable) and
/// `Err(AddressNotReachable)` is [`Unreachable`](Self::Unreachable). Other
/// paths produce NO event: `UnsupportedProtocol` and `Io` reset the
/// candidate and return, a server reporting success when no dial-back
/// was received (`behaviour.rs:186-198`) leaves the candidate `Pending`,
/// and so does a request the handler dropped for want of a slot
/// (`dial_request.rs:99-113`) -- so an adapter must not wait on an
/// outcome for every probe, and ADR-0051's `retest` is how a stuck one is
/// recovered. This crate does not depend on the AutoNAT crate, so the
/// adapter's `match` on `Event.result` is where a third result would
/// surface, not here. An earlier version carried a `Failed` for a probe
/// that never came back; no such probe can be observed, so it is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The server dialled the address back and reached us.
    Reachable,
    /// The server tried and did not reach us. An ADDRESS result from a
    /// server that answered correctly.
    Unreachable,
}

/// Where a server came from.
///
/// The ORDER is the dialling preference `AUTONAT.md`'s Amendment
/// 2026-09-09 leaves static configuration -- which servers this profile
/// guarantees to dial -- and not the §3 selection precedence that
/// amendment removed, which the pinned client cannot express.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ServerSource {
    /// Configured in `autonat.client.static_servers`.
    Static,
    /// Learned through an authorized Identify exchange; only offered when
    /// the operator opted in, which is the caller's decision, not this
    /// module's.
    Identify,
}

/// Emitted when the derived state changes.
///
/// A CHANGE is a change of the normalized state or of the verified
/// address set -- the two things a reader acts on. A timestamp moving
/// under the same verdict is not one: an earlier version compared whole
/// verdicts, so a later failure while already `NotVerified` emitted a
/// `ConnectivityChanged` whose normalized state had not changed, against
/// `lifecycle.md`'s "whenever the normalized state changes". Review
/// finding on PR #84.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectivityChanged {
    /// The state before.
    pub from: ReachabilityVerdict,
    /// The state now.
    pub to: ReachabilityVerdict,
}

/// One server's word on one address. A success clears the failure it
/// supersedes, so a present `failure_at_ms` is always the LATEST word;
/// `success_at_ms` survives a later failure so `derive` can tell a
/// REVERSED observer -- one that said the address was reachable and now
/// contradicts itself -- from a dissenter that never said reachable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Evidence {
    success_at_ms: Option<u64>,
    failure_at_ms: Option<u64>,
}

impl Evidence {
    fn fresh_success(&self, ttl_ms: u64, now_ms: u64) -> Option<u64> {
        self.success_at_ms
            .filter(|at| at.saturating_add(ttl_ms) > now_ms)
    }

    fn fresh_failure(&self, ttl_ms: u64, now_ms: u64) -> Option<u64> {
        self.failure_at_ms
            .filter(|at| at.saturating_add(ttl_ms) > now_ms)
    }
}

/// The manager. See the module note for what it owns and what it refuses.
#[derive(Debug, Clone)]
pub struct ReachabilityManager {
    config: ReachabilityConfig,
    candidates: Vec<String>,
    rejected_candidates: usize,
    truncated_candidates: usize,
    servers: BTreeMap<TransportIdentity, ServerSource>,
    evidence: BTreeMap<(String, TransportIdentity), Evidence>,
    state: ReachabilityVerdict,
}

impl ReachabilityManager {
    /// Build a manager, refusing a configuration whose bounds are zero.
    ///
    /// The configuration crate refuses the same values on the way in; this
    /// refuses them for a caller that never deserialized anything. An
    /// earlier version checked some fields and documented all of them
    /// as checked. Review finding on PR #84.
    pub fn new(config: ReachabilityConfig) -> Result<Self, ReachabilityError> {
        if config.required_distinct_successes == 0 {
            return Err(ReachabilityError::ZeroBound("required_distinct_successes"));
        }
        if config.success_evidence_ttl_ms == 0 {
            return Err(ReachabilityError::ZeroBound("success_evidence_ttl_ms"));
        }
        Ok(Self {
            config,
            candidates: Vec::new(),
            rejected_candidates: 0,
            truncated_candidates: 0,
            servers: BTreeMap::new(),
            evidence: BTreeMap::new(),
            state: ReachabilityVerdict::Unknown,
        })
    }

    /// The current state.
    #[must_use]
    pub const fn state(&self) -> &ReachabilityVerdict {
        &self.state
    }

    /// Replace the candidate addresses, keeping only those a probe server
    /// may legitimately be asked to dial.
    ///
    /// Returns the state change, like the other mutators that can cause
    /// one, so a caller can honour `lifecycle.md`'s "emit
    /// `ConnectivityChanged` whenever the normalized state changes"
    /// without snapshotting around the call: withdrawing a verified
    /// listener ends the verdict, and that edge is one an advertisement
    /// must be withdrawn on. How many addresses were refused is
    /// [`rejected_candidates`](Self::rejected_candidates), and how many
    /// probeable ones did not fit is
    /// [`truncated_candidates`](Self::truncated_candidates), so a caller
    /// can still say why a LAN-only node never reaches `verified_public`
    /// and why a sixty-fifth listener is never verified. Review findings
    /// on PR #84.
    ///
    /// EVIDENCE FOR A WITHDRAWN ADDRESS IS DROPPED: keeping it left the
    /// map unbounded in the address dimension while `candidates` is
    /// bounded, and re-adding an address inside the TTL would have
    /// re-verified it from evidence gathered before it was withdrawn --
    /// silently, and without a probe. RE-DERIVES, because dropping a
    /// candidate can end a verdict. Without it `state()` would keep
    /// naming a withdrawn address in `verified_addresses` until the next
    /// expiry pass, and ADR-0035's advertisement rule reads exactly that
    /// list. Review finding on PR #84.
    pub fn set_candidates<I, S>(&mut self, addresses: I, now_ms: u64) -> Option<ConnectivityChanged>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut accepted = Vec::new();
        let mut seen = BTreeSet::new();
        let mut rejected = 0;
        let mut truncated = 0;
        for address in addresses {
            let address = address.as_ref();
            if !is_probeable_address(address) {
                rejected += 1;
                continue;
            }
            if !seen.insert(address.to_owned()) {
                continue;
            }
            if accepted.len() < MAX_TRACKED_CANDIDATES {
                accepted.push(address.to_owned());
            } else {
                truncated += 1;
            }
        }
        self.rejected_candidates = rejected;
        self.truncated_candidates = truncated;
        self.candidates = accepted;
        self.evidence
            .retain(|(address, _), _| self.candidates.contains(address));
        self.rederive(now_ms)
    }

    /// Candidates refused by the last [`set_candidates`](Self::set_candidates)
    /// because [`is_probeable_address`] said no.
    #[must_use]
    pub const fn rejected_candidates(&self) -> usize {
        self.rejected_candidates
    }

    /// Probeable, distinct candidates the last
    /// [`set_candidates`](Self::set_candidates) had no room for.
    #[must_use]
    pub const fn truncated_candidates(&self) -> usize {
        self.truncated_candidates
    }

    /// The addresses whose evidence counts.
    #[must_use]
    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    /// Offer a server. Eligibility by class and by opt-in is the CALLER's
    /// to decide before calling this (`AUTONAT.md` §3); this module only
    /// remembers where it came from.
    ///
    /// A KNOWN SERVER TAKES THE STRONGER SOURCE: `or_insert` left a peer
    /// first seen through Identify recorded as `Identify` when it was
    /// later added as a configured one, which under the 2026-09-09
    /// amendment is a silent downgrade of the dialling guarantee static
    /// configuration buys. Review finding on PR #84.
    pub fn add_server(&mut self, server: TransportIdentity, source: ServerSource) {
        let slot = self.servers.entry(server).or_insert(source);
        if source < *slot {
            *slot = source;
        }
    }

    /// Withdraw a server and every observation it contributed.
    pub fn remove_server(
        &mut self,
        server: &TransportIdentity,
        now_ms: u64,
    ) -> Option<ConnectivityChanged> {
        self.servers.remove(server);
        self.evidence.retain(|(_, s), _| s != server);
        self.rederive(now_ms)
    }

    /// Whether this peer is a server whose reports count.
    #[must_use]
    pub fn is_server(&self, peer: &TransportIdentity) -> bool {
        self.servers.contains_key(peer)
    }

    /// The servers this profile should be dialling, static ones first.
    ///
    /// This is the one lever the amendment leaves: the client asks only
    /// peers this profile DIALLED, so which peers those are is the whole
    /// of server selection. Within a source the order is by identity, so
    /// two calls agree.
    #[must_use]
    pub fn dial_order(&self) -> Vec<TransportIdentity> {
        let mut servers: Vec<(&ServerSource, &TransportIdentity)> = self
            .servers
            .iter()
            .map(|(id, source)| (source, id))
            .collect();
        servers.sort();
        servers.into_iter().map(|(_, id)| id.clone()).collect()
    }

    /// Fold a probe's result in. Returns the state change, if any.
    ///
    /// Two conditions drop it whole and return `None`, because nothing
    /// is mutated. A SERVER WE DO NOT KNOW: a stranger's report must not
    /// reach `verified_public`. AN ADDRESS WE DO NOT TRACK: `derive`
    /// reads only candidates, so it could never count and would only
    /// grow the map -- and that set is remote-influenced, since Identify
    /// pushes every address a peer CLAIMS to have observed into the set
    /// the pinned client probes. The untracked case is the COMMON one,
    /// not an edge: `AUTONAT.md` §3's open note records exactly this.
    pub fn record_outcome(
        &mut self,
        address: &str,
        server: &TransportIdentity,
        outcome: ProbeOutcome,
        now_ms: u64,
    ) -> Option<ConnectivityChanged> {
        if !self.servers.contains_key(server) {
            return None;
        }
        if !self.candidates.iter().any(|candidate| candidate == address) {
            return None;
        }
        let entry = self
            .evidence
            .entry((address.to_owned(), server.clone()))
            .or_default();
        match outcome {
            // THE NEWER WORD ON THE SAME QUESTION: a success clears the
            // failure it supersedes. A failure leaves the success in
            // place, and `derive` reads the pair as one contradicting
            // observer, not as one of each -- see the module note.
            ProbeOutcome::Reachable => {
                entry.success_at_ms = Some(now_ms);
                entry.failure_at_ms = None;
            }
            ProbeOutcome::Unreachable => {
                entry.failure_at_ms = Some(now_ms);
            }
        }
        self.rederive(now_ms)
    }

    /// Drop expired evidence and re-derive the state.
    pub fn expire_evidence(&mut self, now_ms: u64) -> Option<ConnectivityChanged> {
        self.rederive(now_ms)
    }

    /// The network changed: every observation is stale (`AUTONAT.md` §5,
    /// "startup/network change -> unknown").
    pub fn network_changed(&mut self) -> Option<ConnectivityChanged> {
        let before = self.state.clone();
        self.evidence.clear();
        self.state = ReachabilityVerdict::Unknown;
        Self::change(before, &self.state)
    }

    fn rederive(&mut self, now_ms: u64) -> Option<ConnectivityChanged> {
        let ttl = self.config.success_evidence_ttl_ms;
        self.evidence.retain(|_, e| {
            e.fresh_success(ttl, now_ms).is_some() || e.fresh_failure(ttl, now_ms).is_some()
        });
        let before = self.state.clone();
        self.state = self.derive(now_ms);
        Self::change(before, &self.state)
    }

    fn change(
        before: ReachabilityVerdict,
        after: &ReachabilityVerdict,
    ) -> Option<ConnectivityChanged> {
        if before.state() == after.state()
            && before.verified_addresses() == after.verified_addresses()
        {
            None
        } else {
            Some(ConnectivityChanged {
                from: before,
                to: after.clone(),
            })
        }
    }

    /// `AUTONAT.md` §5, computed from the fresh evidence and the verdict
    /// it is replacing.
    ///
    /// For each candidate, each server with fresh evidence speaks once: a
    /// fresh failure makes it an observer saying unreachable, else a fresh
    /// success makes it one saying reachable. A server whose fresh success
    /// sits under a fresh failure has REVERSED -- it said the address was
    /// reachable and now contradicts itself -- and that is the one place
    /// the retained success is read. It need not have been in the quorum
    /// that first verified the address; §4 says "the servers that said
    /// reachable", which is what this counts. An address is verified when the reachable count
    /// meets the threshold, OR it was verified before and the servers that
    /// said reachable -- still saying it, or reversed -- still make the
    /// threshold with at least one still saying it; in either case fewer
    /// than two say unreachable. A dissenter that never said reachable
    /// counts toward that two and toward nothing else: an earlier version
    /// let it make up the quorum, so removing a verifying server left the
    /// verdict standing on the word of the server calling the address
    /// unreachable. Review finding on PR #84. If any address is verified,
    /// `VerifiedPublic`; else if any fresh failure exists, `NotVerified`;
    /// else `Unknown` -- successes short of the threshold are
    /// "insufficient evidence", which §4 and `CONNECTIVITY.md` §5 both
    /// give to `unknown`.
    fn derive(&self, now_ms: u64) -> ReachabilityVerdict {
        let ttl = self.config.success_evidence_ttl_ms;
        let threshold =
            usize::try_from(self.config.required_distinct_successes).unwrap_or(usize::MAX);
        let previously_verified = self.state.verified_addresses();
        let mut verified = Vec::new();
        let mut evidence_until = u64::MAX;
        let mut any_failure = false;
        let mut last_failure: Option<u64> = None;
        for address in &self.candidates {
            let mut saying_reachable: BTreeSet<&TransportIdentity> = BTreeSet::new();
            let mut reversed: BTreeSet<&TransportIdentity> = BTreeSet::new();
            let mut saying_unreachable: BTreeSet<&TransportIdentity> = BTreeSet::new();
            // Every fresh success's expiry, contradicted members
            // included: a reversed server's success is what holds the
            // count at the threshold when the hold applies, so it ends
            // the verdict when it lapses.
            let mut quorum_expiries: Vec<u64> = Vec::new();
            for ((a, server), e) in &self.evidence {
                if a != address {
                    continue;
                }
                let success = e.fresh_success(ttl, now_ms);
                if let Some(at) = success {
                    quorum_expiries.push(at.saturating_add(ttl));
                }
                match e.fresh_failure(ttl, now_ms) {
                    Some(at) => {
                        any_failure = true;
                        saying_unreachable.insert(server);
                        last_failure = Some(last_failure.map_or(at, |f| f.max(at)));
                        if success.is_some() {
                            reversed.insert(server);
                        }
                    }
                    None => {
                        if success.is_some() {
                            saying_reachable.insert(server);
                        }
                    }
                }
            }
            let meets_threshold = saying_reachable.len() >= threshold;
            // THE HOLD: the observers that verified it, one of them now
            // disagreeing. A success that aged out is silence and leaves
            // the quorum; a failure from a server that never said
            // reachable was never part of it.
            let holds = previously_verified.iter().any(|v| v == address)
                && !saying_reachable.is_empty()
                && saying_reachable.len() + reversed.len() >= threshold;
            if (meets_threshold || holds) && saying_unreachable.len() < 2 {
                verified.push(address.clone());
                // THE THRESHOLD-TH NEWEST, not the earliest: below it the
                // quorum is short, and above it the members that remain
                // still meet the threshold. Taking the earliest
                // understated the horizon whenever more than the
                // threshold agreed; taking it from the uncontradicted
                // members alone OVERSTATED it by up to a whole TTL once a
                // reversed server's success had become load-bearing.
                // Review findings on PR #84.
                quorum_expiries.sort_unstable();
                let nth = threshold.min(quorum_expiries.len());
                evidence_until = evidence_until.min(quorum_expiries[quorum_expiries.len() - nth]);
            }
        }
        if !verified.is_empty() {
            ReachabilityVerdict::VerifiedPublic {
                verified_addresses: verified,
                evidence_until_ms: evidence_until,
            }
        } else if any_failure {
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: last_failure,
            }
        } else {
            ReachabilityVerdict::Unknown
        }
    }
}

/// Whether a probe server may legitimately be asked to dial this address.
///
/// LITERAL IP ONLY, and Internet-public only: the first component must be
/// `/ip4/` or `/ip6/` with a parseable literal, and that literal must not
/// be loopback, unspecified, private (RFC 1918), shared (RFC 6598),
/// link-local, unique-local, multicast, broadcast, documentation,
/// benchmarking, discard-only, ORCHID or NAT64 space. A `/dns4/`
/// candidate is refused -- the server would resolve it, which is a
/// request on the server's behalf rather than a test of our address.
#[must_use]
pub fn is_probeable_address(address: &str) -> bool {
    let mut parts = address.split('/');
    if parts.next() != Some("") {
        return false;
    }
    // NOT A RELAYED ADDRESS, and this is checked over the WHOLE string
    // rather than the first component. `/ip4/<relay>/tcp/4001/p2p/<relay
    // id>/p2p-circuit/p2p/<us>` begins `/ip4/` with a public literal, so
    // reading only the first component accepted it -- and asking a probe
    // server to dial it tests the RELAY's reachability, putting a
    // relay-derived address into `verified_addresses`. `AUTONAT.md` §6
    // keeps "AutoNAT-verified direct addresses" and "active
    // relay-derived addresses" as separate registry classes precisely so
    // that cannot happen, and Identify reports exactly this shape as the
    // observed address on a relayed connection. Review finding on
    // PR #84.
    if address
        .split('/')
        .any(|component| component == "p2p-circuit")
    {
        return false;
    }
    match (parts.next(), parts.next()) {
        (Some("ip4"), Some(literal)) => literal.parse::<Ipv4Addr>().is_ok_and(is_public_v4),
        (Some("ip6"), Some(literal)) => literal.parse::<Ipv6Addr>().is_ok_and(is_public_v6),
        _ => false,
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let shared = a == 100 && (64..=127).contains(&b);
    let benchmarking = a == 198 && (b == 18 || b == 19);
    let reserved = a >= 240;
    // `is_unspecified` is the single address 0.0.0.0; the whole 0.0.0.0/8
    // is "this network" and none of it is a destination.
    let this_network = a == 0;
    // RFC 6890's IETF protocol assignments, and RFC 7526's deprecated
    // 6to4 relay anycast -- both routable-looking and neither ours.
    let protocol_assignments = a == 192 && b == 0 && c == 0;
    let six_to_four_relay = a == 192 && b == 88 && c == 99;
    !(ip.is_loopback()
        || ip.is_unspecified()
        || this_network
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || shared
        || benchmarking
        || protocol_assignments
        || six_to_four_relay
        || reserved)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    let unique_local = (segments[0] & 0xfe00) == 0xfc00;
    let link_local = (segments[0] & 0xffc0) == 0xfe80;
    // Deprecated by RFC 3879 and still routed by some stacks; neither
    // `unique_local`'s nor `link_local`'s mask covers `fec0::/10`.
    let site_local = (segments[0] & 0xffc0) == 0xfec0;
    // RFC 3849's `2001:db8::/32` and RFC 9637's `3fff::/20`. An earlier
    // mask, `segments[0] & 0xfff0 == 0x3ff0`, was `3ff0::/12` -- 256 times
    // the prefix the comment named, refusing `3ff0::`-`3ffe::` with no
    // test on that axis. Review finding on PR #84.
    let documentation = (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x3fff && (segments[1] & 0xf000) == 0);
    let benchmarking = segments[0] == 0x2001 && segments[1] == 0x0002 && segments[2] == 0;
    // 6to4 and Teredo carry an embedded IPv4 address whose reachability
    // is the tunnel's, not ours.
    let six_to_four = segments[0] == 0x2002;
    let teredo = segments[0] == 0x2001 && segments[1] == 0;
    // RFC 6666's discard-only `100::/64`: a black hole that parses as
    // global unicast. RFC 7343's ORCHIDv2 `2001:20::/28`: identifiers,
    // not locators, and never routed. Review finding on PR #84.
    let discard_only = segments[0] == 0x0100 && segments[1..4].iter().all(|s| *s == 0);
    let orchid = segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0x0020;
    // NAT64's well-known `64:ff9b::/96` (RFC 6052) and local-use
    // `64:ff9b:1::/48` (RFC 8215): a v4 address wearing a v6 prefix,
    // whose reachability is the translator's.
    let nat64 = segments[0] == 0x0064
        && segments[1] == 0xff9b
        && (segments[2..6].iter().all(|s| *s == 0) || segments[2] == 1);
    // `to_ipv4_mapped` covers `::ffff:a.b.c.d` only, so the deprecated
    // IPv4-COMPATIBLE form `::a.b.c.d` needs its own arm -- it is
    // otherwise a public-looking address with a private v4 inside.
    let ipv4_compatible = segments[..6].iter().all(|s| *s == 0) && !ip.is_unspecified();
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || unique_local
        || link_local
        || site_local
        || documentation
        || benchmarking
        || six_to_four
        || teredo
        || discard_only
        || orchid
        || nat64
        || ipv4_compatible)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const S2: &str = "12D3KooWHyNGMf9HTd3Zj6dStdkcc5ycsubW1rEgQSp6k6yfZBoy";
    const S3: &str = "12D3KooWQYhTNQdmr3ArTeUHRYzFg94BKyTkoWBDWez9kSCVe2Xo";
    const S4: &str = "12D3KooWL8ZMFsFZwpZ3vGSg1b5RSgVzmMMpr1HdXLzqtoprdzZA";
    const A: &str = "/ip4/8.8.8.8/tcp/4001";
    const B: &str = "/ip4/1.1.1.1/tcp/4001";
    const TTL: u64 = DEFAULT_SUCCESS_EVIDENCE_TTL_MS;

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("a valid identity")
    }

    /// Two tracked public addresses and no servers.
    fn manager() -> ReachabilityManager {
        let mut m =
            ReachabilityManager::new(ReachabilityConfig::default()).expect("defaults are valid");
        assert!(m.set_candidates([A, B], 0).is_none());
        m
    }

    /// `manager()` plus the named static servers.
    fn manager_with(servers: &[&str]) -> ReachabilityManager {
        let mut m = manager();
        for s in servers {
            m.add_server(peer(s), ServerSource::Static);
        }
        m
    }

    fn verified(m: &ReachabilityManager) -> bool {
        matches!(m.state(), ReachabilityVerdict::VerifiedPublic { .. })
    }

    #[test]
    fn every_zero_bound_is_refused_for_a_caller_that_never_deserialized() {
        // EVERY field, not some: the previous version checked four of
        // seven and documented all seven as checked. The destructuring
        // below is exhaustive, so a field added to the config without a
        // row here fails to COMPILE rather than leaving a hand-kept list
        // silently short. Review finding on PR #84.
        let ReachabilityConfig {
            required_distinct_successes: _,
            success_evidence_ttl_ms: _,
        } = ReachabilityConfig::default();
        for (field, config) in [
            (
                "required_distinct_successes",
                ReachabilityConfig {
                    required_distinct_successes: 0,
                    ..ReachabilityConfig::default()
                },
            ),
            (
                "success_evidence_ttl_ms",
                ReachabilityConfig {
                    success_evidence_ttl_ms: 0,
                    ..ReachabilityConfig::default()
                },
            ),
        ] {
            assert_eq!(
                ReachabilityManager::new(config).err(),
                Some(ReachabilityError::ZeroBound(field)),
                "{field}"
            );
        }
        assert!(ReachabilityManager::new(ReachabilityConfig::default()).is_ok());
    }

    #[test]
    fn only_literal_public_ip_addresses_are_candidates() {
        let refused = [
            // Not literal, relayed, or malformed.
            "/dns4/example.invalid/tcp/4001",
            "/ip4/8.8.8.8/tcp/4001/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN/p2p-circuit",
            "/ip4/not-an-ip/tcp/4001",
            "ip4/8.8.8.8/tcp/4001",
            "",
            // v4 special-use.
            "/ip4/127.0.0.1/tcp/4001",
            "/ip4/0.0.0.0/tcp/4001",
            "/ip4/0.1.2.3/tcp/4001",
            "/ip4/10.0.0.1/tcp/4001",
            "/ip4/172.16.0.1/tcp/4001",
            "/ip4/192.168.1.1/tcp/4001",
            "/ip4/169.254.1.1/tcp/4001",
            "/ip4/224.0.0.1/tcp/4001",
            "/ip4/255.255.255.255/tcp/4001",
            "/ip4/192.0.2.1/tcp/4001",
            "/ip4/198.51.100.9/tcp/4001",
            "/ip4/203.0.113.7/tcp/4001",
            "/ip4/100.64.0.1/tcp/4001",
            "/ip4/198.18.0.1/tcp/4001",
            "/ip4/192.0.0.1/tcp/4001",
            "/ip4/192.88.99.1/tcp/4001",
            "/ip4/240.0.0.1/tcp/4001",
            // v6 special-use.
            "/ip6/::1/tcp/4001",
            "/ip6/::/tcp/4001",
            "/ip6/ff02::1/tcp/4001",
            "/ip6/fc00::1/tcp/4001",
            "/ip6/fd12::1/tcp/4001",
            "/ip6/fe80::1/tcp/4001",
            "/ip6/fec0::1/tcp/4001",
            "/ip6/2001:db8::1/tcp/4001",
            "/ip6/3fff::1/tcp/4001",
            "/ip6/3fff:fff::1/tcp/4001",
            "/ip6/2001:2::1/tcp/4001",
            "/ip6/2002:c000:0204::1/tcp/4001",
            "/ip6/2001::1/tcp/4001",
            "/ip6/100::1/tcp/4001",
            "/ip6/2001:20::1/tcp/4001",
            "/ip6/2001:2f::1/tcp/4001",
            "/ip6/64:ff9b::808:808/tcp/4001",
            "/ip6/64:ff9b:1::808:808/tcp/4001",
            "/ip6/::ffff:10.0.0.1/tcp/4001",
            "/ip6/::10.0.0.1/tcp/4001",
        ];
        for address in refused {
            assert!(!is_probeable_address(address), "{address} must be refused");
        }
        let accepted = [
            A,
            B,
            "/ip6/2606:4700:4700::1111/tcp/4001",
            // Outside `3fff::/20` on both sides of the old `/12` mask.
            "/ip6/3ffe::1/tcp/4001",
            "/ip6/3fff:1000::1/tcp/4001",
            "/ip6/2001:30::1/tcp/4001",
            "/ip6/101::1/tcp/4001",
            "/ip6/64:ff9b:2::1/tcp/4001",
            "/ip6/::ffff:8.8.8.8/tcp/4001",
        ];
        for address in accepted {
            assert!(is_probeable_address(address), "{address} must be accepted");
        }
        // And the manager applies the rule: the refusals are counted,
        // not silently dropped.
        let mut m = manager();
        let _ = m.set_candidates(["/ip4/10.0.0.1/tcp/4001", A, "/dns4/x.invalid/tcp/1"], 0);
        assert_eq!(m.candidates(), [A]);
        assert_eq!(m.rejected_candidates(), 2);
        assert_eq!(m.truncated_candidates(), 0);
    }

    #[test]
    fn the_tracked_list_is_bounded_and_deduplicated_and_overflow_is_reported() {
        let mut m = manager();
        // 1.0.0.2 … 1.0.0.71 are public; offer each twice.
        let many: Vec<String> = (1..=MAX_TRACKED_CANDIDATES + 6)
            .flat_map(|i| {
                let a = format!("/ip4/1.0.{}.{}/tcp/4001", i / 250, 1 + i % 250);
                [a.clone(), a]
            })
            .collect();
        let _ = m.set_candidates(&many, 0);
        assert_eq!(m.candidates().len(), MAX_TRACKED_CANDIDATES, "bounded");
        assert_eq!(
            m.truncated_candidates(),
            6,
            "the overflow is reported, not swallowed"
        );
        assert_eq!(
            m.rejected_candidates(),
            0,
            "a duplicate is neither refused nor truncated"
        );
        let distinct: BTreeSet<&String> = m.candidates().iter().collect();
        assert_eq!(distinct.len(), MAX_TRACKED_CANDIDATES, "deduplicated");
    }

    #[test]
    fn the_tracking_bound_is_the_runtime_listener_ceiling() {
        // THIS SIDE ONLY. The libp2p crate's `max_active_listeners`
        // default cannot be named from here, so a change to IT is caught
        // by `SubstrateConfig`'s test in that crate, which asserts its
        // default against this constant. An earlier comment claimed this
        // test pinned both numbers; it never could. Review finding on
        // PR #84.
        assert_eq!(MAX_TRACKED_CANDIDATES, 64);
    }

    #[test]
    fn static_servers_are_dialled_before_identify_learned_ones() {
        let mut m = manager();
        m.add_server(peer(S3), ServerSource::Identify);
        m.add_server(peer(S1), ServerSource::Static);
        m.add_server(peer(S2), ServerSource::Identify);
        let order = m.dial_order();
        assert_eq!(order[0], peer(S1), "static first");
        assert_eq!(order.len(), 3);
        assert!(m.is_server(&peer(S2)));
        assert!(!m.is_server(&peer(S4)));
        // A static addition UPGRADES a server first seen through
        // Identify; an Identify sighting never downgrades a static one.
        m.add_server(peer(S2), ServerSource::Static);
        m.add_server(peer(S1), ServerSource::Identify);
        let order = m.dial_order();
        let mut both = [peer(S1), peer(S2)];
        both.sort();
        assert_eq!(&order[..2], &both[..], "both static, by identity");
        assert_eq!(order[2], peer(S3));
    }

    #[test]
    fn verified_needs_distinct_servers_and_one_server_twice_is_one_observer() {
        let mut m = manager_with(&[S1, S2]);
        // ONE SUCCESS IS `Unknown`: insufficient evidence, which §4 and
        // `CONNECTIVITY.md` §5 both give to that word and not to
        // `not_verified`. An earlier version said `NotVerified` here and
        // a test froze it. Review finding on PR #84.
        assert!(
            m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0)
                .is_none()
        );
        assert_eq!(*m.state(), ReachabilityVerdict::Unknown);
        // The same server again is still one observer.
        assert!(
            m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 1)
                .is_none()
        );
        assert!(!verified(&m), "one server twice is one observer");
        let change = m
            .record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 2)
            .expect("the threshold is crossed");
        assert_eq!(
            change.to,
            ReachabilityVerdict::VerifiedPublic {
                verified_addresses: vec![A.to_owned()],
                evidence_until_ms: 1 + TTL,
            }
        );
        assert_eq!(change.from, ReachabilityVerdict::Unknown);
    }

    #[test]
    fn reports_about_untracked_addresses_and_from_stranger_servers_are_dropped_whole() {
        let mut m = manager_with(&[S1, S2]);
        let before = m.clone();
        // A stranger: not a server at all.
        assert!(
            m.record_outcome(A, &peer(S3), ProbeOutcome::Reachable, 0)
                .is_none()
        );
        assert!(
            m.record_outcome(A, &peer(S3), ProbeOutcome::Unreachable, 0)
                .is_none()
        );
        // A known server about an address we do not track -- the common
        // case under the §3 open note, and also a LAN address a peer
        // claims to have observed.
        let untracked = "/ip4/9.9.9.9/tcp/4001";
        assert!(
            m.record_outcome(untracked, &peer(S1), ProbeOutcome::Reachable, 0)
                .is_none()
        );
        assert!(
            m.record_outcome(untracked, &peer(S2), ProbeOutcome::Reachable, 0)
                .is_none()
        );
        assert!(
            m.record_outcome(
                "/ip4/10.0.0.1/tcp/4001",
                &peer(S1),
                ProbeOutcome::Unreachable,
                0
            )
            .is_none()
        );
        assert_eq!(m.state(), before.state());
        assert_eq!(m.evidence, before.evidence, "nothing was recorded");
        assert_eq!(*m.state(), ReachabilityVerdict::Unknown);
    }

    #[test]
    fn verified_lapses_at_the_evidence_ttl_without_refresh() {
        let mut m = manager_with(&[S1, S2]);
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 10);
        assert!(verified(&m));
        assert!(m.expire_evidence(TTL - 1).is_none(), "still fresh");
        assert!(verified(&m));
        // EXPIRY IS NOT A CONTRADICTION, so the hold does not apply: S1
        // is silent, the threshold is no longer met, and with no failure
        // on record the verdict is `Unknown` at once -- §5's "must not
        // survive beyond its evidence TTL".
        let change = m.expire_evidence(TTL).expect("S1's success has lapsed");
        assert_eq!(change.to, ReachabilityVerdict::Unknown);
        assert!(
            m.expire_evidence(10 + TTL).is_none(),
            "S2 lapsing changes nothing further"
        );
    }

    #[test]
    fn two_fresh_independent_failures_invalidate_a_verified_address_before_ttl() {
        let mut m = manager_with(&[S1, S2, S3, S4]);
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 0);
        assert!(verified(&m));
        // One contradicting server is not enough.
        assert!(
            m.record_outcome(A, &peer(S3), ProbeOutcome::Unreachable, 5)
                .is_none()
        );
        assert!(
            verified(&m),
            "one failure does not invalidate two successes"
        );
        // The second one does -- while both successes are still fresh
        // and still counted.
        let _ = m.record_outcome(A, &peer(S4), ProbeOutcome::Unreachable, 6);
        assert_eq!(
            *m.state(),
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(6)
            },
            "two fresh independent failures invalidate before TTL"
        );
    }

    #[test]
    fn a_failure_from_a_counting_observer_does_not_erase_its_own_success() {
        // The case the four-server test above cannot reach: the
        // contradicting server is one whose success is being counted. An
        // earlier version keyed one observation per pair, so S1's failure
        // REPLACED S1's success and the address fell below the threshold
        // on one failure -- which §5 gives only to two. Review finding
        // on PR #84.
        let mut m = manager_with(&[S1, S2]);
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 0);
        assert!(verified(&m));
        assert!(
            m.record_outcome(A, &peer(S1), ProbeOutcome::Unreachable, 5)
                .is_none()
        );
        assert!(
            verified(&m),
            "S1's success still counts; its failure is one contradiction"
        );
        let change = m
            .record_outcome(A, &peer(S2), ProbeOutcome::Unreachable, 6)
            .expect("the second contradiction invalidates");
        assert_eq!(
            change.to,
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(6)
            }
        );
        // ONCE INVALIDATED, THE HOLD IS GONE: S2 saying reachable again
        // clears S2's contradiction but is one observer, and §5's
        // `not_verified + threshold fresh successes` means the threshold.
        assert!(
            m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 7)
                .is_none()
        );
        assert_eq!(
            *m.state(),
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(5)
            },
            "S1's contradiction still stands alone"
        );
        let change = m
            .record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 8)
            .expect("the threshold is met again");
        assert!(matches!(
            change.to,
            ReachabilityVerdict::VerifiedPublic { .. }
        ));
    }

    #[test]
    fn at_a_threshold_of_one_the_sole_observers_reversal_unverifies_at_once() {
        // The regression the two-sided entry introduced: with the server
        // counted in BOTH sets, one success and one later failure from
        // the same server kept the address verified for a full TTL while
        // its only observer said unreachable. Each server now speaks
        // once, with its latest word. Review finding on PR #84.
        let mut m = ReachabilityManager::new(ReachabilityConfig {
            required_distinct_successes: 1,
            ..ReachabilityConfig::default()
        })
        .expect("valid");
        let _ = m.set_candidates([A], 0);
        m.add_server(peer(S1), ServerSource::Static);
        assert!(
            m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0)
                .is_some()
        );
        assert!(verified(&m));
        let change = m
            .record_outcome(A, &peer(S1), ProbeOutcome::Unreachable, 1_000)
            .expect("the only observer reversed");
        assert_eq!(
            change.to,
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(1_000)
            }
        );
    }

    #[test]
    fn the_hold_needs_the_verifying_observers_still_fresh() {
        // Threshold two, S1 and S2 verify at t=0. S2 lapses at TTL; S1
        // then contradicts. One fresh word from one observer is not the
        // verified set minus a dissenter -- it is a verdict that already
        // lapsed -- so there is nothing to hold.
        let mut m = manager_with(&[S1, S2]);
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 10);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 0);
        assert!(verified(&m));
        assert_eq!(
            m.expire_evidence(TTL).map(|c| c.to),
            Some(ReachabilityVerdict::Unknown),
            "S2 lapsed, threshold unmet, no failure"
        );
        // And a later contradiction from S1 while S1's own success is
        // still fresh does not resurrect anything.
        let change = m
            .record_outcome(A, &peer(S1), ProbeOutcome::Unreachable, TTL + 1)
            .expect("unknown to not_verified");
        assert_eq!(
            change.to,
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(TTL + 1)
            }
        );
    }

    #[test]
    fn a_failure_is_fresh_for_exactly_the_evidence_ttl() {
        let mut m = manager_with(&[S1, S2]);
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Unreachable, 0);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Unreachable, 4);
        assert_eq!(
            *m.state(),
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(4)
            }
        );
        assert!(m.expire_evidence(TTL - 1).is_none());
        assert!(
            m.expire_evidence(TTL).is_none(),
            "S1's failure lapsing leaves S2's, and the verdict, in place"
        );
        assert_eq!(
            m.expire_evidence(4 + TTL).map(|c| c.to),
            Some(ReachabilityVerdict::Unknown),
            "failures alone lapse to unknown"
        );
    }

    #[test]
    fn a_change_is_the_state_or_the_verified_set_and_never_a_timestamp_alone() {
        let mut m = manager_with(&[S1, S2]);
        // Unknown -> NotVerified is a change; a later failure while
        // already NotVerified moves only the timestamp and is not.
        assert!(
            m.record_outcome(A, &peer(S1), ProbeOutcome::Unreachable, 1)
                .is_some()
        );
        assert!(
            m.record_outcome(B, &peer(S1), ProbeOutcome::Unreachable, 2)
                .is_none()
        );
        assert_eq!(
            *m.state(),
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(2)
            },
            "the state still moved"
        );
        // Verifying a second address while already VerifiedPublic IS a
        // change: the advertised set grew.
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 3);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 3);
        assert!(verified(&m));
        let _ = m.record_outcome(B, &peer(S1), ProbeOutcome::Reachable, 4);
        let change = m
            .record_outcome(B, &peer(S2), ProbeOutcome::Reachable, 4)
            .expect("B joins the verified set");
        assert_eq!(change.from.state(), change.to.state());
        // Candidate order, which is the order the caller supplied.
        assert_eq!(change.to.verified_addresses(), [A, B]);
    }

    #[test]
    fn withdrawing_a_verified_candidate_ends_the_verdict_at_once() {
        let mut m = manager_with(&[S1, S2]);
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(B, &peer(S1), ProbeOutcome::Reachable, 0);
        assert!(verified(&m));
        let change = m.set_candidates([B], 1).expect("A is withdrawn");
        assert_eq!(
            change.to,
            ReachabilityVerdict::Unknown,
            "B alone has one success, which is insufficient evidence"
        );
        // And A's evidence is GONE: re-adding it inside the TTL does not
        // re-verify it from before.
        assert!(m.set_candidates([A, B], 2).is_none());
        assert!(!verified(&m), "a re-added address starts over");
    }

    #[test]
    fn a_verified_address_outlives_the_earliest_success_when_more_than_the_threshold_agree() {
        let mut m = manager_with(&[S1, S2, S3]);
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 100);
        let _ = m.record_outcome(A, &peer(S3), ProbeOutcome::Reachable, 200);
        let ReachabilityVerdict::VerifiedPublic {
            evidence_until_ms, ..
        } = m.state().clone()
        else {
            panic!("verified");
        };
        // NOT the earliest success (`TTL`, S1's): with three agreeing and
        // a threshold of two, the verdict survives S1 lapsing and ends
        // when S2's success does. The horizon is the threshold-th newest,
        // and the assertions below walk to it to prove the number is the
        // moment rather than a guess.
        assert_eq!(evidence_until_ms, 100 + TTL);
        assert!(
            m.expire_evidence(TTL).is_none(),
            "S1 lapsed; S2 and S3 still meet the threshold"
        );
        assert!(verified(&m));
        let ReachabilityVerdict::VerifiedPublic {
            evidence_until_ms, ..
        } = m.state().clone()
        else {
            panic!("verified");
        };
        assert_eq!(evidence_until_ms, 100 + TTL, "unchanged: S2 still sets it");
        assert!(
            m.expire_evidence(100 + TTL - 1).is_none(),
            "one millisecond short"
        );
        assert!(verified(&m));
        assert_eq!(
            m.expire_evidence(100 + TTL).map(|c| c.to),
            Some(ReachabilityVerdict::Unknown),
            "the verdict ends exactly at the horizon it published"
        );
    }

    #[test]
    fn the_horizon_counts_a_reversed_servers_success_because_the_quorum_rests_on_it() {
        // S1 and S2 verify A; S2 refreshes late; S1 then contradicts
        // itself while its own success is still fresh, so the hold
        // applies and the quorum is S2 plus reversed S1. The verdict
        // therefore ends when S1's SUCCESS lapses -- and an earlier
        // version took the minimum over the uncontradicted members only,
        // publishing S2's expiry, a whole TTL later than the verdict
        // actually ended. Review finding on PR #84.
        let mut m = manager_with(&[S1, S2]);
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, TTL - 1);
        assert!(
            m.record_outcome(A, &peer(S1), ProbeOutcome::Unreachable, TTL - 1)
                .is_none(),
            "reversed while its success is fresh: the hold applies"
        );
        let ReachabilityVerdict::VerifiedPublic {
            evidence_until_ms, ..
        } = m.state().clone()
        else {
            panic!("verified");
        };
        assert_eq!(
            evidence_until_ms, TTL,
            "S1's success is the threshold-th newest, so it sets the horizon"
        );
        assert_eq!(
            m.expire_evidence(TTL).map(|c| c.to),
            Some(ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(TTL - 1)
            }),
            "and the verdict ends there, not at S2's expiry"
        );
    }

    #[test]
    fn network_change_resets_to_unknown_and_clears_everything() {
        let mut m = manager_with(&[S1, S2]);
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 0);
        assert!(verified(&m));
        let change = m.network_changed().expect("a change");
        assert_eq!(change.to, ReachabilityVerdict::Unknown);
        assert!(m.evidence.is_empty());
        assert!(m.network_changed().is_none(), "idempotent");
        // The servers and candidates survive: they are configuration,
        // not evidence.
        assert_eq!(m.dial_order().len(), 2);
        assert_eq!(m.candidates(), [A, B]);
    }

    #[test]
    fn removing_a_server_withdraws_its_evidence() {
        let mut m = manager_with(&[S1, S2]);
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 0);
        assert!(verified(&m));
        let change = m.remove_server(&peer(S2), 1).expect("below threshold");
        assert_eq!(change.to, ReachabilityVerdict::Unknown);
        assert!(!m.is_server(&peer(S2)));
        // And it is a stranger now: its later word is dropped.
        assert!(
            m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 2)
                .is_none()
        );
        assert!(!verified(&m));
    }

    #[test]
    fn a_dissenter_that_never_said_reachable_cannot_hold_the_verdict() {
        // S1 and S2 verify A; S3 contradicts -- one contradiction, which
        // holds nothing against two. Removing S1 leaves one fresh
        // success and one fresh failure. An earlier hold counted S3
        // toward the quorum and kept A verified on the word of the
        // server calling it unreachable; de-authorising a verifying
        // server must remove its influence. Review finding on PR #84.
        let mut m = manager_with(&[S1, S2, S3]);
        let _ = m.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 0);
        assert!(
            m.record_outcome(A, &peer(S3), ProbeOutcome::Unreachable, 1)
                .is_none()
        );
        assert!(verified(&m));
        let change = m
            .remove_server(&peer(S1), 2)
            .expect("the verifying quorum is gone");
        assert_eq!(
            change.to,
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(1)
            }
        );
    }

    #[test]
    fn a_failure_at_the_expiry_instant_does_not_extend_the_verdict() {
        // S1 at 0 and S2 at 100 verify A with `evidence_until_ms == TTL`.
        // At exactly TTL, S1's success lapses and S3 says unreachable.
        // The only new information is a failure, so the verdict must
        // end, and end the same way whether or not an expiry pass ran in
        // between. That is this sequence, not a general property of the
        // manager: the hold reads the previous verdict, so a rederive
        // that has not run can leave one standing that a run would have
        // ended. Narrowed from a claim about the manager to a claim about
        // this case, which is what the test proves. Review findings on
        // PR #84.
        let mut direct = manager_with(&[S1, S2, S3]);
        let _ = direct.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = direct.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 100);
        assert!(verified(&direct));
        let mut ticked = direct.clone();
        let change = direct
            .record_outcome(A, &peer(S3), ProbeOutcome::Unreachable, TTL)
            .expect("verified ends");
        assert_eq!(
            change.to,
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(TTL)
            }
        );
        assert_eq!(
            ticked.expire_evidence(TTL).map(|c| c.to),
            Some(ReachabilityVerdict::Unknown)
        );
        let _ = ticked.record_outcome(A, &peer(S3), ProbeOutcome::Unreachable, TTL);
        assert_eq!(
            direct.state(),
            ticked.state(),
            "a failure at the expiry instant ends the verdict either way"
        );
    }

    #[test]
    fn an_observer_whose_success_aged_out_is_a_dissenter_not_a_reversal() {
        // S1 at 0 and S2 at 100 verify A. At TTL, S1's success has just
        // lapsed; S1 now saying unreachable is a dissenter with no fresh
        // success under its failure, so the quorum is S2 alone and the
        // verdict ends. One millisecond earlier S1's success was fresh,
        // S1 REVERSED, and the hold applied -- until the success lapsed.
        let mut late = manager_with(&[S1, S2]);
        let _ = late.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = late.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 100);
        let change = late
            .record_outcome(A, &peer(S1), ProbeOutcome::Unreachable, TTL)
            .expect("aged out, then contradicting: no hold");
        assert_eq!(
            change.to,
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(TTL)
            }
        );
        let mut early = manager_with(&[S1, S2]);
        let _ = early.record_outcome(A, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = early.record_outcome(A, &peer(S2), ProbeOutcome::Reachable, 100);
        assert!(
            early
                .record_outcome(A, &peer(S1), ProbeOutcome::Unreachable, TTL - 1)
                .is_none(),
            "reversed while its success is fresh: the hold applies"
        );
        assert!(verified(&early));
        assert_eq!(
            early.expire_evidence(TTL).map(|c| c.to),
            Some(ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(TTL - 1)
            }),
            "and lapses with the success it stood on"
        );
    }

    #[test]
    fn every_verdict_maps_to_the_neutral_state_of_the_same_name() {
        assert_eq!(
            ReachabilityVerdict::Unknown.state(),
            DirectInboundState::Unknown
        );
        assert_eq!(
            ReachabilityVerdict::VerifiedPublic {
                verified_addresses: vec![],
                evidence_until_ms: 0,
            }
            .state(),
            DirectInboundState::VerifiedPublic
        );
        assert_eq!(
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: None
            }
            .state(),
            DirectInboundState::NotVerified
        );
    }
}
