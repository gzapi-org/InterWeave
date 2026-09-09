// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Direct-inbound reachability evidence (AutoNAT v2 client policy).
//!
//! `AUTONAT.md` §2 splits the client in two: the Swarm owns the libp2p
//! behaviour, and a `ReachabilityManager` owns POLICY and EVIDENCE. This
//! is that manager. It decides which `(address, server)` pairs are due a
//! probe, folds probe outcomes into evidence, and derives the
//! `ReachabilityVerdict` that `contracts/CONNECTIVITY.md` §5 and the
//! `connectivity-summary` schema describe. It opens no socket and reads no
//! clock: every method that can expire or schedule anything takes
//! `now_ms`, so the transitions are testable by enumeration.
//!
//! # Evidence is keyed by `(address, server)`, and only DISTINCT servers count
//!
//! `AUTONAT.md` §4: "Do not count repeated probes from one server as
//! distinct observers." A server that answers twice is one observer, and
//! `verified_public` needs `required_distinct_successes` of them for ONE
//! address with fresh evidence. The key makes that structural rather than
//! a check: a second success from the same server overwrites the first.
//!
//! # A private address is never a candidate
//!
//! `AUTONAT.md` §6: "Private/LAN addresses are never promoted to
//! Internet-public solely because they were configured or echoed by a
//! peer." This module refuses them one step earlier -- they are not
//! PROBED -- because sending a server a loopback or RFC 1918 address is
//! the SSRF-shaped request `AUTONAT.md` §7 makes the server refuse, and a
//! client that never sends one cannot be the reason a server had to.
//! [`is_probeable_address`] is the rule, and it is literal-IP only.
//!
//! # What this does NOT do: pick the probe server
//!
//! `AUTONAT.md`'s Amendment 2026-09-09 settled that the pinned client
//! chooses its own server -- uniformly at random among the connections
//! this profile DIALLED whose remote advertises the protocol -- and
//! exposes no hook to rank or veto one.
//! So [`ReachabilityManager::due_probes`] is NOT what starts a probe. It
//! is the schedule an adapter uses to decide **which servers to DIAL**
//! and when a pair's evidence has gone stale enough to be worth another
//! round: the amendment gives static configuration exactly that weaker
//! meaning, a server this profile guarantees to dial. Dial, not hold a
//! connection to -- an inbound connection from a server is never
//! eligible, because the client offers dial-request only on connections
//! it opened. [`ServerSource`] orders that dialling preference, not a
//! selection among peers already connected.
//!
//! The crate also reports no event when a probe STARTS -- only
//! `Event { tested_addr, server, result }` on completion -- so
//! [`ReachabilityManager::has_outstanding_probe_to`] answers for probes
//! this manager planned, and an adapter must not read it as "the crate
//! is probing that server right now".
//!
//! # Bounds are enforced here, not only by the configuration that names them
//!
//! `max_inflight_probes` and `max_candidate_addresses_per_cycle` are
//! ceilings the configuration crate validates on the way in. They are
//! also enforced at the point of use, because a caller building a
//! [`ReachabilityConfig`] in Rust never passed that validation -- the same
//! reasoning `profile-config` applies to its own candidate lists.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr};

use interweave_transport_api::{DirectInboundState, TransportIdentity};

/// `AUTONAT.md` §4 defaults, the ones the schema pins as defaults too.
pub const DEFAULT_REQUIRED_DISTINCT_SUCCESSES: u32 = 2;
/// Success evidence lifetime: 15 minutes.
pub const DEFAULT_SUCCESS_EVIDENCE_TTL_MS: u64 = 15 * 60 * 1000;
/// Initial retry after a failed probe: 30 seconds.
pub const DEFAULT_RETRY_INTERVAL_MS: u64 = 30 * 1000;
/// Bounded backoff ceiling for retries: 5 minutes.
pub const MAX_RETRY_BACKOFF_MS: u64 = 5 * 60 * 1000;
/// Refresh interval for a verified address: 5 minutes.
pub const DEFAULT_REFRESH_INTERVAL_MS: u64 = 5 * 60 * 1000;
/// Probes in flight at once.
pub const DEFAULT_MAX_INFLIGHT_PROBES: usize = 2;
/// Candidate addresses offered per cycle.
pub const DEFAULT_MAX_CANDIDATES_PER_CYCLE: usize = 4;
/// Per-probe timeout: 15 seconds.
pub const DEFAULT_PROBE_TIMEOUT_MS: u64 = 15 * 1000;
/// How many probeable addresses the manager will TRACK.
///
/// Distinct from `max_candidates_per_cycle`, which bounds how many are
/// offered in one cycle. This bounds memory: the evidence map is keyed
/// by address, and the address registry is not this module's to trust.
pub const MAX_TRACKED_CANDIDATES: usize = 32;

/// The policy knobs `AUTONAT.md` §4 names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachabilityConfig {
    /// Distinct authorized servers that must agree before an address is
    /// verified.
    pub required_distinct_successes: u32,
    /// How long one success stands.
    pub success_evidence_ttl_ms: u64,
    /// Delay before retrying a failed probe; doubles per consecutive
    /// failure up to [`MAX_RETRY_BACKOFF_MS`].
    pub retry_interval_ms: u64,
    /// How often a verified address is re-probed while its evidence is
    /// still fresh.
    pub refresh_interval_ms: u64,
    /// Probes in flight at once.
    pub max_inflight_probes: usize,
    /// Candidate addresses considered per cycle.
    pub max_candidates_per_cycle: usize,
    /// A probe older than this with no outcome is a failure.
    pub probe_timeout_ms: u64,
}

impl Default for ReachabilityConfig {
    fn default() -> Self {
        Self {
            required_distinct_successes: DEFAULT_REQUIRED_DISTINCT_SUCCESSES,
            success_evidence_ttl_ms: DEFAULT_SUCCESS_EVIDENCE_TTL_MS,
            retry_interval_ms: DEFAULT_RETRY_INTERVAL_MS,
            refresh_interval_ms: DEFAULT_REFRESH_INTERVAL_MS,
            max_inflight_probes: DEFAULT_MAX_INFLIGHT_PROBES,
            max_candidates_per_cycle: DEFAULT_MAX_CANDIDATES_PER_CYCLE,
            probe_timeout_ms: DEFAULT_PROBE_TIMEOUT_MS,
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
        /// The earliest expiry among the successes currently counted.
        ///
        /// NOT a bound on the claim: with more than the threshold of
        /// fresh successes the state survives this moment, because the
        /// ones that remain still meet it. An earlier version of this
        /// line said "and no later than then", which three successes and
        /// a threshold of two falsify. Review finding on PR #84.
        evidence_until_ms: u64,
    },
    /// Evidence is sufficient to say the threshold is not met.
    NotVerified {
        /// The most recent failure observed, if any.
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
}

/// What one probe told us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The server dialled the address back and reached us.
    Reachable,
    /// The server tried and did not reach us.
    Unreachable,
    /// The probe did not complete: refused, timed out, or errored before
    /// a dial-back was attempted. Says nothing about the address.
    Failed,
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

/// One probe the caller should start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbePlan {
    /// The address to have tested.
    pub address: String,
    /// The server to ask.
    pub server: TransportIdentity,
}

/// Emitted when the derived state changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectivityChanged {
    /// The state before.
    pub from: ReachabilityVerdict,
    /// The state now.
    pub to: ReachabilityVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Evidence {
    outcome: ProbeOutcome,
    observed_at_ms: u64,
    expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServerRecord {
    source: ServerSource,
    consecutive_failures: u32,
    backoff_until_ms: u64,
    /// When this server last failed to COMPLETE a probe. Kept here and
    /// not in `evidence`, because a `Failed` says nothing about the
    /// address it was testing -- see `record_outcome`.
    last_failure_at_ms: Option<u64>,
}

/// The manager. See the module note for what it owns and what it refuses.
#[derive(Debug, Clone)]
pub struct ReachabilityManager {
    config: ReachabilityConfig,
    candidates: Vec<String>,
    rejected_candidates: usize,
    servers: BTreeMap<TransportIdentity, ServerRecord>,
    evidence: BTreeMap<(String, TransportIdentity), Evidence>,
    inflight: BTreeMap<(String, TransportIdentity), u64>,
    state: ReachabilityVerdict,
}

impl ReachabilityManager {
    /// Build a manager, refusing a configuration whose bounds are zero.
    ///
    /// The configuration crate refuses the same values on the way in; this
    /// refuses them for a caller that never deserialized anything, so the
    /// `bounded` claims below hold for every constructor.
    pub fn new(config: ReachabilityConfig) -> Result<Self, ReachabilityError> {
        if config.required_distinct_successes == 0 {
            return Err(ReachabilityError::ZeroBound("required_distinct_successes"));
        }
        if config.max_inflight_probes == 0 {
            return Err(ReachabilityError::ZeroBound("max_inflight_probes"));
        }
        if config.max_candidates_per_cycle == 0 {
            return Err(ReachabilityError::ZeroBound(
                "max_candidate_addresses_per_cycle",
            ));
        }
        if config.probe_timeout_ms == 0 {
            return Err(ReachabilityError::ZeroBound("probe_timeout_ms"));
        }
        Ok(Self {
            config,
            candidates: Vec::new(),
            rejected_candidates: 0,
            servers: BTreeMap::new(),
            evidence: BTreeMap::new(),
            inflight: BTreeMap::new(),
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
    /// one, so a
    /// caller can honour `lifecycle.md`'s "emit `ConnectivityChanged`
    /// whenever the normalized state changes" without snapshotting
    /// around the call: withdrawing a verified listener ends the
    /// verdict, and that edge is one an advertisement must be withdrawn
    /// on. How many addresses were refused is
    /// [`rejected_candidates`](Self::rejected_candidates), so a caller
    /// can still say why a LAN-only node never reaches
    /// `verified_public`. Review finding on PR #84.
    ///
    /// The accepted list is bounded to [`MAX_TRACKED_CANDIDATES`], which
    /// is NOT `max_candidates_per_cycle`: the schema and `AUTONAT.md`
    /// §4 call that one a per-CYCLE ceiling, and applying it here made
    /// it a permanent truncation -- a profile with five listeners would
    /// never probe the fifth, and with evidence pruned by candidacy a
    /// registry whose iteration order varied would drop and re-gather
    /// evidence for whichever address rotated out. The cycle ceiling is
    /// applied where a cycle exists, in [`due_probes`].
    /// EVIDENCE FOR A WITHDRAWN ADDRESS
    /// IS DROPPED: keeping it left the map unbounded in the address
    /// dimension while `candidates` is bounded, and re-adding an address
    /// inside the TTL would have re-verified it from evidence gathered
    /// before it was withdrawn -- silently, and without a probe.
    /// RE-DERIVES, because dropping a candidate can end a verdict.
    /// Without it `state()` would keep naming a withdrawn address in
    /// `verified_addresses` until the next expiry pass, and ADR-0035's
    /// advertisement rule reads exactly that list. Review finding on
    /// PR #84.
    pub fn set_candidates<I, S>(&mut self, addresses: I, now_ms: u64) -> Option<ConnectivityChanged>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut accepted = Vec::new();
        let mut seen = BTreeSet::new();
        let mut rejected = 0;
        for address in addresses {
            let address = address.as_ref();
            if !is_probeable_address(address) {
                rejected += 1;
                continue;
            }
            if seen.insert(address.to_owned()) && accepted.len() < MAX_TRACKED_CANDIDATES {
                accepted.push(address.to_owned());
            }
        }
        self.rejected_candidates = rejected;
        self.candidates = accepted;
        self.inflight
            .retain(|(address, _), _| self.candidates.contains(address));
        self.evidence
            .retain(|(address, _), _| self.candidates.contains(address));
        self.rederive(now_ms)
    }

    /// Candidates refused by the last [`set_candidates`](Self::set_candidates).
    #[must_use]
    pub const fn rejected_candidates(&self) -> usize {
        self.rejected_candidates
    }

    /// The addresses currently eligible for probing.
    #[must_use]
    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    /// Offer a server. Eligibility by class and by opt-in is the CALLER's
    /// to decide before calling this (`AUTONAT.md` §3); this module orders
    /// static servers before Identify-learned ones and tracks backoff.
    pub fn add_server(&mut self, server: TransportIdentity, source: ServerSource) {
        match self.servers.entry(server) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(ServerRecord {
                    source,
                    consecutive_failures: 0,
                    backoff_until_ms: 0,
                    last_failure_at_ms: None,
                });
            }
            // A KNOWN SERVER KEEPS ITS BACKOFF but takes the stronger
            // source: `or_insert` left a peer first seen through Identify
            // recorded as `Identify` when it was later added as a
            // configured one, which under the 2026-09-09 amendment is a
            // silent downgrade of the dialling guarantee static
            // configuration buys. Review finding on PR #84.
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                if source < slot.get().source {
                    slot.get_mut().source = source;
                }
            }
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
        self.inflight.retain(|(_, s), _| s != server);
        self.rederive(now_ms)
    }

    /// Whether a probe to this server is outstanding.
    ///
    /// This is the input the inbound admission arm needs for route 3: a
    /// dial-back from a server we asked is expected, and one from a server
    /// we did not ask is not.
    #[must_use]
    pub fn has_outstanding_probe_to(&self, server: &TransportIdentity) -> bool {
        self.inflight.keys().any(|(_, s)| s == server)
    }

    /// Probes to start now.
    ///
    /// BOUNDED: never more than `max_inflight_probes` in flight, and no
    /// more than `max_candidates_per_cycle` distinct addresses offered
    /// in one call -- the per-cycle ceiling `AUTONAT.md` §4 names,
    /// applied here because this is where a cycle exists. Addresses are
    /// considered LEAST-RECENTLY-PROBED FIRST, so a profile with more
    /// listeners than the ceiling rotates through them instead of
    /// probing the first few forever. A pair with fresh
    /// success evidence is re-probed only once `refresh_interval_ms` has
    /// passed since that success; a pair whose last outcome was a failure
    /// waits out the server's backoff; a pair already in flight is not
    /// offered twice. Static servers are offered before Identify-learned
    /// ones, and for one server the candidates are offered in order.
    pub fn due_probes(&mut self, now_ms: u64) -> Vec<ProbePlan> {
        let mut plans = Vec::new();
        let room = self
            .config
            .max_inflight_probes
            .saturating_sub(self.inflight.len());
        if room == 0 {
            return plans;
        }
        // LEAST RECENTLY PROBED FIRST. `candidates` keeps the order the
        // caller gave, which would otherwise mean the first few are
        // probed forever and the rest never.
        let mut order: Vec<(u64, &String)> = self
            .candidates
            .iter()
            .map(|address| {
                let newest = self
                    .evidence
                    .iter()
                    .filter(|((a, _), _)| a == address)
                    .map(|(_, e)| e.observed_at_ms)
                    .max();
                (newest.unwrap_or(0), address)
            })
            .collect();
        order.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));
        let cycle: Vec<String> = order
            .into_iter()
            .take(self.config.max_candidates_per_cycle)
            .map(|(_, address)| address.clone())
            .collect();
        let mut servers: Vec<(&TransportIdentity, &ServerRecord)> = self.servers.iter().collect();
        servers.sort_by_key(|(_, record)| record.source);
        for (server, record) in servers {
            if record.backoff_until_ms > now_ms {
                continue;
            }
            for address in &cycle {
                if plans.len() >= room {
                    break;
                }
                let key = (address.clone(), server.clone());
                if self.inflight.contains_key(&key) {
                    continue;
                }
                let due = match self.evidence.get(&key) {
                    None => true,
                    Some(evidence) => match evidence.outcome {
                        ProbeOutcome::Reachable => {
                            now_ms
                                >= evidence
                                    .observed_at_ms
                                    .saturating_add(self.config.refresh_interval_ms)
                                || now_ms >= evidence.expires_at_ms
                        }
                        ProbeOutcome::Unreachable | ProbeOutcome::Failed => true,
                    },
                };
                if due {
                    plans.push(ProbePlan {
                        address: address.clone(),
                        server: server.clone(),
                    });
                }
            }
            if plans.len() >= room {
                break;
            }
        }
        for plan in &plans {
            self.inflight
                .insert((plan.address.clone(), plan.server.clone()), now_ms);
        }
        plans
    }

    /// Fold a probe's outcome in. Returns the state change, if any.
    ///
    /// An outcome for a pair that is not in flight is still recorded --
    /// the behaviour may report one we did not plan. TWO conditions drop
    /// it, and both return `None`: an address this manager does not
    /// track, because `derive` reads only candidates so such evidence
    /// could never count and would only grow the map; and a server we
    /// do not know, so a stranger's report cannot reach
    /// `verified_public`. FAILS CLOSED on both.
    ///
    /// The untracked-address case is the COMMON one, not an edge:
    /// `AUTONAT.md` §3's open note records that the pinned client probes
    /// from libp2p's own candidate set, which is not this list.
    pub fn record_outcome(
        &mut self,
        address: &str,
        server: &TransportIdentity,
        outcome: ProbeOutcome,
        now_ms: u64,
    ) -> Option<ConnectivityChanged> {
        let record = self.servers.get_mut(server)?;
        let key = (address.to_owned(), server.clone());
        self.inflight.remove(&key);
        // THE SERVER ACCOUNTING BELOW IS NOT ADDRESS-SCOPED, so it runs
        // for every outcome from a known server -- including the many
        // whose address this manager does not track, since `AUTONAT.md`
        // §3's open note records that the pinned client probes from
        // libp2p's own candidate set rather than this one. Gating it on
        // the address would have meant a server that times out on those
        // probes never accumulates backoff and one that succeeds never
        // clears it, which is health information about the SERVER thrown
        // away for an address reason. Only the evidence insert is gated.
        // Review findings on PR #84.
        let tracked = self.candidates.iter().any(|candidate| candidate == address);
        match outcome {
            ProbeOutcome::Reachable => {
                record.consecutive_failures = 0;
                record.backoff_until_ms = 0;
                // AND THE INCOMPLETE-PROBE TIMESTAMP, which otherwise
                // ages without bound: the evidence half it is merged
                // with expires after `retry_interval_ms`, so a timeout
                // from a server that has since worked would go on being
                // what `NotVerified` reports. Review finding on PR #84.
                record.last_failure_at_ms = None;
            }
            ProbeOutcome::Unreachable | ProbeOutcome::Failed => {
                record.consecutive_failures = record.consecutive_failures.saturating_add(1);
                let shift = record.consecutive_failures.saturating_sub(1).min(16);
                let backoff = self
                    .config
                    .retry_interval_ms
                    .saturating_mul(1_u64 << shift)
                    .min(MAX_RETRY_BACKOFF_MS);
                record.backoff_until_ms = now_ms.saturating_add(backoff);
            }
        }
        // A `Failed` IS NOT ADDRESS EVIDENCE, and must not displace any.
        // `evidence` is keyed `(address, server)` and an insert replaces,
        // so recording a timeout here destroyed that server's still-fresh
        // `Reachable` for the same address -- and one refresh that timed
        // out dropped a verified address below the threshold on its own,
        // which `AUTONAT.md` §5 gives only to TWO fresh independent
        // failures. The variant's own doc says it "says nothing about the
        // address"; this is what makes that true. It still counts against
        // the SERVER: the backoff above, and the timestamp here, which is
        // what `NotVerified` reports. Review finding on PR #84.
        if matches!(outcome, ProbeOutcome::Failed) {
            record.last_failure_at_ms = Some(
                record
                    .last_failure_at_ms
                    .map_or(now_ms, |previous| previous.max(now_ms)),
            );
            return self.rederive(now_ms);
        }
        if !tracked {
            return self.rederive(now_ms);
        }
        let expires_at_ms = match outcome {
            ProbeOutcome::Reachable => now_ms.saturating_add(self.config.success_evidence_ttl_ms),
            // An `Unreachable` is fresh for one retry interval: long
            // enough to count toward invalidation, short enough not to
            // pin `not_verified` after the condition has passed.
            // `Failed` returned above and cannot reach here.
            ProbeOutcome::Unreachable | ProbeOutcome::Failed => {
                now_ms.saturating_add(self.config.retry_interval_ms)
            }
        };
        self.evidence.insert(
            key,
            Evidence {
                outcome,
                observed_at_ms: now_ms,
                expires_at_ms,
            },
        );
        self.rederive(now_ms)
    }

    /// Time out probes that have been in flight too long, recording each
    /// as [`ProbeOutcome::Failed`]. Returns the state change, if any.
    pub fn expire_inflight(&mut self, now_ms: u64) -> Option<ConnectivityChanged> {
        let stale: Vec<(String, TransportIdentity)> = self
            .inflight
            .iter()
            .filter(|(_, started)| now_ms.saturating_sub(**started) >= self.config.probe_timeout_ms)
            .map(|(key, _)| key.clone())
            .collect();
        let before = self.state.clone();
        for (address, server) in stale {
            let _ = self.record_outcome(&address, &server, ProbeOutcome::Failed, now_ms);
        }
        Self::change(before, &self.state)
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
        self.inflight.clear();
        for record in self.servers.values_mut() {
            record.consecutive_failures = 0;
            record.backoff_until_ms = 0;
            record.last_failure_at_ms = None;
        }
        self.state = ReachabilityVerdict::Unknown;
        Self::change(before, &self.state)
    }

    fn rederive(&mut self, now_ms: u64) -> Option<ConnectivityChanged> {
        self.evidence.retain(|_, e| e.expires_at_ms > now_ms);
        let before = self.state.clone();
        self.state = self.derive(now_ms);
        Self::change(before, &self.state)
    }

    fn change(
        before: ReachabilityVerdict,
        after: &ReachabilityVerdict,
    ) -> Option<ConnectivityChanged> {
        if before == *after {
            None
        } else {
            Some(ConnectivityChanged {
                from: before,
                to: after.clone(),
            })
        }
    }

    /// `AUTONAT.md` §5, computed from the fresh evidence alone.
    ///
    /// For each candidate: the distinct servers reporting `Reachable` and
    /// the distinct servers reporting `Unreachable`. An address is
    /// verified when the first count meets the threshold AND fewer than
    /// two distinct servers currently contradict it -- "two fresh
    /// independent failures may invalidate a previously verified address
    /// before TTL". If any address is verified, `VerifiedPublic`; else if
    /// any evidence at all exists, `NotVerified`; else `Unknown`.
    fn derive(&self, now_ms: u64) -> ReachabilityVerdict {
        let mut verified = Vec::new();
        let mut evidence_until = u64::MAX;
        let mut any_evidence = false;
        let mut last_failure = None;
        for address in &self.candidates {
            let mut reachable: BTreeSet<&TransportIdentity> = BTreeSet::new();
            let mut unreachable: BTreeSet<&TransportIdentity> = BTreeSet::new();
            let mut earliest_success_expiry = u64::MAX;
            for ((a, server), e) in &self.evidence {
                if a != address || e.expires_at_ms <= now_ms {
                    continue;
                }
                any_evidence = true;
                match e.outcome {
                    ProbeOutcome::Reachable => {
                        reachable.insert(server);
                        earliest_success_expiry = earliest_success_expiry.min(e.expires_at_ms);
                    }
                    ProbeOutcome::Unreachable => {
                        unreachable.insert(server);
                        last_failure = Some(
                            last_failure.map_or(e.observed_at_ms, |f: u64| f.max(e.observed_at_ms)),
                        );
                    }
                    // A `Failed` never reaches `evidence`; see
                    // `record_outcome`.
                    ProbeOutcome::Failed => {}
                }
            }
            let threshold =
                usize::try_from(self.config.required_distinct_successes).unwrap_or(usize::MAX);
            if reachable.len() >= threshold && unreachable.len() < 2 {
                verified.push(address.clone());
                evidence_until = evidence_until.min(earliest_success_expiry);
            }
        }
        // THE MOST RECENT INCOMPLETE PROBE, read from the server records
        // -- reported by `NotVerified`, but never a reason to BE it.
        let last_incomplete = self
            .servers
            .values()
            .filter_map(|record| record.last_failure_at_ms)
            .max();
        if !verified.is_empty() {
            ReachabilityVerdict::VerifiedPublic {
                verified_addresses: verified,
                evidence_until_ms: evidence_until,
            }
        } else if any_evidence {
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: match (last_failure, last_incomplete) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                },
            }
        } else {
            // Timeouts and refusals ALONE are indeterminate: §5 gives
            // `not_verified` to evidence "sufficient to say the proof
            // threshold is not currently satisfied", and a probe that
            // never completed is not that.
            ReachabilityVerdict::Unknown
        }
    }
}

/// Whether a probe server may legitimately be asked to dial this address.
///
/// LITERAL IP ONLY, and Internet-public only: the first component must be
/// `/ip4/` or `/ip6/` with a parseable literal, and that literal must not
/// be loopback, unspecified, private (RFC 1918), shared (RFC 6598),
/// link-local, unique-local, multicast, broadcast, documentation or
/// benchmarking space. A `/dns4/` candidate is refused -- the server
/// would resolve it, which is a request on the server's behalf rather
/// than a test of our address.
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
    let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
    let benchmarking = segments[0] == 0x2001 && segments[1] == 0x0002 && segments[2] == 0;
    // 6to4 and Teredo carry an embedded IPv4 address whose reachability
    // is the tunnel's, not ours.
    let six_to_four = segments[0] == 0x2002;
    let teredo = segments[0] == 0x2001 && segments[1] == 0;
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
        || ipv4_compatible)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const S2: &str = "12D3KooWHyNGMf9HTd3Zj6dStdkcc5ycsubW1rEgQSp6k6yfZBoy";
    const S3: &str = "12D3KooWQYhTNQdmr3ArTeUHRYzFg94BKyTkoWBDWez9kSCVe2Xo";
    const S4: &str = "12D3KooWL8ZMFsFZwpZ3vGSg1b5RSgVzmMMpr1HdXLzqtoprdzZA";
    const PUBLIC_A: &str = "/ip4/203.0.113.7/tcp/4001";
    const PUBLIC_B: &str = "/ip4/198.51.100.9/tcp/4001";

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("a valid identity")
    }

    fn manager() -> ReachabilityManager {
        let mut m =
            ReachabilityManager::new(ReachabilityConfig::default()).expect("defaults are valid");
        // 203.0.113.0/24 and 198.51.100.0/24 are documentation space and
        // so NOT probeable; the tests use real-looking public literals
        // through `public_manager` below. These two exist for the
        // refusal test.
        let _ = (PUBLIC_A, PUBLIC_B);
        m.set_candidates(["/ip4/8.8.8.8/tcp/4001", "/ip4/1.1.1.1/tcp/4001"], 0);
        m
    }

    fn verified(m: &ReachabilityManager) -> bool {
        matches!(m.state(), ReachabilityVerdict::VerifiedPublic { .. })
    }

    #[test]
    fn a_zero_bound_is_refused_for_a_caller_that_never_deserialized() {
        for (field, config) in [
            (
                "required_distinct_successes",
                ReachabilityConfig {
                    required_distinct_successes: 0,
                    ..ReachabilityConfig::default()
                },
            ),
            (
                "max_inflight_probes",
                ReachabilityConfig {
                    max_inflight_probes: 0,
                    ..ReachabilityConfig::default()
                },
            ),
            (
                "max_candidate_addresses_per_cycle",
                ReachabilityConfig {
                    max_candidates_per_cycle: 0,
                    ..ReachabilityConfig::default()
                },
            ),
            (
                "probe_timeout_ms",
                ReachabilityConfig {
                    probe_timeout_ms: 0,
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
    fn private_loopback_dns_and_documentation_addresses_are_never_candidates() {
        // THE RULE, NOT THE LIST: each refused address is a class
        // `AUTONAT.md` §6/§7 names, and one public address is the
        // positive control so the test cannot pass by refusing everything.
        let refused = [
            "/ip4/127.0.0.1/tcp/4001",
            "/ip4/10.1.2.3/tcp/4001",
            "/ip4/172.16.0.9/tcp/4001",
            "/ip4/192.168.1.2/tcp/4001",
            "/ip4/169.254.1.1/tcp/4001",
            "/ip4/100.64.0.1/tcp/4001",
            "/ip4/0.0.0.0/tcp/4001",
            "/ip4/224.0.0.1/tcp/4001",
            "/ip4/255.255.255.255/tcp/4001",
            "/ip4/203.0.113.7/tcp/4001",
            "/ip4/198.18.0.1/tcp/4001",
            "/ip4/240.0.0.1/tcp/4001",
            "/ip6/::1/tcp/4001",
            "/ip6/::/tcp/4001",
            "/ip6/fe80::1/tcp/4001",
            "/ip6/fc00::1/tcp/4001",
            "/ip6/fd12::1/tcp/4001",
            "/ip6/ff02::1/tcp/4001",
            "/ip6/2001:db8::1/tcp/4001",
            "/ip6/::ffff:10.0.0.1/tcp/4001",
            "/dns4/relay.example.net/tcp/4001",
            // THE SHAPE A LISTENER ACTUALLY HANDS OVER, not the bare
            // marker: a relayed observed address begins `/ip4/` with a
            // public literal and was accepted while only the first
            // component was read.
            "/ip4/198.41.0.4/tcp/4001/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN/p2p-circuit/p2p/12D3KooWHyNGMf9HTd3Zj6dStdkcc5ycsubW1rEgQSp6k6yfZBoy",
            "/ip6/2606:4700:4700::1111/tcp/4001/p2p-circuit",
            "/p2p-circuit",
            "/ip4/0.1.2.3/tcp/4001",
            "/ip4/192.0.0.8/tcp/4001",
            "/ip4/192.88.99.1/tcp/4001",
            "/ip6/fec0::1/tcp/4001",
            "/ip6/2002:c058:6301::1/tcp/4001",
            "/ip6/2001:0:1234::1/tcp/4001",
            "/ip6/::10.0.0.1/tcp/4001",
            "garbage",
            "/ip4/not-an-ip/tcp/4001",
            "",
        ];
        for address in refused {
            assert!(!is_probeable_address(address), "must refuse {address:?}");
        }
        for address in [
            "/ip4/8.8.8.8/tcp/4001",
            "/ip6/2606:4700:4700::1111/tcp/4001",
            "/ip4/1.1.1.1",
        ] {
            assert!(is_probeable_address(address), "must accept {address:?}");
        }
        let mut m = ReachabilityManager::new(ReachabilityConfig::default()).expect("valid");
        m.set_candidates(
            [
                "/ip4/127.0.0.1/tcp/4001",
                "/ip4/8.8.8.8/tcp/4001",
                "/dns4/x/tcp/1",
            ],
            0,
        );
        assert_eq!(m.rejected_candidates(), 2);
        assert_eq!(m.candidates(), ["/ip4/8.8.8.8/tcp/4001"]);
    }

    #[test]
    fn the_tracked_list_is_bounded_and_deduplicated_and_the_cycle_rotates() {
        let mut m = ReachabilityManager::new(ReachabilityConfig::default()).expect("valid");
        let many: Vec<String> = (1..=50)
            .map(|i| format!("/ip4/8.8.{i}.{i}/tcp/4001"))
            .collect();
        m.set_candidates(many.iter().chain(many.iter()), 0);
        assert_eq!(m.rejected_candidates(), 0, "all fifty are public");
        // TRACKED is bounded by MAX_TRACKED_CANDIDATES, not by the
        // per-cycle ceiling: applying the cycle ceiling here made it a
        // permanent truncation, so a fifth listener was never probed at
        // all. Review finding on PR #84.
        assert_eq!(m.candidates().len(), MAX_TRACKED_CANDIDATES);
        assert_eq!(m.candidates(), &many[..MAX_TRACKED_CANDIDATES]);

        // AND THE CYCLE ROTATES. Each round offers `max_inflight_probes`
        // addresses; the ones already probed go to the back, so a later
        // round reaches addresses the first never touched.
        m.add_server(peer(S1), ServerSource::Static);
        let first = m.due_probes(0);
        assert_eq!(first.len(), DEFAULT_MAX_INFLIGHT_PROBES);
        for plan in &first {
            let _ = m.record_outcome(&plan.address, &plan.server, ProbeOutcome::Unreachable, 1);
        }
        // S1 backs off after two failures, so a second server keeps the
        // cycle moving.
        m.add_server(peer(S2), ServerSource::Static);
        let second = m.due_probes(2);
        assert_eq!(second.len(), DEFAULT_MAX_INFLIGHT_PROBES);
        assert!(
            second
                .iter()
                .all(|plan| !first.iter().any(|earlier| earlier.address == plan.address)),
            "the second cycle must reach addresses the first did not: {first:?} then {second:?}"
        );
    }

    #[test]
    fn the_cycle_ceiling_binds_when_the_in_flight_bound_does_not() {
        // WITH THE DEFAULTS THIS CEILING IS INVISIBLE: two in-flight
        // slots against four candidates per cycle, so the smaller bound
        // always decides and deleting the `take` changed nothing any
        // test could see. Here the ceiling is two and the slots are
        // eight, so it is the only thing limiting the round.
        // Review finding on PR #84.
        let mut m = ReachabilityManager::new(ReachabilityConfig {
            max_candidates_per_cycle: 2,
            max_inflight_probes: 8,
            ..ReachabilityConfig::default()
        })
        .expect("valid");
        m.set_candidates(
            (1..=5)
                .map(|i| format!("/ip4/8.8.{i}.{i}/tcp/4001"))
                .collect::<Vec<_>>(),
            0,
        );
        m.add_server(peer(S1), ServerSource::Static);
        let plans = m.due_probes(0);
        let addresses: BTreeSet<&String> = plans.iter().map(|plan| &plan.address).collect();
        assert_eq!(
            addresses.len(),
            2,
            "at most `max_candidates_per_cycle` distinct addresses in one cycle: {plans:?}"
        );
    }

    #[test]
    fn in_flight_probes_are_bounded_and_not_offered_twice() {
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        m.add_server(peer(S2), ServerSource::Static);
        let first = m.due_probes(0);
        assert_eq!(
            first.len(),
            DEFAULT_MAX_INFLIGHT_PROBES,
            "two in flight, no more"
        );
        assert!(
            m.due_probes(0).is_empty(),
            "nothing more while both slots are taken"
        );
        assert!(m.has_outstanding_probe_to(&peer(S1)));
        // One completes; exactly one slot reopens, and the completed pair
        // is not re-offered while its success is fresh.
        let done = &first[0];
        let _ = m.record_outcome(&done.address, &done.server, ProbeOutcome::Reachable, 1);
        let next = m.due_probes(1);
        assert_eq!(next.len(), 1);
        // ON THE SET, not on inequality with the one that completed:
        // deleting the `inflight.contains_key` guard re-offers the pair
        // still in flight, which is a DIFFERENT pair from `done` and so
        // passed an `assert_ne!` against it. Review finding on PR #84.
        assert!(
            !next.iter().any(|plan| first.contains(plan)),
            "a pair already in flight must not be offered twice: {next:?} against {first:?}"
        );
    }

    #[test]
    fn static_servers_are_preferred_for_dialling_before_identify_learned_ones() {
        // DIALLING PREFERENCE, not selection among connected peers:
        // `AUTONAT.md`'s Amendment 2026-09-09 removed the selection-order
        // rule because the pinned client cannot express one. What
        // survives is that a static server is one this profile
        // guarantees to dial, and this ordering is what an
        // adapter reads to honour that.
        let mut m = manager();
        m.add_server(peer(S2), ServerSource::Identify);
        m.add_server(peer(S1), ServerSource::Static);
        let plans = m.due_probes(0);
        assert!(plans.iter().all(|p| p.server == peer(S1)), "{plans:?}");
    }

    #[test]
    fn verified_needs_distinct_servers_and_one_server_twice_is_one_observer() {
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        m.add_server(peer(S2), ServerSource::Static);
        let a = "/ip4/8.8.8.8/tcp/4001";
        assert_eq!(
            m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, 10)
                .map(|c| c.to),
            Some(ReachabilityVerdict::NotVerified {
                last_failure_at_ms: None
            })
        );
        // THE SAME SERVER AGAIN: still one observer.
        assert!(
            m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, 20)
                .is_none()
        );
        assert!(!verified(&m), "one server twice is not two servers");
        let change = m
            .record_outcome(a, &peer(S2), ProbeOutcome::Reachable, 30)
            .expect("second distinct server verifies");
        assert_eq!(
            change.to,
            ReachabilityVerdict::VerifiedPublic {
                verified_addresses: vec![a.to_owned()],
                evidence_until_ms: 20 + DEFAULT_SUCCESS_EVIDENCE_TTL_MS,
            },
            "evidence_until is the EARLIEST counting success's expiry"
        );
        assert_eq!(m.state().state(), DirectInboundState::VerifiedPublic);
    }

    #[test]
    fn an_outcome_for_an_untracked_address_is_dropped() {
        // The behaviour reports whatever the swarm handed it, and
        // `derive` iterates candidates -- so evidence for an address we
        // do not track could never be counted, and keeping it would grow
        // the map between `set_candidates` calls. Review finding on
        // PR #84.
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        m.add_server(peer(S2), ServerSource::Static);
        let stranger = "/ip4/9.9.9.9/tcp/4001";
        assert!(!m.candidates().iter().any(|c| c == stranger));
        assert!(
            m.record_outcome(stranger, &peer(S1), ProbeOutcome::Reachable, 1)
                .is_none()
        );
        assert!(
            m.record_outcome(stranger, &peer(S2), ProbeOutcome::Reachable, 2)
                .is_none()
        );
        assert_eq!(
            *m.state(),
            ReachabilityVerdict::Unknown,
            "two successes for an address we do not track verify nothing"
        );
        // THE LOAD-BEARING LINE. The three assertions above pass with the
        // guard deleted too, because `derive` reads only candidates -- so
        // this is the one that fails, and it is not to be simplified away.
        assert!(m.evidence.is_empty(), "and are not retained");
        // BUT THE SERVER'S HEALTH IS STILL RECORDED, because a probe that
        // failed says something about the server whatever address it
        // named. The pinned client probes mostly untracked addresses, so
        // gating this on the address would have left backoff never
        // accumulating and never clearing. Review finding on PR #84.
        let untracked = "/ip4/9.9.9.9/tcp/4001";
        let _ = m.record_outcome(untracked, &peer(S1), ProbeOutcome::Unreachable, 3);
        let record = m.servers.get(&peer(S1)).expect("known");
        assert_eq!(record.consecutive_failures, 1, "the server backs off");
        assert!(record.backoff_until_ms > 3);
        let _ = m.record_outcome(untracked, &peer(S1), ProbeOutcome::Reachable, 4);
        let record = m.servers.get(&peer(S1)).expect("known");
        assert_eq!(record.backoff_until_ms, 0, "and a success clears it");
        assert!(
            m.evidence.is_empty(),
            "still no evidence for an untracked address"
        );
    }

    #[test]
    fn a_stranger_server_is_dropped_and_counts_toward_nothing() {
        // FAILS CLOSED: a report from a server never offered is not
        // evidence. Without this, any peer could report our address
        // reachable and a threshold of two is two strangers.
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        let a = "/ip4/8.8.8.8/tcp/4001";
        assert!(
            m.record_outcome(a, &peer(S2), ProbeOutcome::Reachable, 1)
                .is_none()
        );
        assert!(
            m.record_outcome(a, &peer(S3), ProbeOutcome::Reachable, 2)
                .is_none()
        );
        assert_eq!(
            *m.state(),
            ReachabilityVerdict::Unknown,
            "two strangers verify nothing"
        );
    }

    #[test]
    fn verified_lapses_at_the_evidence_ttl_without_refresh() {
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        m.add_server(peer(S2), ServerSource::Static);
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(a, &peer(S2), ProbeOutcome::Reachable, 0);
        assert!(verified(&m));
        assert!(
            m.expire_evidence(DEFAULT_SUCCESS_EVIDENCE_TTL_MS - 1)
                .is_none(),
            "fresh until the TTL"
        );
        let change = m
            .expire_evidence(DEFAULT_SUCCESS_EVIDENCE_TTL_MS)
            .expect("lapses at the TTL");
        assert_eq!(
            change.to,
            ReachabilityVerdict::Unknown,
            "no evidence left, so unknown rather than not_verified"
        );
    }

    #[test]
    fn two_fresh_independent_failures_invalidate_a_verified_address_before_ttl() {
        // FOUR SERVERS, because the two contradicting reports must come
        // from servers whose own successes are not being overwritten: a
        // failure from S2 replaces S2's success under the same key and
        // drops the count below threshold by itself. The first version
        // of this test did that and passed with `unreachable.len() < 2`
        // deleted -- it was testing the threshold, not the invalidation.
        let mut m = manager();
        for s in [S1, S2, S3, S4] {
            m.add_server(peer(s), ServerSource::Static);
        }
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(a, &peer(S2), ProbeOutcome::Reachable, 0);
        assert!(verified(&m));
        // One contradicting server is not enough.
        assert!(
            m.record_outcome(a, &peer(S3), ProbeOutcome::Unreachable, 5)
                .is_none()
        );
        assert!(
            verified(&m),
            "one failure does not invalidate two successes"
        );
        // The second one, from a fourth server, does -- while both
        // successes are still fresh and still counted.
        let _ = m.record_outcome(a, &peer(S4), ProbeOutcome::Unreachable, 6);
        assert_eq!(
            *m.state(),
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(6)
            },
            "two fresh independent failures invalidate before TTL"
        );
    }

    #[test]
    fn a_failure_backs_off_the_server_bounded_and_a_success_resets_it() {
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        let a = "/ip4/8.8.8.8/tcp/4001";
        let mut now = 0;
        let mut last_backoff = 0;
        for i in 0..12 {
            let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Failed, now);
            let record = m.servers.get(&peer(S1)).expect("known");
            let backoff = record.backoff_until_ms - now;
            assert!(
                backoff >= last_backoff,
                "backoff never shrinks on failure (iteration {i})"
            );
            assert!(
                backoff <= MAX_RETRY_BACKOFF_MS,
                "and is BOUNDED at five minutes: {backoff}"
            );
            assert!(
                m.due_probes(now).is_empty(),
                "not offered while backing off"
            );
            assert!(
                !m.due_probes(now + backoff).is_empty(),
                "offered once the backoff passes"
            );
            m.inflight.clear();
            last_backoff = backoff;
            now += backoff;
        }
        assert_eq!(
            last_backoff, MAX_RETRY_BACKOFF_MS,
            "twelve doublings from 30s saturate at 5m"
        );
        let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, now);
        assert_eq!(
            m.servers.get(&peer(S1)).expect("known").backoff_until_ms,
            0,
            "a success resets the backoff"
        );
    }

    #[test]
    fn a_probe_with_no_outcome_times_out_and_leaves_the_state_indeterminate() {
        // A TIMEOUT IS NOT A VERDICT. §5 gives `not_verified` to evidence
        // "sufficient to say the proof threshold is not currently
        // satisfied"; a probe that never completed is not that, so the
        // state stays `unknown` and the timestamp is carried for the
        // report rather than being a reason to leave `unknown`.
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        let plans = m.due_probes(0);
        assert!(!plans.is_empty());
        assert!(
            m.expire_inflight(DEFAULT_PROBE_TIMEOUT_MS - 1).is_none(),
            "not yet"
        );
        assert!(m.has_outstanding_probe_to(&peer(S1)));
        assert!(
            m.expire_inflight(DEFAULT_PROBE_TIMEOUT_MS).is_none(),
            "indeterminate: unknown to unknown is no change"
        );
        assert_eq!(*m.state(), ReachabilityVerdict::Unknown);
        assert!(
            !m.has_outstanding_probe_to(&peer(S1)),
            "and is no longer outstanding"
        );
        // AND ONCE AN ADDRESS HAS A VERDICT, the timeout is reported
        // beside it rather than lost -- with the LATER of the two being
        // the timeout, which is what makes this a test of the merge.
        // The first version put the timeout first, so `max` returned the
        // evidence timestamp either way and dropping the server-record
        // half was invisible; the commit message claimed it was proved.
        // Review finding on PR #84.
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Unreachable, 20_000);
        assert_eq!(
            *m.state(),
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(20_000)
            }
        );
        // A later probe to a SECOND server times out while that
        // `Unreachable` is still fresh -- it expires at 20_000 + 30_000,
        // and the timeout lands at 45_000, so the state is still
        // `NotVerified` and the question is which timestamp it carries.
        m.add_server(peer(S2), ServerSource::Static);
        let planned = m.due_probes(30_000);
        assert!(
            planned.iter().any(|plan| plan.server == peer(S2)),
            "the second server must actually be probed: {planned:?}"
        );
        let _ = m.expire_inflight(30_000 + DEFAULT_PROBE_TIMEOUT_MS);
        assert_eq!(
            *m.state(),
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: Some(30_000 + DEFAULT_PROBE_TIMEOUT_MS)
            },
            "the incomplete probe is newer than the evidence, so it is what is reported"
        );
    }

    #[test]
    fn withdrawing_a_verified_candidate_ends_the_verdict_at_once() {
        // `state()` is a cached value, so without a re-derive here it
        // would go on naming an address the profile no longer listens
        // on -- and ADR-0035's advertisement rule reads exactly that
        // list. Review finding on PR #84.
        let mut m = manager();
        m.set_candidates(["/ip4/8.8.8.8/tcp/4001"], 0);
        m.add_server(peer(S1), ServerSource::Static);
        m.add_server(peer(S2), ServerSource::Static);
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(a, &peer(S2), ProbeOutcome::Reachable, 0);
        assert!(verified(&m));
        let change = m
            .set_candidates(["/ip4/1.1.1.1/tcp/4001"], 1)
            .expect("withdrawing the verified address is a state change");
        assert!(
            matches!(change.from, ReachabilityVerdict::VerifiedPublic { .. }),
            "and the edge reports where it came from: {change:?}"
        );
        assert_eq!(
            change.to,
            ReachabilityVerdict::Unknown,
            "the verified address is gone, so the verdict is too"
        );
        assert_eq!(*m.state(), ReachabilityVerdict::Unknown);
        // AND ITS EVIDENCE IS GONE WITH IT: re-adding the address inside
        // the TTL must re-probe, not silently re-verify from
        // observations gathered before it was withdrawn.
        assert!(
            m.set_candidates([a], 2).is_none(),
            "re-adding must not resurrect the verdict"
        );
        assert_eq!(*m.state(), ReachabilityVerdict::Unknown);
    }

    #[test]
    fn a_success_clears_the_servers_incomplete_probe_timestamp() {
        // Otherwise the value `NotVerified` reports ages without bound:
        // the evidence half it is merged with expires after
        // `retry_interval_ms`, so a timeout from a server that has since
        // WORKED would go on being reported as the last failure.
        // Review finding on PR #84.
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = m.due_probes(0);
        let _ = m.expire_inflight(DEFAULT_PROBE_TIMEOUT_MS);
        assert_eq!(
            m.servers.get(&peer(S1)).expect("known").last_failure_at_ms,
            Some(DEFAULT_PROBE_TIMEOUT_MS)
        );
        let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, 20_000);
        // One fresh success against a threshold of two, so the state is
        // `NotVerified` -- and it must carry NO failure, because the only
        // one on record belongs to a server that has since answered.
        assert_eq!(
            *m.state(),
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: None
            },
            "a success clears the timeout that preceded it"
        );
    }

    #[test]
    fn one_timed_out_refresh_does_not_unverify_an_address_two_servers_verified() {
        // THE DEFECT THIS EXISTS FOR: evidence is keyed
        // `(address, server)` and an insert replaces, so recording a
        // timeout as evidence destroyed that server's still-fresh
        // success and dropped the address below the threshold -- on ONE
        // event that says nothing about the address, where §5 requires
        // two fresh independent failures. Reachable on the ordinary
        // refresh path, which is what makes it worth a test rather than
        // a comment. Review finding on PR #84.
        let mut m = manager();
        m.set_candidates(["/ip4/8.8.8.8/tcp/4001"], 0);
        m.add_server(peer(S1), ServerSource::Static);
        m.add_server(peer(S2), ServerSource::Static);
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(a, &peer(S2), ProbeOutcome::Reachable, 0);
        assert!(verified(&m));
        // The refresh falls due, is issued, and never completes.
        let refresh = m.due_probes(DEFAULT_REFRESH_INTERVAL_MS);
        assert!(
            refresh.iter().any(|p| p.address == a),
            "the refresh must actually be planned, or this proves nothing: {refresh:?}"
        );
        let _ = m.expire_inflight(DEFAULT_REFRESH_INTERVAL_MS + DEFAULT_PROBE_TIMEOUT_MS);
        assert!(
            verified(&m),
            "a timed-out refresh must not unverify: {:?}",
            m.state()
        );
        // And the server it timed out against is still backed off, so
        // the failure was not simply discarded.
        assert!(
            m.due_probes(DEFAULT_REFRESH_INTERVAL_MS + DEFAULT_PROBE_TIMEOUT_MS)
                .iter()
                .all(|p| p.server != refresh[0].server),
            "the timed-out server backs off"
        );
    }

    #[test]
    fn a_verified_address_is_refreshed_after_the_refresh_interval_and_not_before() {
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        m.set_candidates(["/ip4/8.8.8.8/tcp/4001"], 0);
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, 0);
        assert!(
            m.due_probes(DEFAULT_REFRESH_INTERVAL_MS - 1).is_empty(),
            "fresh success is not re-probed"
        );
        assert_eq!(
            m.due_probes(DEFAULT_REFRESH_INTERVAL_MS).len(),
            1,
            "refreshed at the interval"
        );
    }

    #[test]
    fn a_static_addition_upgrades_a_server_first_seen_through_identify() {
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Identify);
        m.add_server(peer(S2), ServerSource::Static);
        // Re-adding the Identify one as configured must change which is
        // preferred for dialling; `or_insert` left it Identify.
        m.add_server(peer(S1), ServerSource::Static);
        assert_eq!(
            m.servers.get(&peer(S1)).expect("known").source,
            ServerSource::Static,
            "the upgraded server is now configured, not Identify-learned"
        );
        // And it does not go the other way.
        m.add_server(peer(S2), ServerSource::Identify);
        assert_eq!(
            m.servers.get(&peer(S2)).expect("known").source,
            ServerSource::Static,
            "a later Identify sighting must not downgrade a configured server"
        );
    }

    #[test]
    fn a_verified_address_outlives_the_earliest_success_when_more_than_the_threshold_agree() {
        // `evidence_until_ms` is the earliest COUNTING success's expiry
        // and NOT a bound on the claim: a third fresh success keeps the
        // threshold met after the first lapses. The doc said "no later
        // than then" until this test was written for it.
        // Review finding on PR #84.
        let mut m = manager();
        m.set_candidates(["/ip4/8.8.8.8/tcp/4001"], 0);
        for server in [S1, S2, S3] {
            m.add_server(peer(server), ServerSource::Static);
        }
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(a, &peer(S2), ProbeOutcome::Reachable, 10);
        let _ = m.record_outcome(a, &peer(S3), ProbeOutcome::Reachable, 20);
        let ReachabilityVerdict::VerifiedPublic {
            evidence_until_ms, ..
        } = m.state().clone()
        else {
            panic!("three successes verify: {:?}", m.state())
        };
        assert_eq!(evidence_until_ms, DEFAULT_SUCCESS_EVIDENCE_TTL_MS);
        // A CHANGE IS REPORTED, and it is not a lapse: `VerifiedPublic`
        // carries `evidence_until_ms`, so `ConnectivityChanged` fires
        // when the earliest counting success rolls forward even though
        // the WORD is unchanged. A consumer that only forwards the
        // summary word must compare words, not `Option::is_some`.
        let change = m
            .expire_evidence(evidence_until_ms)
            .expect("the earliest counting expiry rolls forward");
        assert_eq!(
            change.from.state(),
            change.to.state(),
            "the word does not change: {change:?}"
        );
        assert!(verified(&m), "so the claim OUTLIVES evidence_until_ms");
        let ReachabilityVerdict::VerifiedPublic {
            evidence_until_ms: now_until,
            ..
        } = m.state().clone()
        else {
            panic!("still verified")
        };
        assert_eq!(now_until, 10 + DEFAULT_SUCCESS_EVIDENCE_TTL_MS);
        // It ends when the second one goes: S3's success is still
        // fresh, so this is `not_verified` -- evidence sufficient to say
        // the threshold is not met -- and not `unknown`.
        let change = m
            .expire_evidence(10 + DEFAULT_SUCCESS_EVIDENCE_TTL_MS)
            .expect("now the threshold is not met");
        assert_eq!(
            change.to,
            ReachabilityVerdict::NotVerified {
                last_failure_at_ms: None
            },
            "one fresh success left, and no failure to report"
        );
    }

    #[test]
    fn the_backoff_shift_is_capped_before_it_can_overflow() {
        // `1_u64 << shift` panics at shift 64, and the `.min(16)` is
        // what stops it. Twelve failures never reach the cap, so the
        // clamp was untested; twenty do. Review finding on PR #84.
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        let a = "/ip4/8.8.8.8/tcp/4001";
        for i in 0..20 {
            let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Unreachable, i);
        }
        let record = m.servers.get(&peer(S1)).expect("known");
        assert_eq!(record.consecutive_failures, 20);
        assert!(
            record.backoff_until_ms.saturating_sub(19) <= MAX_RETRY_BACKOFF_MS,
            "still bounded at five minutes after twenty failures"
        );
    }

    #[test]
    fn dropping_a_candidate_frees_the_probe_slot_it_held() {
        // `set_candidates` retains only in-flight pairs whose address
        // survived. Without it a withdrawn listener's probe would hold
        // one of the two slots until the process ended, and nothing
        // would say so. Review finding on PR #84.
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        let first = m.due_probes(0);
        assert_eq!(first.len(), DEFAULT_MAX_INFLIGHT_PROBES);
        m.set_candidates(["/ip4/9.9.9.9/tcp/4001"], 0);
        assert!(
            !m.has_outstanding_probe_to(&peer(S1)),
            "the in-flight pairs named addresses that are gone"
        );
        assert_eq!(
            m.due_probes(1).len(),
            1,
            "and the slots are free for the new candidate"
        );
        // AND A LATE OUTCOME FOR THE DROPPED ADDRESS CHANGES NOTHING.
        // The two `retain`s above are what make this safe -- the
        // in-flight entry went with the address, so `record_outcome`'s
        // untracked-address guard has no slot left to leak. True today
        // by the pairing of those two lines, which is why it is pinned.
        let stale = &first[0];
        assert!(
            m.record_outcome(&stale.address, &stale.server, ProbeOutcome::Reachable, 2)
                .is_none(),
            "the address is gone, so its outcome is not evidence"
        );
        assert!(
            m.due_probes(3).is_empty(),
            "and it neither freed nor occupied a slot: the one live candidate is still in flight"
        );
        assert!(m.has_outstanding_probe_to(&peer(S1)));
        assert_eq!(*m.state(), ReachabilityVerdict::Unknown);
    }

    #[test]
    fn network_change_resets_to_unknown_and_clears_everything() {
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        m.add_server(peer(S2), ServerSource::Static);
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(a, &peer(S2), ProbeOutcome::Reachable, 0);
        let _ = m.due_probes(1);
        assert!(verified(&m));
        let change = m.network_changed().expect("a change");
        assert_eq!(change.to, ReachabilityVerdict::Unknown);
        assert!(!m.has_outstanding_probe_to(&peer(S1)) && !m.has_outstanding_probe_to(&peer(S2)));
        assert!(
            m.network_changed().is_none(),
            "unknown to unknown is no change"
        );
    }

    #[test]
    fn removing_a_server_withdraws_its_evidence() {
        let mut m = manager();
        m.add_server(peer(S1), ServerSource::Static);
        m.add_server(peer(S2), ServerSource::Static);
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = m.record_outcome(a, &peer(S1), ProbeOutcome::Reachable, 0);
        let _ = m.record_outcome(a, &peer(S2), ProbeOutcome::Reachable, 0);
        assert!(verified(&m));
        let change = m
            .remove_server(&peer(S2), 1)
            .expect("losing an observer changes the state");
        assert!(matches!(change.to, ReachabilityVerdict::NotVerified { .. }));
    }

    #[test]
    fn every_verdict_maps_to_the_neutral_state_of_the_same_name() {
        // THE WORDS ARE NOT HERE. They are `DirectInboundState`'s serde,
        // pinned against `connectivity-summary.schema.json` by
        // `transport-api/tests/schema_agreement.rs`, so this asserts the
        // mapping and lets that test own the vocabulary. The version
        // this replaced compared string literals to string literals in
        // a crate that reads no schema -- it could not fail if the two
        // diverged, which is the case it was named for.
        assert_eq!(
            ReachabilityVerdict::Unknown.state(),
            DirectInboundState::Unknown
        );
        assert_eq!(
            ReachabilityVerdict::VerifiedPublic {
                verified_addresses: vec![],
                evidence_until_ms: 0
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
