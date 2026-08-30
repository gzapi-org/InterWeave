// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! [`KademliaDiscovery`]: the provider face over the control-api port.

use interweave_discovery_api::{
    CandidatePeer, DiscoveryError, DiscoveryEvent, DiscoveryProvider, HintDisposition,
    MAX_ADDRESS_BYTES, PeerHint, ProtocolId, ProviderDescriptor, ProviderError, ProviderHealth,
    ProviderMode, ProviderScope,
};
use interweave_kademlia_control_api::{
    KademliaCommand, KademliaEvent, KademliaMode, MAX_ROUTING_PEERS, OfferedAddress,
    OfferedAddresses, QueryClass, QueryFailure, RoutingView,
};
use interweave_transport_api::TransportIdentity;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::budgets::{Permit, QueryBudgets};
use crate::scheduler::Pacing;
use crate::{health, normalize};

/// The provider name, and the `source` on every candidate it emits.
pub const SOURCE: &str = "kademlia";

/// The provider-interface version this implements.
const INTERFACE_VERSION: &str = "1.0";

/// The provider's own configuration version (`config_version: 1`).
const CONFIG_VERSION: &str = "1.0";

/// Ceiling on candidates tracked from query results.
///
/// The routing-table ceiling ([`MAX_ROUTING_PEERS`]) is the largest peer
/// population the driver itself can hold, so candidates beyond it are
/// churn rather than reach. When full, the entry expiring SOONEST is
/// evicted — it is the one the TTL was about to remove anyway, and the
/// consumer sees an ordinary expiry rather than silence.
pub const MAX_TRACKED_CANDIDATES: usize = MAX_ROUTING_PEERS;

/// Ceiling on per-peer server-capability evidence entries.
///
/// Same value as the candidate bound, for the same reason: evidence about
/// more peers than the table could ever route is memory spent on nothing.
/// When full, the STALEST entry is evicted — and an arrival staler than
/// everything held is dropped instead, so churn cannot replace fresh
/// evidence with old.
pub const MAX_CAPABILITY_EVIDENCE: usize = MAX_ROUTING_PEERS;

/// Ceiling on routing OFFERS waiting for the driver.
///
/// When it is reached an incoming offer displaces the OLDEST offer, so a
/// stalled driver bounds memory at the cost of the stalest advice — which
/// the next observation regenerates. Control commands (`SetMode`,
/// `Shutdown`) and queries are never displaced: dropping a `StartQuery`
/// would leak the budget slot bound to it, and the budgets already cap
/// how many can be outstanding.
pub const MAX_PENDING_COMMANDS: usize = 256;

/// The base-32 alphabet of a network hash: lowercase RFC 4648, unpadded.
const NETWORK_HASH_LEN: usize = 26;

/// Static configuration for the provider.
///
/// Supplied by the composition root from the validated profile; the
/// scheduler intervals and budgets (§13's remaining fields) arrive with
/// the scheduler itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KademliaProviderConfig {
    /// Client or server, explicit and never inferred (§5).
    pub mode: KademliaMode,
    /// The wire major version of the Kademlia protocol (`1` today).
    pub wire_major: u32,
    /// The derived network namespace hash — 26 lowercase base-32
    /// characters, per `fixtures/kademlia/kad-network-namespace-v1.json`.
    pub network_hash: String,
    /// How long a query-result candidate stays usable (`candidate_ttl`).
    pub candidate_ttl_ms: u64,
    /// Minimum spacing between targeted lookups of one peer.
    pub targeted_lookup_cooldown_ms: u64,
    /// Configured routing-table target size.
    pub target_routing_peers: u32,
    /// Configured routing-table ceiling.
    pub max_routing_peers: u32,
    /// Base interval between exploration rounds (`exploration_interval`).
    pub exploration_interval_ms: u64,
    /// Jitter applied to that interval, in percent (`exploration_jitter_percent`).
    pub exploration_jitter_percent: u32,
    /// Concurrent query ceiling across all classes (`max_concurrent_queries`).
    pub max_concurrent_queries: u32,
    /// Query-rate ceiling across all classes (`max_queries_per_minute`).
    pub max_queries_per_minute: u32,
    /// Floor on bootstrap frequency (`bootstrap_min_interval`).
    pub bootstrap_min_interval_ms: u64,
    /// Routine bootstrap refresh interval (`bootstrap_refresh_interval`).
    pub bootstrap_refresh_interval_ms: u64,
}

/// Why a [`KademliaProviderConfig`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderConfigError {
    /// `wire_major` was zero; the protocol family starts at 1.
    ZeroWireMajor,
    /// The network hash was not 26 lowercase base-32 characters.
    MalformedNetworkHash,
    /// A zero candidate TTL would expire every observation on arrival.
    ZeroCandidateTtl,
    /// `target_routing_peers` exceeded `max_routing_peers`.
    TargetAboveMax,
    /// `max_routing_peers` exceeded the port ceiling.
    MaxAboveCeiling {
        /// The configured value.
        got: u32,
    },
    /// A zero exploration interval would busy-loop the scheduler.
    ZeroExplorationInterval,
    /// Jitter above 100 percent could schedule into the past.
    JitterAboveHundred,
    /// A zero concurrency ceiling could never run a query.
    ZeroConcurrency,
    /// A zero rate ceiling could never run a query.
    ZeroRate,
    /// `bootstrap_refresh_interval` below `bootstrap_min_interval` asks
    /// for a refresh the floor forbids (§13 rule 2).
    RefreshBelowMinimum,
}

/// Why a bootstrap request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapRefusal {
    /// The provider is not running.
    NotRunning,
    /// `bootstrap_min_interval` has not elapsed since the last one.
    TooSoon,
    /// The global query budget refused the work.
    BudgetExhausted,
    /// The local identity is not an Ed25519 PeerId, so no self-lookup
    /// key can be recovered.
    UntargetableSelf,
}

/// Why a targeted lookup was refused (§9.2, one conjunct at a time).
///
/// Bounded reasons rather than a formatted string, per the spec's "record
/// a bounded reason diagnostic instead of guessing remote mode".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetedRefusal {
    /// The provider is not running.
    NotRunning,
    /// The target is the local peer.
    SelfTarget,
    /// Conjunct 1: the target is not in the current trust snapshot.
    NotTrusted,
    /// Conjunct 2: no server-capability evidence exists for the target.
    NoServerEvidence,
    /// Conjunct 2: the evidence exists but its freshness bound has lapsed.
    StaleServerEvidence,
    /// Conjunct 2: the freshest evidence says the protocol is NOT served.
    NegativeServerEvidence,
    /// Conjunct 3: a usable address already exists; the lookup is not needed.
    UsableAddressExists,
    /// Conjunct 4: the per-target cooldown has not elapsed.
    CooldownActive,
    /// Conjunct 5: the global query budget refused the work.
    BudgetExhausted,
    /// The identity is not an Ed25519 PeerId, so no 32-byte lookup key
    /// can be recovered; querying a point that is not the peer's would be
    /// worse than refusing.
    NotTargetableIdentity,
}

/// What a query observed about one peer, and when it stops being usable.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tracked {
    observed_at: u64,
    expires_at: u64,
    addresses: BTreeSet<String>,
}

/// The freshest server-capability observation held for one peer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Evidence {
    supported: bool,
    observed_at: u64,
    expires_at: u64,
}

/// The DHT as a discovery provider.
///
/// Everything driver-facing is a queue the composition root pumps:
/// [`Self::drain_commands`] feeds the driver, [`Self::ingest_driver_event`]
/// returns what it saw. Discovery events are DERIVED FROM STATE at drain
/// time — the difference between `tracked` and `emitted` — so nothing a
/// bound discards is lost: the next drain recomputes it. It is the same
/// model the static and cache providers use, for the same reason.
#[derive(Debug)]
pub struct KademliaDiscovery {
    config: KademliaProviderConfig,
    local: TransportIdentity,
    /// The exact server protocol this network advertises, rendered once.
    /// Eligibility compares the FULL string, never a prefix — the test
    /// against the frozen fixture is what keeps this formula and the
    /// cache's renderer from drifting apart.
    expected_protocol: ProtocolId,
    started: bool,
    stopped: bool,
    /// The composer-supplied trust snapshot, local peer excluded.
    ///
    /// A SET rather than a count, because §9.2's first conjunct needs
    /// per-peer membership and the view needs the population; one source
    /// cannot disagree with itself. The provider still depends only on
    /// this snapshot, never on trust-api.
    trusted: BTreeSet<TransportIdentity>,
    /// Peers currently in the driver's routing table, from its events.
    routing: BTreeSet<TransportIdentity>,
    recent_queries_succeeded: bool,
    last_reported_health: Option<ProviderHealth>,
    commands: VecDeque<KademliaCommand>,
    tracked: BTreeMap<TransportIdentity, Tracked>,
    emitted: BTreeMap<TransportIdentity, Tracked>,
    evidence: BTreeMap<TransportIdentity, Evidence>,
    /// Last targeted-lookup issue time per peer. Pruned to the trust
    /// snapshot, so it is bounded by the trust policy's own size.
    cooldowns: BTreeMap<TransportIdentity, u64>,
    /// The global query budgets (§15), shared across all three classes.
    budgets: QueryBudgets,
    /// Exploration due-times and bootstrap spacing.
    pacing: Pacing,
    /// Consecutive successful exploration rounds that admitted nothing
    /// (§9.3). Reset by every saturation-invalidating event: progress,
    /// trust revision, a new seed or capability observation, routing
    /// loss.
    no_progress_rounds: u32,
    /// Total routing admissions ever, so an exploration round can tell
    /// whether anything was admitted while it ran even if something was
    /// also removed.
    admissions: u64,
    /// The admission count when the in-flight exploration was issued;
    /// `Some` means one exploration is running and `tick` starts no
    /// second one.
    exploration_snapshot: Option<u64>,
    /// The slot held for the library's implicit bootstrap (F2): a
    /// routing insertion on an empty table starts one query nobody
    /// requested, and it dials, so it is charged like the work it is.
    implicit_charge: Option<Permit>,
    /// The local self-lookup key, if the local identity is Ed25519.
    local_key: Option<[u8; 32]>,
    /// The latest instant any timed call has seen; what `health()` —
    /// which the trait gives no clock — judges evidence freshness at.
    clock: u64,
    /// Jitter entropy from the in-flight exploration's key, used when
    /// its completion schedules the next round.
    exploration_entropy: u64,
}

impl KademliaDiscovery {
    /// Build a provider for one network namespace.
    ///
    /// # Errors
    /// [`ProviderConfigError`] when a field is outside the port's bounds
    /// or the namespace hash is not in canonical form.
    pub fn new(
        config: KademliaProviderConfig,
        local: TransportIdentity,
    ) -> Result<Self, ProviderConfigError> {
        if config.wire_major == 0 {
            return Err(ProviderConfigError::ZeroWireMajor);
        }
        let hash_ok = config.network_hash.len() == NETWORK_HASH_LEN
            && config
                .network_hash
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b));
        if !hash_ok {
            return Err(ProviderConfigError::MalformedNetworkHash);
        }
        if config.candidate_ttl_ms == 0 {
            return Err(ProviderConfigError::ZeroCandidateTtl);
        }
        if config.target_routing_peers > config.max_routing_peers {
            return Err(ProviderConfigError::TargetAboveMax);
        }
        if config.max_routing_peers as usize > MAX_ROUTING_PEERS {
            return Err(ProviderConfigError::MaxAboveCeiling {
                got: config.max_routing_peers,
            });
        }
        if config.exploration_interval_ms == 0 {
            return Err(ProviderConfigError::ZeroExplorationInterval);
        }
        if config.exploration_jitter_percent > 100 {
            return Err(ProviderConfigError::JitterAboveHundred);
        }
        if config.max_concurrent_queries == 0 {
            return Err(ProviderConfigError::ZeroConcurrency);
        }
        if config.max_queries_per_minute == 0 {
            return Err(ProviderConfigError::ZeroRate);
        }
        if config.bootstrap_refresh_interval_ms < config.bootstrap_min_interval_ms {
            return Err(ProviderConfigError::RefreshBelowMinimum);
        }
        let rendered = format!(
            "/interweave/kad/{}.0.0/{}",
            config.wire_major, config.network_hash
        );
        let expected_protocol =
            ProtocolId::parse(rendered).map_err(|_| ProviderConfigError::MalformedNetworkHash)?;
        let budgets = QueryBudgets::new(
            config.max_concurrent_queries as usize,
            config.max_queries_per_minute,
        );
        let local_key = normalize::targeted_lookup_key(&local);
        Ok(Self {
            config,
            local,
            expected_protocol,
            started: false,
            stopped: false,
            trusted: BTreeSet::new(),
            routing: BTreeSet::new(),
            recent_queries_succeeded: false,
            last_reported_health: None,
            commands: VecDeque::new(),
            tracked: BTreeMap::new(),
            emitted: BTreeMap::new(),
            evidence: BTreeMap::new(),
            cooldowns: BTreeMap::new(),
            budgets,
            pacing: Pacing::default(),
            no_progress_rounds: 0,
            admissions: 0,
            exploration_snapshot: None,
            implicit_charge: None,
            local_key,
            clock: 0,
            exploration_entropy: 0,
        })
    }

    /// The exact server protocol id this provider requires as evidence.
    #[must_use]
    pub const fn expected_protocol(&self) -> &ProtocolId {
        &self.expected_protocol
    }

    /// Replace the remote-trusted snapshot.
    ///
    /// The composition root supplies this from the live trust policy —
    /// tests today, `TransportRuntime` when the composed daemon exists.
    /// The local peer is excluded here rather than trusted to be absent:
    /// §9.3's population counts DISTINCT REMOTE trusted peers, and a
    /// snapshot that included self would inflate the effective target.
    pub fn set_remote_trusted(&mut self, mut peers: BTreeSet<TransportIdentity>) {
        peers.remove(&self.local);
        self.cooldowns.retain(|peer, _| peers.contains(peer));
        self.trusted = peers;
        // RECONCILED IMMEDIATELY, not on the driver's schedule. The
        // driver's own revocation path removes the peer and reports
        // RoutingPeerRemoved, but until that event arrives a view that
        // still counted a revoked router could report the new effective
        // target satisfied and rest on a peer that is no longer
        // authorized. The later removal event finds nothing and changes
        // nothing.
        let trusted = &self.trusted;
        self.routing.retain(|peer| trusted.contains(peer));
        // §9.3: a trust-policy revision invalidates saturation — the set
        // the exploration ran out of is not the set any more.
        self.no_progress_rounds = 0;
    }

    /// The routing view the health model consumes.
    #[must_use]
    pub fn routing_view(&self) -> RoutingView {
        RoutingView {
            routing_peers: u32::try_from(self.routing.len()).unwrap_or(u32::MAX),
            target_routing_peers: self.config.target_routing_peers,
            max_routing_peers: self.config.max_routing_peers,
            remote_trusted_population: u32::try_from(self.trusted.len()).unwrap_or(u32::MAX),
            no_progress_rounds: self.no_progress_rounds,
        }
    }

    /// Take at most `max` commands for the driver, oldest first.
    pub fn drain_commands(&mut self, max: usize) -> Vec<KademliaCommand> {
        let take = max.min(self.commands.len());
        self.commands.drain(..take).collect()
    }

    /// Feed one driver event back in.
    ///
    /// Total by construction: an event outside the running lifecycle is
    /// dropped, because the state it would update has been cleared.
    pub fn ingest_driver_event(&mut self, event: KademliaEvent, now_ms: u64) {
        if !self.started || self.stopped {
            return;
        }
        self.clock = self.clock.max(now_ms);
        match event {
            KademliaEvent::QueryResults { candidates, class } => {
                // §9.3 counts a NEW USABLE ADDRESS for a routed peer as
                // progress, not only an admission. Judged BEFORE the
                // candidates are tracked, against what was known when
                // the round ran — tracking first would erase the very
                // difference being measured.
                let address_progress = candidates.as_slice().iter().any(|c| {
                    self.routing.contains(&c.peer_id)
                        && normalize::normalized_addresses(c, &self.local).is_some_and(
                            |addresses| {
                                let known = self.tracked.get(&c.peer_id);
                                addresses
                                    .iter()
                                    .any(|a| known.is_none_or(|t| !t.addresses.contains(a)))
                            },
                        )
                });
                self.settle(class, now_ms, true, address_progress);
                self.recent_queries_succeeded = true;
                for candidate in candidates.as_slice() {
                    self.track(candidate, now_ms);
                }
            }
            KademliaEvent::RoutingPeerAdded { peer } => {
                // F2: an insertion on an EMPTY table starts one bootstrap
                // the provider never scheduled, and it dials — so it is
                // charged like the work it is, past the ceilings if it
                // must be, and the slot is settled by the bootstrap-class
                // completion the driver reports for it.
                if self.routing.is_empty() && self.implicit_charge.is_none() {
                    self.implicit_charge = Some(self.budgets.charge_unscheduled(now_ms));
                }
                self.routing.insert(peer);
                self.admissions += 1;
                // Progress: the backed-off pace no longer describes the
                // situation, and neither does any saturation.
                self.no_progress_rounds = 0;
                self.pacing.reset_exploration();
            }
            KademliaEvent::RoutingPeerRemoved { peer } => {
                if self.routing.remove(&peer) {
                    // §9.3: routing loss invalidates saturation.
                    self.no_progress_rounds = 0;
                }
            }
            KademliaEvent::QueryFailed { reason, class } => {
                self.settle(class, now_ms, false, false);
                match reason {
                    QueryFailure::TimedOut | QueryFailure::NoRoutingPeers => {
                        self.recent_queries_succeeded = false;
                    }
                    // A refused budget or a shutdown is scheduling, not
                    // the network failing; it says nothing about query
                    // health.
                    QueryFailure::BudgetExhausted | QueryFailure::ShuttingDown => {}
                }
            }
        }
    }

    /// Settle the budget slot and pacing for one completed query.
    fn settle(&mut self, class: QueryClass, now_ms: u64, succeeded: bool, address_progress: bool) {
        let was_commanded = self.budgets.finish_oldest(class);
        if class == QueryClass::Bootstrap
            && !was_commanded
            && let Some(charge) = self.implicit_charge.take()
        {
            // The implicit bootstrap finished: its slot comes back, its
            // rate charge stays spent — the window really was used.
            self.budgets.consume(charge);
        }
        if class == QueryClass::Exploration
            && let Some(snapshot) = self.exploration_snapshot.take()
        {
            // §9.3: only a SUCCESSFUL round that admitted nothing AND
            // surfaced no new address counts toward saturation; a failed
            // round proves nothing about the network being exhausted.
            if address_progress {
                // A new usable address for a routed peer is progress:
                // the backed-off pace no longer describes the situation.
                self.no_progress_rounds = 0;
                self.pacing.reset_exploration();
            } else if succeeded && self.admissions == snapshot {
                self.no_progress_rounds = self.no_progress_rounds.saturating_add(1);
            }
            // Schedule the next round from the count as it now stands,
            // so a no-progress increment lengthens the very next wait.
            self.pacing.schedule_exploration(
                now_ms,
                &self.routing_view(),
                self.config.exploration_interval_ms,
                self.config.exploration_jitter_percent,
                self.exploration_entropy,
            );
        }
    }

    /// Start one exploration round if the view warrants one (§9.3).
    ///
    /// `entropy` must be fresh cryptographic randomness from the caller:
    /// it becomes the 32-byte lookup key — independent per query, never
    /// derived from any application identity, never persisted — and its
    /// first bytes drive the jitter. Returns whether a query was issued.
    pub fn tick(&mut self, now_ms: u64, entropy: [u8; 32]) -> bool {
        if !self.started || self.stopped {
            return false;
        }
        self.clock = self.clock.max(now_ms);
        if self.exploration_snapshot.is_some() {
            return false;
        }
        let view = self.routing_view();
        // While the view is neither target-satisfied nor validly
        // saturated, explore; otherwise rest.
        if view.is_target_satisfied() || (view.is_saturated() && self.saturation_conjuncts_hold()) {
            return false;
        }
        if self.routing.is_empty() {
            // Nothing to query through; bootstrap and seeding come first.
            return false;
        }
        if !self.pacing.exploration_due(now_ms) {
            return false;
        }
        let Ok(permit) = self.budgets.acquire(now_ms) else {
            return false;
        };
        let mut jitter_bytes = [0_u8; 8];
        jitter_bytes.copy_from_slice(&entropy[..8]);
        self.exploration_entropy = u64::from_le_bytes(jitter_bytes);
        self.queue_command(KademliaCommand::StartQuery {
            class: QueryClass::Exploration,
            key: entropy,
        });
        self.budgets.bind(permit, QueryClass::Exploration);
        self.exploration_snapshot = Some(self.admissions);
        true
    }

    /// Request one bootstrap (§9.1), spaced by `bootstrap_min_interval`.
    ///
    /// # Errors
    /// [`BootstrapRefusal`] naming what refused.
    pub fn request_bootstrap(&mut self, now_ms: u64) -> Result<(), BootstrapRefusal> {
        if !self.started || self.stopped {
            return Err(BootstrapRefusal::NotRunning);
        }
        self.clock = self.clock.max(now_ms);
        let Some(key) = self.local_key else {
            return Err(BootstrapRefusal::UntargetableSelf);
        };
        if !self
            .pacing
            .bootstrap_allowed(now_ms, self.config.bootstrap_min_interval_ms)
        {
            return Err(BootstrapRefusal::TooSoon);
        }
        let Ok(permit) = self.budgets.acquire(now_ms) else {
            return Err(BootstrapRefusal::BudgetExhausted);
        };
        self.queue_command(KademliaCommand::StartQuery {
            class: QueryClass::Bootstrap,
            key,
        });
        self.budgets.bind(permit, QueryClass::Bootstrap);
        self.pacing.record_bootstrap(now_ms);
        Ok(())
    }

    /// When the next exploration round is due, if one is scheduled.
    ///
    /// For the composition root's own scheduling: `None` means the next
    /// eligible [`Self::tick`] may explore immediately.
    #[must_use]
    pub fn next_exploration_due_ms(&self) -> Option<u64> {
        self.pacing.next_exploration_due_ms()
    }

    /// Whether the periodic bootstrap refresh is due (§9.1).
    #[must_use]
    pub fn bootstrap_refresh_due(&self, now_ms: u64) -> bool {
        self.pacing
            .bootstrap_refresh_due(now_ms, self.config.bootstrap_refresh_interval_ms)
    }

    /// §9.3's extra saturation conjuncts, beyond the round count: a
    /// routing peer to rest on, no fresh targetable server evidence
    /// waiting OUTSIDE the routing set, and recent query health.
    ///
    /// The blocking observation must be IMMEDIATELY targetable, so the
    /// §9.2 conditions this provider can judge from its own state
    /// apply: trust, fresh positive evidence, a recoverable lookup key,
    /// and an elapsed cooldown. Two stay out deliberately. The budget
    /// is momentary — saturation is a resting state, and a full window
    /// this second says nothing about the next. Address usability is
    /// the composer's knowledge, and the conservative reading — keep
    /// exploring — is the safe direction to be wrong in.
    fn saturation_conjuncts_hold(&self) -> bool {
        !self.routing.is_empty()
            && self.recent_queries_succeeded
            && !self.evidence.iter().any(|(peer, e)| {
                e.supported
                    && e.expires_at > self.clock
                    && self.trusted.contains(peer)
                    && !self.routing.contains(peer)
                    && normalize::targeted_lookup_key(peer).is_some()
                    && self.cooldowns.get(peer).is_none_or(|last| {
                        last.saturating_add(self.config.targeted_lookup_cooldown_ms) <= self.clock
                    })
            })
    }

    /// Request one targeted lookup of `target` (§9.2).
    ///
    /// The one conjunct the provider cannot judge arrives as an
    /// argument: whether a usable address already exists is the
    /// aggregate manager's knowledge (backoff lives with dial policy,
    /// not here). Everything else — the budget included — is checked
    /// against this provider's own state, one conjunct at a time, so a
    /// refusal names exactly what was missing.
    ///
    /// # Errors
    /// The first failed conjunct, as a [`TargetedRefusal`].
    pub fn request_targeted_lookup(
        &mut self,
        target: &TransportIdentity,
        now_ms: u64,
        usable_address_exists: bool,
    ) -> Result<(), TargetedRefusal> {
        if !self.started || self.stopped {
            return Err(TargetedRefusal::NotRunning);
        }
        self.clock = self.clock.max(now_ms);
        if *target == self.local {
            return Err(TargetedRefusal::SelfTarget);
        }
        if !self.trusted.contains(target) {
            return Err(TargetedRefusal::NotTrusted);
        }
        let evidence = self
            .evidence
            .get(target)
            .ok_or(TargetedRefusal::NoServerEvidence)?;
        if evidence.expires_at <= now_ms {
            return Err(TargetedRefusal::StaleServerEvidence);
        }
        if !evidence.supported {
            return Err(TargetedRefusal::NegativeServerEvidence);
        }
        if usable_address_exists {
            return Err(TargetedRefusal::UsableAddressExists);
        }
        if let Some(last) = self.cooldowns.get(target)
            && last.saturating_add(self.config.targeted_lookup_cooldown_ms) > now_ms
        {
            return Err(TargetedRefusal::CooldownActive);
        }
        // The permit comes BEFORE the command exists: a budget consulted
        // afterwards records a decision that has already been made.
        let Ok(permit) = self.budgets.acquire(now_ms) else {
            return Err(TargetedRefusal::BudgetExhausted);
        };
        let Some(key) = normalize::targeted_lookup_key(target) else {
            // Nothing was started, so nothing was spent: the refund is
            // what keeps a retried untargetable identity from exhausting
            // the rate window with zero queries run.
            self.budgets.release(permit);
            return Err(TargetedRefusal::NotTargetableIdentity);
        };
        self.queue_command(KademliaCommand::StartQuery {
            class: QueryClass::Targeted,
            key,
        });
        self.budgets.bind(permit, QueryClass::Targeted);
        self.cooldowns.insert(target.clone(), now_ms);
        Ok(())
    }

    /// Record one query-result candidate, replacing what was tracked.
    fn track(&mut self, candidate: &CandidatePeer, now_ms: u64) {
        let Some(addresses) = normalize::normalized_addresses(candidate, &self.local) else {
            return;
        };
        if addresses.is_empty() {
            // Nothing reachable survived normalization; an observation
            // with no addresses gives the consumer nothing to do.
            return;
        }
        let entry = Tracked {
            observed_at: now_ms,
            expires_at: now_ms.saturating_add(self.config.candidate_ttl_ms),
            addresses,
        };
        if !self.tracked.contains_key(&candidate.peer_id)
            && self.tracked.len() >= MAX_TRACKED_CANDIDATES
        {
            let soonest = self
                .tracked
                .iter()
                .min_by_key(|(_, t)| t.expires_at)
                .map(|(p, _)| p.clone());
            if let Some(peer) = soonest {
                self.tracked.remove(&peer);
            }
        }
        self.tracked.insert(candidate.peer_id.clone(), entry);
    }

    /// Record capability evidence, freshest observation winning.
    ///
    /// Recency decides, not sign: a NEWER negative replaces an older
    /// positive, which is what makes withdrawn server mode stick instead
    /// of being outvoted by stale optimism.
    fn record_evidence(&mut self, peer: &TransportIdentity, incoming: Evidence) {
        if *peer == self.local {
            return;
        }
        if let Some(held) = self.evidence.get(peer) {
            if incoming.observed_at >= held.observed_at {
                self.evidence.insert(peer.clone(), incoming);
            }
            return;
        }
        if self.evidence.len() >= MAX_CAPABILITY_EVIDENCE {
            let stalest = self
                .evidence
                .iter()
                .min_by_key(|(_, e)| e.observed_at)
                .map(|(p, e)| (p.clone(), e.observed_at));
            match stalest {
                Some((victim, held_at)) if held_at < incoming.observed_at => {
                    self.evidence.remove(&victim);
                }
                // The arrival is the stalest thing in sight; dropping it
                // keeps what is fresher.
                _ => return,
            }
        }
        self.evidence.insert(peer.clone(), incoming);
    }

    /// Queue a command. Offers past the bound displace the OLDEST offer;
    /// everything else is appended untouched.
    ///
    /// Only offers are displaceable. `SetMode` and `Shutdown` are
    /// control commands; a `StartQuery` carries a budget slot bound to
    /// it, and silently dropping one would leave that slot held for a
    /// completion that can never arrive. Queries need no displacement
    /// bound of their own — the budgets cap how many can be outstanding.
    fn queue_command(&mut self, command: KademliaCommand) {
        let offer = |c: &KademliaCommand| matches!(c, KademliaCommand::OfferRoutingPeer { .. });
        if matches!(command, KademliaCommand::OfferRoutingPeer { .. }) {
            let offers = self.commands.iter().filter(|c| offer(c)).count();
            if offers >= MAX_PENDING_COMMANDS
                && let Some(idx) = self.commands.iter().position(offer)
            {
                self.commands.remove(idx);
            }
        }
        self.commands.push_back(command);
    }

    /// The health this provider would report right now.
    fn health_now(&self) -> ProviderHealth {
        health::provider_health(
            self.started,
            self.stopped,
            self.config.mode,
            &self.routing_view(),
            self.recent_queries_succeeded,
            self.saturation_conjuncts_hold(),
        )
    }
}

impl DiscoveryProvider for KademliaDiscovery {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            name: SOURCE.to_owned(),
            interface_version: INTERFACE_VERSION.to_owned(),
            config_version: Some(CONFIG_VERSION.to_owned()),
            scope: ProviderScope::Network,
            mode: ProviderMode::Active,
            supports_expiry: true,
            supports_hints: true,
        }
    }

    fn start(&mut self, _now_ms: u64) -> Result<(), ProviderError> {
        if self.started {
            return Err(ProviderError::AlreadyStarted);
        }
        self.started = true;
        // Explicit, never inferred (§5): the first thing the driver hears
        // is which mode this node is in.
        self.commands.push_back(KademliaCommand::SetMode {
            mode: self.config.mode,
        });
        Ok(())
    }

    fn drain_events(&mut self, now_ms: u64, max: usize) -> Vec<DiscoveryEvent> {
        if !self.started || self.stopped {
            return Vec::new();
        }
        self.clock = self.clock.max(now_ms);
        let mut out = Vec::new();

        // Health first: the manager learns health only from this event.
        let health = self.health_now();
        if self.last_reported_health != Some(health) && out.len() < max {
            out.push(DiscoveryEvent::HealthChanged {
                source: SOURCE.to_owned(),
                health,
            });
            self.last_reported_health = Some(health);
        }

        // TTL sweep: what queries stopped refreshing, expiry removes.
        self.tracked.retain(|_, t| t.expires_at > now_ms);

        // Then the outstanding difference, DERIVED FROM STATE: `emitted`
        // advances only for events actually returned, so anything `max`
        // cuts off is recomputed on the next drain rather than lost.
        let gone: Vec<TransportIdentity> = self
            .emitted
            .keys()
            .filter(|p| !self.tracked.contains_key(*p))
            .cloned()
            .collect();
        for peer_id in gone {
            if out.len() >= max {
                return out;
            }
            self.emitted.remove(&peer_id);
            out.push(DiscoveryEvent::CandidateExpired {
                peer_id,
                source: SOURCE.to_owned(),
                // Empty retracts the whole (peer, source) candidate:
                // kademlia observations are whole-peer, and only the
                // "kademlia" provenance goes — whether the aggregate peer
                // survives is the manager's decision.
                addresses: BTreeSet::new(),
            });
        }
        let changed: Vec<(TransportIdentity, Tracked)> = self
            .tracked
            .iter()
            .filter(|(p, t)| self.emitted.get(*p) != Some(*t))
            .map(|(p, t)| (p.clone(), t.clone()))
            .collect();
        for (peer_id, entry) in changed {
            if out.len() >= max {
                return out;
            }
            out.push(DiscoveryEvent::CandidateObserved {
                candidate: Box::new(CandidatePeer {
                    peer_id: peer_id.clone(),
                    addresses: entry.addresses.clone(),
                    source: SOURCE.to_owned(),
                    observed_at: entry.observed_at,
                    expires_at: Some(entry.expires_at),
                    protocol_observations: BTreeSet::new(),
                }),
            });
            self.emitted.insert(peer_id, entry);
        }
        out
    }

    fn add_hint(&mut self, hint: PeerHint, now_ms: u64) -> HintDisposition {
        // The lifecycle order in `providers/kademlia.md` accepts hints
        // AFTER mode is set: outside the running window there is no
        // driver to offer anything to.
        if !self.started || self.stopped {
            return HintDisposition::Unsupported;
        }
        match hint {
            PeerHint::ObservedProtocol {
                peer_id,
                protocol_id,
                supported,
                observed_at,
            } => {
                if protocol_id != self.expected_protocol {
                    // Evidence about another protocol — or another
                    // network's Kademlia — is not this provider's to
                    // hold. The comparison is the FULL id, never a
                    // prefix: a wrong-network peer must not become
                    // targetable here.
                    return HintDisposition::Unsupported;
                }
                // JUDGED AT DELIVERY TIME. A hint that claims the
                // future would out-recency every real observation until
                // that time arrives: a forged negative becomes a
                // suppression lever, a forged positive stale optimism
                // with an artificial life. The cache provider clamps the
                // same way.
                let observed_at = observed_at.min(now_ms);
                self.record_evidence(
                    &peer_id,
                    Evidence {
                        supported,
                        observed_at,
                        expires_at: observed_at.saturating_add(self.config.candidate_ttl_ms),
                    },
                );
                // §9.3: a new capability observation invalidates
                // saturation — there may be someone new to find.
                self.no_progress_rounds = 0;
                HintDisposition::Accepted
            }
            PeerHint::ObservedReachable {
                peer_id, address, ..
            } => {
                if peer_id == self.local {
                    // Advisory no-op: self is never offered to routing.
                    return HintDisposition::Accepted;
                }
                let Ok(parsed) = OfferedAddress::parse(&address) else {
                    return HintDisposition::Rejected(DiscoveryError::InvalidLength {
                        field: "address",
                        got: address.len(),
                        max: MAX_ADDRESS_BYTES,
                    });
                };
                let addresses = match OfferedAddresses::new([parsed]) {
                    Ok(a) => a,
                    Err(_) => {
                        return HintDisposition::Rejected(DiscoveryError::InvalidLength {
                            field: "address",
                            got: address.len(),
                            max: MAX_ADDRESS_BYTES,
                        });
                    }
                };
                self.queue_command(KademliaCommand::OfferRoutingPeer {
                    addresses,
                    peer: peer_id,
                });
                // §9.3: a new external seed invalidates saturation.
                self.no_progress_rounds = 0;
                HintDisposition::Accepted
            }
            PeerHint::CandidateHint(candidate) => {
                if candidate.source == SOURCE {
                    // §8's feedback rule: a kademlia-derived candidate is
                    // already inside the driver's state, and re-seeding it
                    // would launder DHT output into external evidence.
                    return HintDisposition::Accepted;
                }
                if let Err(err) = candidate.validate() {
                    return HintDisposition::Rejected(err);
                }
                if candidate.peer_id == self.local {
                    return HintDisposition::Accepted;
                }
                for observation in &candidate.protocol_observations {
                    if observation.protocol_id == self.expected_protocol {
                        // Clamped at delivery like the bare hint above,
                        // and the expiry is the EARLIER of the record's
                        // word and the observation's own age: a
                        // long-lived record must not keep an old
                        // observation eligible — the same class as the
                        // cache export's own-age bound.
                        let observed_at = observation.observed_at.min(now_ms);
                        self.record_evidence(
                            &candidate.peer_id,
                            Evidence {
                                supported: observation.supported,
                                observed_at,
                                expires_at: observed_at
                                    .saturating_add(self.config.candidate_ttl_ms)
                                    .min(candidate.expires_at.unwrap_or(u64::MAX)),
                            },
                        );
                    }
                }
                if !candidate.addresses.is_empty() {
                    // The port's own bounded parse — `parse_all` exists
                    // for exactly this caller, and keeps the length
                    // check ahead of the copy.
                    let Ok(addresses) =
                        OfferedAddresses::parse_all(candidate.addresses.iter().map(String::as_str))
                    else {
                        return HintDisposition::Rejected(DiscoveryError::TooManyItems {
                            field: "addresses",
                            got: candidate.addresses.len(),
                            max: interweave_discovery_api::MAX_ADDRESSES,
                        });
                    };
                    self.queue_command(KademliaCommand::OfferRoutingPeer {
                        addresses,
                        peer: candidate.peer_id.clone(),
                    });
                }
                // §9.3: a new external seed invalidates saturation.
                self.no_progress_rounds = 0;
                HintDisposition::Accepted
            }
        }
    }

    fn health(&self) -> ProviderHealth {
        self.health_now()
    }

    fn shutdown(&mut self, _now_ms: u64) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        // Pending advice is for a driver that is going away; the one
        // command still worth delivering is the stop itself.
        self.commands.clear();
        self.commands.push_back(KademliaCommand::Shutdown);
        self.tracked.clear();
        self.emitted.clear();
        self.evidence.clear();
        self.cooldowns.clear();
        self.routing.clear();
        self.exploration_snapshot = None;
        if let Some(charge) = self.implicit_charge.take() {
            self.budgets.consume(charge);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interweave_discovery_api::ProtocolObservation;

    /// The fixture golden hash for `example-private-network`.
    const HASH: &str = "ssbtblqj7mexczivog5qfbfjvi";
    const TTL_MS: u64 = 30 * 60 * 1000;
    const COOLDOWN_MS: u64 = 5 * 60 * 1000;

    /// A synthetic PeerId that DECODES: the identity-multihash envelope
    /// of a libp2p Ed25519 public-key protobuf, with only the key bytes
    /// varying. Same construction as the mdns provider's tests, for the
    /// same reason — a hand-spelled tail is a string no parser accepts.
    fn synthetic_peer(n: u64) -> TransportIdentity {
        let mut bytes = [0_u8; 38];
        bytes[..6].copy_from_slice(&[0x00, 0x24, 0x08, 0x01, 0x12, 0x20]);
        bytes[6..14].copy_from_slice(&n.to_be_bytes());
        TransportIdentity::parse(bs58::encode(bytes).into_string()).expect("valid identity")
    }

    fn config(mode: KademliaMode) -> KademliaProviderConfig {
        KademliaProviderConfig {
            mode,
            wire_major: 1,
            network_hash: HASH.to_owned(),
            candidate_ttl_ms: TTL_MS,
            targeted_lookup_cooldown_ms: COOLDOWN_MS,
            target_routing_peers: 64,
            max_routing_peers: 256,
            exploration_interval_ms: 60_000,
            exploration_jitter_percent: 0,
            max_concurrent_queries: 2,
            max_queries_per_minute: 6,
            bootstrap_min_interval_ms: 300_000,
            bootstrap_refresh_interval_ms: 900_000,
        }
    }

    /// A completion event of `class` carrying no candidates.
    fn done(class: QueryClass) -> KademliaEvent {
        KademliaEvent::QueryResults {
            candidates: interweave_kademlia_control_api::ObservedCandidates::new([])
                .expect("empty is bounded"),
            class,
        }
    }

    /// A provider with one routed peer and its F2 charge already
    /// settled, so budget tests start from a clean slate.
    fn routed(p: &mut KademliaDiscovery, peer: &TransportIdentity, now_ms: u64) {
        p.ingest_driver_event(
            KademliaEvent::RoutingPeerAdded { peer: peer.clone() },
            now_ms,
        );
        p.ingest_driver_event(done(QueryClass::Bootstrap), now_ms);
    }

    fn local() -> TransportIdentity {
        synthetic_peer(0)
    }

    fn started(mode: KademliaMode) -> KademliaDiscovery {
        let mut p = KademliaDiscovery::new(config(mode), local()).expect("valid config");
        p.start(0).expect("starts");
        p
    }

    fn results(peers: &[(TransportIdentity, &str)]) -> KademliaEvent {
        let candidates = peers
            .iter()
            .map(|(peer_id, address)| CandidatePeer {
                peer_id: peer_id.clone(),
                addresses: [(*address).to_owned()].into_iter().collect(),
                source: SOURCE.to_owned(),
                observed_at: 0,
                expires_at: None,
                protocol_observations: BTreeSet::new(),
            })
            .collect::<Vec<_>>();
        KademliaEvent::QueryResults {
            candidates: interweave_kademlia_control_api::ObservedCandidates::new(candidates)
                .expect("bounded"),
            class: QueryClass::Exploration,
        }
    }

    fn expected_id(p: &KademliaDiscovery) -> ProtocolId {
        p.expected_protocol().clone()
    }

    /// Positive fresh evidence for `peer`, observed at `at`.
    fn give_evidence(p: &mut KademliaDiscovery, peer: &TransportIdentity, at: u64) {
        let disposition = p.add_hint(
            PeerHint::ObservedProtocol {
                peer_id: peer.clone(),
                protocol_id: expected_id(p),
                supported: true,
                observed_at: at,
            },
            at,
        );
        assert_eq!(disposition, HintDisposition::Accepted);
    }

    fn trust(p: &mut KademliaDiscovery, peers: &[&TransportIdentity]) {
        p.set_remote_trusted(peers.iter().map(|q| (*q).clone()).collect());
    }

    fn observations(events: &[DiscoveryEvent]) -> Vec<&CandidatePeer> {
        events
            .iter()
            .filter_map(|e| match e {
                DiscoveryEvent::CandidateObserved { candidate } => Some(candidate.as_ref()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_descriptor_validates_and_names_the_source() {
        let p = started(KademliaMode::Client);
        let d = p.descriptor();
        d.validate().expect("well-formed descriptor");
        assert_eq!(d.name, "kademlia");
        assert_eq!(d.scope, ProviderScope::Network);
        assert_eq!(d.mode, ProviderMode::Active);
        assert!(d.supports_expiry);
        assert!(d.supports_hints);
    }

    #[test]
    fn a_config_outside_the_bounds_is_refused() {
        let base = config(KademliaMode::Client);
        let cases: Vec<(KademliaProviderConfig, ProviderConfigError)> = vec![
            (
                KademliaProviderConfig {
                    wire_major: 0,
                    ..base.clone()
                },
                ProviderConfigError::ZeroWireMajor,
            ),
            (
                KademliaProviderConfig {
                    network_hash: "SSBTBLQJ7MEXCZIVOG5QFBFJVI".to_owned(),
                    ..base.clone()
                },
                ProviderConfigError::MalformedNetworkHash,
            ),
            (
                KademliaProviderConfig {
                    network_hash: "abc".to_owned(),
                    ..base.clone()
                },
                ProviderConfigError::MalformedNetworkHash,
            ),
            (
                KademliaProviderConfig {
                    candidate_ttl_ms: 0,
                    ..base.clone()
                },
                ProviderConfigError::ZeroCandidateTtl,
            ),
            (
                KademliaProviderConfig {
                    target_routing_peers: 300,
                    ..base.clone()
                },
                ProviderConfigError::TargetAboveMax,
            ),
            (
                KademliaProviderConfig {
                    target_routing_peers: 2000,
                    max_routing_peers: 2000,
                    ..base.clone()
                },
                ProviderConfigError::MaxAboveCeiling { got: 2000 },
            ),
        ];
        for (bad, want) in cases {
            assert_eq!(KademliaDiscovery::new(bad, local()).unwrap_err(), want);
        }
    }

    #[test]
    fn the_expected_protocol_matches_the_frozen_fixture() {
        // The drift check between this crate's rendering and the cache's:
        // both are tested against the SAME frozen vectors, so the formula
        // cannot fork without one of the two tests failing.
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/kademlia/kad-network-namespace-v1.json"
        ))
        .expect("fixture parses");
        let vectors = fixture["vectors"].as_array().expect("vectors");
        assert!(!vectors.is_empty(), "an empty fixture proves nothing");
        for v in vectors {
            let hash = v["network_hash"].as_str().expect("hash");
            let protocol = v["protocol"].as_str().expect("protocol");
            let p = KademliaDiscovery::new(
                KademliaProviderConfig {
                    network_hash: hash.to_owned(),
                    ..config(KademliaMode::Client)
                },
                local(),
            )
            .expect("fixture hash is canonical");
            assert_eq!(p.expected_protocol().as_str(), protocol);
        }
    }

    #[test]
    fn start_is_once_and_commands_the_configured_mode() {
        let mut p = started(KademliaMode::Server);
        assert_eq!(p.start(1).unwrap_err(), ProviderError::AlreadyStarted);
        let commands = p.drain_commands(usize::MAX);
        assert_eq!(
            commands,
            vec![KademliaCommand::SetMode {
                mode: KademliaMode::Server
            }],
            "mode is explicit, never inferred"
        );
    }

    #[test]
    fn health_transitions_are_reported_once_each() {
        let mut p = started(KademliaMode::Client);
        let first = p.drain_events(1, usize::MAX);
        assert_eq!(
            first,
            vec![DiscoveryEvent::HealthChanged {
                source: SOURCE.to_owned(),
                health: ProviderHealth::Unavailable,
            }],
            "with zero trusted peers the provider cannot become healthy"
        );
        assert!(
            p.drain_events(2, usize::MAX).is_empty(),
            "an unchanged health is not re-reported"
        );
        let peer = synthetic_peer(1);
        trust(&mut p, &[&peer]);
        assert_eq!(
            p.drain_events(3, usize::MAX),
            vec![DiscoveryEvent::HealthChanged {
                source: SOURCE.to_owned(),
                health: ProviderHealth::Degraded,
            }],
            "a trusted population makes health reachable, so the change is reported"
        );
    }

    #[test]
    fn a_query_result_becomes_a_kademlia_candidate() {
        let mut p = started(KademliaMode::Client);
        let peer = synthetic_peer(1);
        p.ingest_driver_event(results(&[(peer.clone(), "/ip4/192.0.2.1/tcp/4001")]), 5_000);
        let events = p.drain_events(5_000, usize::MAX);
        let obs = observations(&events);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].peer_id, peer, "the candidate is the observed peer");
        assert_eq!(obs[0].source, SOURCE);
        assert_eq!(obs[0].observed_at, 5_000);
        assert_eq!(
            obs[0].expires_at,
            Some(5_000 + TTL_MS),
            "provenance TTL from the observation time"
        );
        assert!(obs[0].addresses.contains("/ip4/192.0.2.1/tcp/4001"));
    }

    #[test]
    fn the_local_peer_is_never_emitted() {
        let mut p = started(KademliaMode::Client);
        let other = synthetic_peer(1);
        p.ingest_driver_event(
            results(&[
                (local(), "/ip4/192.0.2.9/tcp/4001"),
                (other.clone(), "/ip4/192.0.2.1/tcp/4001"),
            ]),
            5_000,
        );
        let events = p.drain_events(5_000, usize::MAX);
        let obs = observations(&events);
        assert_eq!(obs.len(), 1, "self is discarded, the other peer is not");
        assert_eq!(obs[0].peer_id, other);
    }

    #[test]
    fn expiry_retracts_the_candidate_it_tracked() {
        let mut p = started(KademliaMode::Client);
        let peer = synthetic_peer(1);
        p.ingest_driver_event(results(&[(peer.clone(), "/ip4/192.0.2.1/tcp/4001")]), 1_000);
        p.drain_events(1_000, usize::MAX);
        let events = p.drain_events(1_000 + TTL_MS + 1, usize::MAX);
        let expiries: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                DiscoveryEvent::CandidateExpired {
                    peer_id,
                    source,
                    addresses,
                } => Some((peer_id, source, addresses)),
                _ => None,
            })
            .collect();
        assert_eq!(expiries.len(), 1);
        assert_eq!(*expiries[0].0, peer, "the expiry names the tracked peer");
        assert_eq!(
            expiries[0].1, SOURCE,
            "only kademlia provenance is retracted"
        );
        assert!(
            expiries[0].2.is_empty(),
            "empty addresses retract the whole (peer, source) candidate"
        );
    }

    #[test]
    fn a_refreshed_observation_extends_expiry() {
        let mut p = started(KademliaMode::Client);
        let peer = synthetic_peer(1);
        p.ingest_driver_event(results(&[(peer.clone(), "/ip4/192.0.2.1/tcp/4001")]), 1_000);
        p.drain_events(1_000, usize::MAX);
        p.ingest_driver_event(results(&[(peer.clone(), "/ip4/192.0.2.1/tcp/4001")]), 9_000);
        let refreshed = p.drain_events(9_000, usize::MAX);
        let obs = observations(&refreshed);
        assert_eq!(obs.len(), 1, "a refresh is re-emitted, not silent");
        assert_eq!(obs[0].expires_at, Some(9_000 + TTL_MS));
        let at_old_deadline = p.drain_events(1_000 + TTL_MS + 1, usize::MAX);
        assert!(
            !at_old_deadline
                .iter()
                .any(|e| matches!(e, DiscoveryEvent::CandidateExpired { .. })),
            "the old deadline no longer expires a refreshed candidate"
        );
    }

    #[test]
    fn the_tracked_bound_evicts_the_soonest_expiring() {
        let mut p = started(KademliaMode::Client);
        // The first peer is observed earliest, so its expiry is soonest.
        let victim = synthetic_peer(1);
        p.ingest_driver_event(results(&[(victim.clone(), "/ip4/192.0.2.1/tcp/1")]), 1_000);
        let mut n = 2_u64;
        while (p.tracked.len()) < MAX_TRACKED_CANDIDATES {
            let batch: Vec<(TransportIdentity, String)> = (0..20)
                .map(|i| {
                    (
                        synthetic_peer(n + i),
                        format!("/ip4/198.51.100.1/tcp/{}", n + i),
                    )
                })
                .collect();
            let refs: Vec<(TransportIdentity, &str)> = batch
                .iter()
                .map(|(peer, a)| (peer.clone(), a.as_str()))
                .collect();
            p.ingest_driver_event(results(&refs), 2_000);
            n += 20;
        }
        assert_eq!(p.tracked.len(), MAX_TRACKED_CANDIDATES);
        let newcomer = synthetic_peer(n);
        p.ingest_driver_event(
            results(&[(newcomer.clone(), "/ip4/198.51.100.2/tcp/1")]),
            3_000,
        );
        assert_eq!(p.tracked.len(), MAX_TRACKED_CANDIDATES, "the bound holds");
        assert!(
            !p.tracked.contains_key(&victim),
            "the soonest-expiring entry made room"
        );
        assert!(p.tracked.contains_key(&newcomer));
    }

    #[test]
    fn an_eligible_targeted_lookup_issues_the_query() {
        let mut p = started(KademliaMode::Client);
        let target = synthetic_peer(7);
        trust(&mut p, &[&target]);
        give_evidence(&mut p, &target, 1_000);
        p.drain_commands(usize::MAX);
        p.request_targeted_lookup(&target, 2_000, false)
            .expect("all five conjuncts hold");
        let commands = p.drain_commands(usize::MAX);
        let mut want_key = [0_u8; 32];
        want_key[..8].copy_from_slice(&7_u64.to_be_bytes());
        assert_eq!(
            commands,
            vec![KademliaCommand::StartQuery {
                class: QueryClass::Targeted,
                key: want_key,
            }],
            "the key is the target's Ed25519 public key"
        );
        assert_eq!(
            p.request_targeted_lookup(&target, 2_001, false),
            Err(TargetedRefusal::CooldownActive),
            "issuing records the cooldown"
        );
    }

    #[test]
    fn each_targeted_conjunct_refuses_alone() {
        let mut p = started(KademliaMode::Client);
        let target = synthetic_peer(7);

        assert_eq!(
            p.request_targeted_lookup(&target, 1_000, false),
            Err(TargetedRefusal::NotTrusted),
            "conjunct 1: trust"
        );
        trust(&mut p, &[&target]);
        assert_eq!(
            p.request_targeted_lookup(&target, 1_000, false),
            Err(TargetedRefusal::NoServerEvidence),
            "conjunct 2: evidence must exist"
        );
        give_evidence(&mut p, &target, 1_000);
        assert_eq!(
            p.request_targeted_lookup(&target, 1_000 + TTL_MS, false),
            Err(TargetedRefusal::StaleServerEvidence),
            "conjunct 2: evidence must be fresh"
        );
        assert_eq!(
            p.request_targeted_lookup(&target, 2_000, true),
            Err(TargetedRefusal::UsableAddressExists),
            "conjunct 3: a usable address makes the lookup unnecessary"
        );
        let held_a = p.budgets.acquire(2_000).expect("slot");
        let held_b = p.budgets.acquire(2_000).expect("slot");
        assert_eq!(
            p.request_targeted_lookup(&target, 2_000, false),
            Err(TargetedRefusal::BudgetExhausted),
            "conjunct 5: the global budget"
        );
        p.budgets.release(held_a);
        p.budgets.release(held_b);
        assert_eq!(
            p.request_targeted_lookup(&local(), 2_000, false),
            Err(TargetedRefusal::SelfTarget)
        );
        p.request_targeted_lookup(&target, 2_000, false)
            .expect("the control: with every conjunct held, the lookup runs");
    }

    #[test]
    fn a_newer_negative_overrides_the_older_positive() {
        let mut p = started(KademliaMode::Client);
        let target = synthetic_peer(7);
        trust(&mut p, &[&target]);
        give_evidence(&mut p, &target, 1_000);
        let disposition = p.add_hint(
            PeerHint::ObservedProtocol {
                peer_id: target.clone(),
                protocol_id: expected_id(&p),
                supported: false,
                observed_at: 1_500,
            },
            1_500,
        );
        assert_eq!(disposition, HintDisposition::Accepted);
        assert_eq!(
            p.request_targeted_lookup(&target, 2_000, false),
            Err(TargetedRefusal::NegativeServerEvidence),
            "recency decides, not sign: withdrawn server mode sticks"
        );
        // And the reverse arrival order changes nothing: the older
        // positive does not resurrect over the newer negative.
        let replay = p.add_hint(
            PeerHint::ObservedProtocol {
                peer_id: target.clone(),
                protocol_id: expected_id(&p),
                supported: true,
                observed_at: 1_200,
            },
            2_100,
        );
        assert_eq!(replay, HintDisposition::Accepted);
        assert_eq!(
            p.request_targeted_lookup(&target, 2_200, false),
            Err(TargetedRefusal::NegativeServerEvidence),
            "stale optimism does not outvote fresher withdrawal"
        );
    }

    #[test]
    fn wrong_network_or_major_evidence_never_qualifies() {
        let mut p = started(KademliaMode::Client);
        let target = synthetic_peer(7);
        trust(&mut p, &[&target]);
        for wrong in [
            // Same family, another network: a prefix comparison would
            // accept this, which is exactly the mutation this test kills.
            "/interweave/kad/1.0.0/ygneka5pm3tlc4zypofzfj4vsq",
            // Same network, another wire major.
            "/interweave/kad/2.0.0/ssbtblqj7mexczivog5qfbfjvi",
        ] {
            let disposition = p.add_hint(
                PeerHint::ObservedProtocol {
                    peer_id: target.clone(),
                    protocol_id: ProtocolId::parse(wrong).expect("printable"),
                    supported: true,
                    observed_at: 1_000,
                },
                1_000,
            );
            assert_eq!(
                disposition,
                HintDisposition::Unsupported,
                "evidence about {wrong} is not this provider's to hold"
            );
        }
        assert_eq!(
            p.request_targeted_lookup(&target, 1_500, false),
            Err(TargetedRefusal::NoServerEvidence),
            "neither hint became eligibility"
        );
    }

    #[test]
    fn a_qm_identity_is_not_targetable() {
        let mut p = started(KademliaMode::Client);
        let mut bytes = [0_u8; 34];
        bytes[..2].copy_from_slice(&[0x12, 0x20]);
        bytes[2] = 9;
        let target =
            TransportIdentity::parse(bs58::encode(bytes).into_string()).expect("valid Qm id");
        trust(&mut p, &[&target]);
        give_evidence(&mut p, &target, 1_000);
        assert_eq!(
            p.request_targeted_lookup(&target, 2_000, false),
            Err(TargetedRefusal::NotTargetableIdentity),
            "no recoverable key means refusing, not querying the wrong point"
        );
    }

    #[test]
    fn a_reachable_hint_forms_an_offer() {
        let mut p = started(KademliaMode::Client);
        p.drain_commands(usize::MAX);
        let peer = synthetic_peer(3);
        let disposition = p.add_hint(
            PeerHint::ObservedReachable {
                peer_id: peer.clone(),
                address: "/ip4/192.0.2.1/tcp/4001".to_owned(),
                observed_at: 1_000,
            },
            1_000,
        );
        assert_eq!(disposition, HintDisposition::Accepted);
        let commands = p.drain_commands(usize::MAX);
        let want = OfferedAddresses::new([
            OfferedAddress::parse("/ip4/192.0.2.1/tcp/4001").expect("bounded")
        ])
        .expect("one is bounded");
        assert_eq!(
            commands,
            vec![KademliaCommand::OfferRoutingPeer {
                addresses: want,
                peer,
            }],
            "the hint became exactly this offer"
        );
    }

    #[test]
    fn a_candidate_hint_offers_and_its_evidence_enables_targeting() {
        let mut p = started(KademliaMode::Client);
        p.drain_commands(usize::MAX);
        let peer = synthetic_peer(3);
        trust(&mut p, &[&peer]);
        let candidate = CandidatePeer {
            peer_id: peer.clone(),
            addresses: ["/ip4/192.0.2.1/tcp/4001".to_owned()].into_iter().collect(),
            source: "peer-cache".to_owned(),
            observed_at: 1_000,
            expires_at: Some(1_000 + TTL_MS),
            protocol_observations: [ProtocolObservation {
                protocol_id: expected_id(&p),
                supported: true,
                observed_at: 1_000,
            }]
            .into_iter()
            .collect(),
        };
        assert_eq!(
            p.add_hint(PeerHint::CandidateHint(Box::new(candidate)), 1_000),
            HintDisposition::Accepted
        );
        let commands = p.drain_commands(usize::MAX);
        assert!(
            matches!(&commands[..], [KademliaCommand::OfferRoutingPeer { peer: offered, .. }] if *offered == peer),
            "the seed became an offer"
        );
        p.request_targeted_lookup(&peer, 2_000, false)
            .expect("the carried observation is eligibility evidence");
    }

    #[test]
    fn a_kademlia_sourced_candidate_is_not_re_offered() {
        let mut p = started(KademliaMode::Client);
        p.drain_commands(usize::MAX);
        let peer = synthetic_peer(3);
        let candidate = CandidatePeer {
            peer_id: peer.clone(),
            addresses: ["/ip4/192.0.2.1/tcp/4001".to_owned()].into_iter().collect(),
            source: SOURCE.to_owned(),
            observed_at: 1_000,
            expires_at: None,
            protocol_observations: BTreeSet::new(),
        };
        assert_eq!(
            p.add_hint(PeerHint::CandidateHint(Box::new(candidate)), 1_000),
            HintDisposition::Accepted,
            "handled — by deliberately doing nothing"
        );
        assert!(
            p.drain_commands(usize::MAX).is_empty(),
            "DHT output does not launder into external seed evidence (§8)"
        );
    }

    #[test]
    fn hints_outside_the_running_lifecycle_are_unsupported() {
        let mut fresh =
            KademliaDiscovery::new(config(KademliaMode::Client), local()).expect("valid config");
        let hint = || PeerHint::ObservedReachable {
            peer_id: synthetic_peer(3),
            address: "/ip4/192.0.2.1/tcp/4001".to_owned(),
            observed_at: 1_000,
        };
        assert_eq!(fresh.add_hint(hint(), 1_000), HintDisposition::Unsupported);
        let mut p = started(KademliaMode::Client);
        p.shutdown(2_000);
        assert_eq!(p.add_hint(hint(), 3_000), HintDisposition::Unsupported);
    }

    #[test]
    fn the_command_bound_drops_the_oldest_advisory_never_control() {
        let mut p = started(KademliaMode::Client);
        // SetMode is still queued; flood past the advisory bound.
        for n in 0..(MAX_PENDING_COMMANDS as u64 + 5) {
            let disposition = p.add_hint(
                PeerHint::ObservedReachable {
                    peer_id: synthetic_peer(100 + n),
                    address: "/ip4/192.0.2.1/tcp/4001".to_owned(),
                    observed_at: 1_000,
                },
                1_000,
            );
            assert_eq!(disposition, HintDisposition::Accepted);
        }
        let commands = p.drain_commands(usize::MAX);
        assert_eq!(
            commands.len(),
            MAX_PENDING_COMMANDS + 1,
            "the bound holds: every advisory slot plus the control command"
        );
        assert!(
            matches!(commands[0], KademliaCommand::SetMode { .. }),
            "control commands are never displaced"
        );
        match &commands[1] {
            KademliaCommand::OfferRoutingPeer { peer, .. } => {
                assert_eq!(
                    *peer,
                    synthetic_peer(105),
                    "the five oldest advisories were displaced, oldest first"
                );
            }
            other => panic!("expected an offer, got {other:?}"),
        }
    }

    #[test]
    fn health_reflects_population_and_query_success() {
        let mut p = started(KademliaMode::Client);
        let (a, b) = (synthetic_peer(1), synthetic_peer(2));
        trust(&mut p, &[&a, &b]);
        assert_eq!(p.health(), ProviderHealth::Degraded, "warming, not broken");
        p.ingest_driver_event(KademliaEvent::RoutingPeerAdded { peer: a.clone() }, 1_000);
        p.ingest_driver_event(KademliaEvent::RoutingPeerAdded { peer: b.clone() }, 1_000);
        p.ingest_driver_event(
            results(&[(synthetic_peer(3), "/ip4/192.0.2.1/tcp/1")]),
            1_000,
        );
        assert_eq!(
            p.health(),
            ProviderHealth::Healthy,
            "a two-peer trusted overlay is fully healthy at its effective target"
        );
        p.ingest_driver_event(
            KademliaEvent::QueryFailed {
                class: QueryClass::Exploration,
                reason: QueryFailure::BudgetExhausted,
            },
            2_000,
        );
        assert_eq!(
            p.health(),
            ProviderHealth::Healthy,
            "a refused budget is scheduling, not the network failing"
        );
        p.ingest_driver_event(
            KademliaEvent::QueryFailed {
                class: QueryClass::Exploration,
                reason: QueryFailure::TimedOut,
            },
            2_000,
        );
        assert_eq!(p.health(), ProviderHealth::Degraded, "a timeout degrades");
        p.ingest_driver_event(KademliaEvent::RoutingPeerRemoved { peer: b }, 3_000);
        assert_eq!(p.routing_view().routing_peers, 1);
    }

    #[test]
    fn the_view_counts_remote_trusted_excluding_self() {
        let mut p = started(KademliaMode::Client);
        let peer = synthetic_peer(1);
        p.set_remote_trusted([local(), peer].into_iter().collect());
        assert_eq!(
            p.routing_view().remote_trusted_population,
            1,
            "§9.3 counts DISTINCT REMOTE trusted peers"
        );
    }

    #[test]
    fn the_evidence_bound_keeps_the_freshest() {
        let mut p = started(KademliaMode::Client);
        for n in 0..MAX_CAPABILITY_EVIDENCE as u64 {
            give_evidence(&mut p, &synthetic_peer(10_000 + n), 1_000 + n);
        }
        assert_eq!(p.evidence.len(), MAX_CAPABILITY_EVIDENCE);
        // Staler than everything held: dropped, not admitted by eviction.
        give_evidence(&mut p, &synthetic_peer(999), 500);
        assert!(
            !p.evidence.contains_key(&synthetic_peer(999)),
            "churn cannot replace fresh evidence with old"
        );
        // Fresher than the stalest: admitted, evicting the stalest.
        give_evidence(&mut p, &synthetic_peer(998), 900_000);
        assert!(p.evidence.contains_key(&synthetic_peer(998)));
        assert!(
            !p.evidence.contains_key(&synthetic_peer(10_000)),
            "the stalest entry made room"
        );
        assert_eq!(p.evidence.len(), MAX_CAPABILITY_EVIDENCE);
    }

    #[test]
    fn shutdown_clears_and_commands_shutdown() {
        let mut p = started(KademliaMode::Client);
        let peer = synthetic_peer(1);
        p.ingest_driver_event(results(&[(peer.clone(), "/ip4/192.0.2.1/tcp/1")]), 1_000);
        p.shutdown(2_000);
        assert_eq!(p.health(), ProviderHealth::Unavailable);
        assert!(
            p.drain_events(3_000, usize::MAX).is_empty(),
            "the stream ends"
        );
        assert_eq!(
            p.drain_commands(usize::MAX),
            vec![KademliaCommand::Shutdown],
            "pending advice is dropped; the stop itself is delivered"
        );
        p.shutdown(4_000);
        assert!(p.drain_commands(usize::MAX).is_empty(), "idempotent");
    }

    #[test]
    fn an_empty_normalized_candidate_is_not_tracked() {
        let mut p = started(KademliaMode::Client);
        let peer = synthetic_peer(1);
        let liar = synthetic_peer(2);
        let lying = format!("/ip4/192.0.2.1/tcp/4001/p2p/{}", liar.as_str());
        p.ingest_driver_event(results(&[(peer, lying.as_str())]), 1_000);
        assert!(
            observations(&p.drain_events(1_000, usize::MAX)).is_empty(),
            "a candidate whose every address was rejected observes nothing"
        );
    }

    #[test]
    fn a_permit_is_taken_before_the_command_is_queued() {
        let mut p = started(KademliaMode::Client);
        p.drain_commands(usize::MAX);
        let target = synthetic_peer(7);
        trust(&mut p, &[&target]);
        give_evidence(&mut p, &target, 1_000);
        let held_a = p.budgets.acquire(2_000).expect("slot");
        let held_b = p.budgets.acquire(2_000).expect("slot");
        assert_eq!(
            p.request_targeted_lookup(&target, 2_000, false),
            Err(TargetedRefusal::BudgetExhausted)
        );
        assert!(
            p.drain_commands(usize::MAX).is_empty(),
            "a refused request queues NOTHING: the permit comes before the \
             command, or the ceiling is bookkeeping"
        );
        p.budgets.release(held_a);
        p.budgets.release(held_b);
    }

    #[test]
    fn the_implicit_bootstrap_is_charged_and_settles_on_completion() {
        let mut p = KademliaDiscovery::new(
            KademliaProviderConfig {
                max_queries_per_minute: 3,
                ..config(KademliaMode::Client)
            },
            local(),
        )
        .expect("valid config");
        p.start(0).expect("starts");
        let router = synthetic_peer(1);
        let (t1, t2, t3) = (synthetic_peer(2), synthetic_peer(3), synthetic_peer(4));
        trust(&mut p, &[&router, &t1, &t2, &t3]);
        for t in [&t1, &t2, &t3] {
            give_evidence(&mut p, t, 1_000);
        }

        // F2: the first insertion on an empty table charges the implicit
        // bootstrap — one slot held, one rate charge spent.
        p.ingest_driver_event(
            KademliaEvent::RoutingPeerAdded {
                peer: router.clone(),
            },
            1_000,
        );
        assert_eq!(p.budgets.held(), 1, "the uncommanded query holds a slot");
        p.request_targeted_lookup(&t1, 2_000, false)
            .expect("the second slot is free");
        assert_eq!(
            p.request_targeted_lookup(&t2, 2_000, false),
            Err(TargetedRefusal::BudgetExhausted),
            "the implicit bootstrap occupies real concurrency"
        );

        // The driver reports the bootstrap it ran; the slot comes back.
        p.ingest_driver_event(done(QueryClass::Bootstrap), 3_000);
        p.request_targeted_lookup(&t2, 3_000, false)
            .expect("the settled slot is free again");

        // But its RATE charge stays spent: three starts are on the
        // window (implicit + t1 + t2), and the ceiling is three.
        p.ingest_driver_event(done(QueryClass::Targeted), 4_000);
        p.ingest_driver_event(done(QueryClass::Targeted), 4_000);
        assert_eq!(
            p.request_targeted_lookup(&t3, 4_000, false),
            Err(TargetedRefusal::BudgetExhausted),
            "consume keeps the rate charge; the window really was used"
        );
        p.request_targeted_lookup(&t3, 1_000 + 60_000, false)
            .expect("one window after the charge, the rate recovers");
    }

    #[test]
    fn saturation_requires_no_targetable_evidence_outside_the_routing_set() {
        let mut p = started(KademliaMode::Client);
        let router = synthetic_peer(1);
        let (b, c) = (synthetic_peer(2), synthetic_peer(3));
        trust(&mut p, &[&router, &b, &c]);
        routed(&mut p, &router, 1_000);
        // Fresh positive evidence for a trusted peer NOT in the routing
        // set: exactly what §9.3 says forbids resting.
        give_evidence(&mut p, &c, 1_000);

        // Three successful no-progress exploration rounds.
        let mut now = 10_000;
        for round in 0..3 {
            assert!(p.tick(now, [0_u8; 32]), "round {round} issues");
            p.ingest_driver_event(done(QueryClass::Exploration), now + 1);
            now += 500_000;
        }
        assert_eq!(p.no_progress_rounds, 3);
        assert!(p.routing_view().is_saturated());
        assert_eq!(
            p.health(),
            ProviderHealth::Degraded,
            "a targetable peer waits outside the routing set, so the \
             saturated-looking view may not rest"
        );

        // Once that evidence lapses, the same rounds ARE a resting state.
        p.drain_events(1_000 + TTL_MS + 1, usize::MAX);
        assert_eq!(
            p.health(),
            ProviderHealth::Healthy,
            "with the conjunct released, saturation stands and health rests"
        );
        assert!(
            !p.tick(1_000 + TTL_MS + 2, [0_u8; 32]),
            "a validly saturated view does not explore"
        );
    }

    #[test]
    fn a_small_overlay_backs_off_to_the_cap() {
        let mut p = KademliaDiscovery::new(
            KademliaProviderConfig {
                // Evidence that stays fresh for the whole climb, so the
                // outside-evidence conjunct keeps saturation invalid and
                // rounds keep accruing past three.
                candidate_ttl_ms: 100_000_000,
                ..config(KademliaMode::Client)
            },
            local(),
        )
        .expect("valid config");
        p.start(0).expect("starts");
        let router = synthetic_peer(1);
        let c = synthetic_peer(3);
        trust(&mut p, &[&router, &synthetic_peer(2), &c]);
        routed(&mut p, &router, 1_000);
        give_evidence(&mut p, &c, 1_000);

        let mut now = 10_000;
        for _ in 0..6 {
            assert!(p.tick(now, [0_u8; 32]));
            p.ingest_driver_event(done(QueryClass::Exploration), now);
            let due = p.pacing.next_exploration_due_ms().expect("scheduled");
            assert!(!p.tick(due - 1, [0_u8; 32]), "not before it is due");
            now = due;
        }
        assert_eq!(
            p.pacing.next_exploration_due_ms(),
            Some(now),
            "the loop advanced exactly to each due time"
        );
        // Six no-progress rounds: 60s doubled past 960s rests at the cap.
        p.ingest_driver_event(done(QueryClass::Exploration), now);
        let before = now;
        assert!(p.tick(now, [0_u8; 32]));
        p.ingest_driver_event(done(QueryClass::Exploration), now);
        assert_eq!(
            p.pacing.next_exploration_due_ms(),
            Some(before + interweave_kademlia_control_api::MAX_EXPLORATION_INTERVAL_MS),
            "a two-peer overlay does not run a 60-second loop forever"
        );
    }

    #[test]
    fn exploration_rests_at_the_effective_target() {
        let mut p = started(KademliaMode::Client);
        let (a, b) = (synthetic_peer(1), synthetic_peer(2));
        trust(&mut p, &[&a, &b]);
        routed(&mut p, &a, 1_000);
        p.ingest_driver_event(KademliaEvent::RoutingPeerAdded { peer: b.clone() }, 1_000);
        p.drain_commands(usize::MAX);
        assert!(
            !p.tick(10_000, [0_u8; 32]),
            "two routed of two trusted is the effective target; resting"
        );
        assert!(p.drain_commands(usize::MAX).is_empty());
    }

    #[test]
    fn bootstrap_is_spaced_and_keyed_to_self() {
        let mut p = started(KademliaMode::Client);
        p.drain_commands(usize::MAX);
        p.request_bootstrap(1_000).expect("the first is free");
        let want_key = normalize::targeted_lookup_key(&local()).expect("ed25519 local");
        assert_eq!(
            p.drain_commands(usize::MAX),
            vec![KademliaCommand::StartQuery {
                class: QueryClass::Bootstrap,
                key: want_key,
            }],
            "bootstrap is a self lookup"
        );
        assert_eq!(
            p.request_bootstrap(1_001),
            Err(BootstrapRefusal::TooSoon),
            "bootstrap_min_interval is a floor, not a suggestion"
        );
        p.request_bootstrap(1_000 + 300_000)
            .expect("the floor elapsed");
        assert!(!p.bootstrap_refresh_due(1_000 + 300_000 + 899_999));
        assert!(p.bootstrap_refresh_due(1_000 + 300_000 + 900_000));
    }

    #[test]
    fn a_trust_revision_invalidates_saturation() {
        let mut p = started(KademliaMode::Client);
        let router = synthetic_peer(1);
        let others: BTreeSet<TransportIdentity> =
            [router.clone(), synthetic_peer(2), synthetic_peer(3)]
                .into_iter()
                .collect();
        p.set_remote_trusted(others.clone());
        routed(&mut p, &router, 1_000);
        let mut now = 10_000;
        for _ in 0..3 {
            assert!(p.tick(now, [0_u8; 32]));
            p.ingest_driver_event(done(QueryClass::Exploration), now);
            now += 500_000;
        }
        assert_eq!(p.health(), ProviderHealth::Healthy, "validly saturated");
        p.set_remote_trusted(others);
        assert_eq!(
            p.health(),
            ProviderHealth::Degraded,
            "a trust revision means the set explored is not the set any \
             more; saturation is invalidated (§9.3)"
        );
    }

    #[test]
    fn evidence_is_judged_at_delivery_time_not_at_its_claimed_instant() {
        let mut p = started(KademliaMode::Client);
        let target = synthetic_peer(7);
        trust(&mut p, &[&target]);
        // A future-dated POSITIVE arrives at t=1_000 claiming t=999_999.
        let disposition = p.add_hint(
            PeerHint::ObservedProtocol {
                peer_id: target.clone(),
                protocol_id: expected_id(&p),
                supported: true,
                observed_at: 999_999_999,
            },
            1_000,
        );
        assert_eq!(disposition, HintDisposition::Accepted);
        // A real, honestly-dated negative afterwards must supersede it:
        // unclamped, the forged timestamp out-recencies reality.
        let real = p.add_hint(
            PeerHint::ObservedProtocol {
                peer_id: target.clone(),
                protocol_id: expected_id(&p),
                supported: false,
                observed_at: 2_000,
            },
            2_000,
        );
        assert_eq!(real, HintDisposition::Accepted);
        assert_eq!(
            p.request_targeted_lookup(&target, 3_000, false),
            Err(TargetedRefusal::NegativeServerEvidence),
            "a forged future timestamp does not outvote a real observation"
        );
    }

    #[test]
    fn a_carried_observation_never_outlives_its_own_age() {
        let mut p = started(KademliaMode::Client);
        let target = synthetic_peer(7);
        trust(&mut p, &[&target]);
        let candidate = CandidatePeer {
            peer_id: target.clone(),
            addresses: BTreeSet::new(),
            source: "peer-cache".to_owned(),
            observed_at: 1_000,
            // A record vouched-for far beyond the observation's age.
            expires_at: Some(1_000 + 10 * TTL_MS),
            protocol_observations: [interweave_discovery_api::ProtocolObservation {
                protocol_id: expected_id(&p),
                supported: true,
                observed_at: 1_000,
            }]
            .into_iter()
            .collect(),
        };
        assert_eq!(
            p.add_hint(PeerHint::CandidateHint(Box::new(candidate)), 1_000),
            HintDisposition::Accepted
        );
        p.request_targeted_lookup(&target, 1_000 + TTL_MS - 1, false)
            .expect("inside its own age the evidence is eligible");
        assert_eq!(
            p.request_targeted_lookup(&target, 1_000 + TTL_MS + 1, false),
            Err(TargetedRefusal::StaleServerEvidence),
            "the record's long life does not keep an old observation eligible"
        );
    }

    #[test]
    fn a_new_address_for_a_routed_peer_is_exploration_progress() {
        let mut p = started(KademliaMode::Client);
        let router = synthetic_peer(1);
        trust(&mut p, &[&router, &synthetic_peer(2), &synthetic_peer(3)]);
        routed(&mut p, &router, 1_000);

        assert!(p.tick(10_000, [0_u8; 32]));
        p.ingest_driver_event(results(&[(router.clone(), "/ip4/192.0.2.1/tcp/1")]), 10_000);
        assert_eq!(
            p.no_progress_rounds, 0,
            "a first address for a routed peer is progress"
        );

        assert!(p.tick(200_000, [0_u8; 32]));
        p.ingest_driver_event(
            results(&[(router.clone(), "/ip4/192.0.2.1/tcp/1")]),
            200_000,
        );
        assert_eq!(p.no_progress_rounds, 1, "the same address again is not");

        assert!(p.tick(400_000, [0_u8; 32]));
        p.ingest_driver_event(
            results(&[(synthetic_peer(9), "/ip4/192.0.2.5/tcp/5")]),
            400_000,
        );
        assert_eq!(
            p.no_progress_rounds, 2,
            "a stranger's address is a candidate, not routed-peer progress"
        );

        assert!(p.tick(700_000, [0_u8; 32]));
        p.ingest_driver_event(
            results(&[(router.clone(), "/ip4/192.0.2.9/tcp/9")]),
            700_000,
        );
        assert_eq!(
            p.no_progress_rounds, 0,
            "a NEW usable address for a routed peer is §9.3 progress"
        );
    }

    #[test]
    fn untargetable_evidence_does_not_block_saturation() {
        let mut p = started(KademliaMode::Client);
        let router = synthetic_peer(1);
        let mut bytes = [0_u8; 34];
        bytes[..2].copy_from_slice(&[0x12, 0x20]);
        bytes[2] = 9;
        let qm = TransportIdentity::parse(bs58::encode(bytes).into_string()).expect("valid Qm id");
        p.set_remote_trusted(
            [router.clone(), qm.clone(), synthetic_peer(2)]
                .into_iter()
                .collect(),
        );
        routed(&mut p, &router, 1_000);
        // Fresh positive evidence for a trusted, non-routed peer whose
        // identity no lookup key can ever be recovered from.
        give_evidence(&mut p, &qm, 1_000);
        let mut now = 10_000;
        for _ in 0..3 {
            assert!(p.tick(now, [0_u8; 32]));
            p.ingest_driver_event(done(QueryClass::Exploration), now);
            now += 500_000;
        }
        assert_eq!(
            p.health(),
            ProviderHealth::Healthy,
            "evidence no lookup can ever act on does not forbid resting: \
             request_targeted_lookup would refuse this peer forever"
        );
    }

    #[test]
    fn a_cooling_down_target_does_not_block_saturation() {
        let mut p = started(KademliaMode::Client);
        let router = synthetic_peer(1);
        let c = synthetic_peer(3);
        trust(&mut p, &[&router, &synthetic_peer(2), &c]);
        routed(&mut p, &router, 1_000);
        give_evidence(&mut p, &c, 1_000);
        let mut now = 10_000;
        for _ in 0..3 {
            assert!(p.tick(now, [0_u8; 32]));
            p.ingest_driver_event(done(QueryClass::Exploration), now);
            now += 500_000;
        }
        assert_eq!(
            p.health(),
            ProviderHealth::Degraded,
            "the control: an immediately targetable peer forbids resting"
        );
        p.request_targeted_lookup(&c, now, false)
            .expect("eligible, and the lookup records its cooldown");
        assert_eq!(
            p.health(),
            ProviderHealth::Healthy,
            "inside its cooldown the target is not IMMEDIATELY targetable, \
             so the saturated view may rest (§9.3)"
        );
    }

    #[test]
    fn a_trust_revision_drops_revoked_routers_from_the_view_at_once() {
        let mut p = started(KademliaMode::Client);
        let (a, b) = (synthetic_peer(1), synthetic_peer(2));
        trust(&mut p, &[&a]);
        routed(&mut p, &a, 1_000);
        assert_eq!(p.routing_view().routing_peers, 1);

        // A is replaced by B: the driver's RoutingPeerRemoved has not
        // arrived yet, and must not be needed for the VIEW to be honest.
        trust(&mut p, &[&b]);
        assert_eq!(
            p.routing_view().routing_peers,
            0,
            "a revoked router does not satisfy the new effective target"
        );
        // The driver's own removal, arriving later, finds nothing left
        // to do.
        p.ingest_driver_event(KademliaEvent::RoutingPeerRemoved { peer: a }, 2_000);
        assert_eq!(p.routing_view().routing_peers, 0);
    }
}
