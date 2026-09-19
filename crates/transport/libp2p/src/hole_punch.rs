// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `DCUTR.md` §§2-4, 7 and `transport/libp2p/CONNECTIVITY.md` §13 at
//! the behaviour boundary: the hole-punch ATTEMPT, which the pinned
//! crate has no notion of.
//!
//! # One attempt is not one dial
//!
//! `libp2p-dcutr` 0.14.1 takes a `PeerId` and nothing else: no
//! concurrency cap, no per-peer cap, no cooldown. Its
//! `MAX_NUMBER_OF_UPGRADE_ATTEMPTS = 3` is a retry count per relayed
//! connection on the initiating side, and SPIKE-004 measured that one
//! punch produces a dial at BOTH ends, so no single gate ever sees the
//! attempt -- only its own half -- and the outcome reaches the
//! behaviour rather than the gate (SPIKES.md, "DCUtR's bounds have no
//! knob"). So the attempt lifecycle lives here, around the crate:
//!
//! - an attempt BEGINS when a relayed connection to a data-plane peer
//!   is established -- inbound, where the crate's handler initiates the
//!   CONNECT at once (`behaviour.rs:186`), or outbound, where it waits
//!   for the remote's -- and §13's eligibility is decided at that
//!   moment: no direct connection to the peer, the peer not in
//!   cooldown, fewer than `max_inflight_per_peer` attempts toward it
//!   and fewer than `max_inflight` in all. A relayed connection that
//!   fails the test gets a handler that speaks no DCUtR at all
//!   (`dummy`), so the crate never learns of it and the remote's
//!   CONNECT finds no protocol -- the same shape `ClassGated` uses to
//!   withhold a service;
//! - it ENDS on the crate's `Event`: `Ok` clears the peer's cooldown,
//!   `Err` starts it (§7: timeout, unsupported, attempts exceeded, a
//!   punch dial the root gate refused -- the crate retries that one to
//!   its ceiling and then reports it); or on the relayed connection
//!   closing (§7: "relay disappears during punch", no cooldown); or at
//!   `ATTEMPT_HORIZON_MS`, because the crate reports NO outcome to the
//!   responding side of a failed punch (`on_dial_failure` tracks only
//!   the initiator's attempts) and a punch nobody reports on would hold
//!   its permit forever.
//!
//! # The address-class boundary (`DCUTR.md` §6, ADR-0052)
//!
//! A punch candidate is a peer-supplied address this profile would
//! TCP-connect to, and trust in the far end is trust with the data
//! plane, not with where this host opens sockets. So the boundary
//! `is_punchable_address` draws -- a literal IP, no circuit, none of
//! the special-use ranges, a private range only beside a private
//! listener of the same family -- is applied three times here: to the
//! candidates the Swarm tells the crate about (a peer's Identify
//! observed us on loopback, say), so what this profile SENDS in a
//! CONNECT is inside it; to the listeners the runtime offers, for the
//! same reason; and to every punch dial the crate issues, FILTERED
//! (ADR-0052 rule 5): the far end loses only the refused address, never
//! the punch. The crate's `Dial` carries its address list in a field the
//! Swarm crate keeps to itself, so the first place the list is visible
//! is the pending hook -- and a hook can add addresses, never remove one
//! -- so the filter runs THERE by denying the crate's dial and issuing
//! the survivors as a dial of the wrapper's own, with every option of
//! the crate's kept (the peer, `PeerCondition::Always`, and the role
//! override on the initiating end). The wrapper needs none of the
//! crate's id-keyed bookkeeping for the outcome: it ends an attempt on
//! ANY direct connection to the peer (`punched`), so the crate's not
//! knowing the replacement's id changes nothing. A list with no
//! survivor ends the attempt `refused_by_class`; removed candidates
//! are counted by class apart from that; and the wrapper's own dial is
//! judged at the same hook as the BACKSTOP -- a refused address there
//! is a defect in the filter, counted under its own label. Every
//! refusal names the class and never the address. Pinned by
//! `a_punch_dial_carrying_a_refused_candidate_is_reissued_without_it`,
//! `a_punch_dial_with_no_admitted_candidate_ends_the_attempt` and
//! the composed case in `tests/connectivity/tests/dcutr.rs`.
//!
//! What this wrapper does NOT decide: which peers may punch at all --
//! `ClassGated` outside it hands a non-data-plane peer no DCUtR handler,
//! which is §2's "never toward an infrastructure-only destination" and
//! D1's fix at the gate beside it; and which punch dials are admitted
//! -- `Attributing` outside it announces every one as `DcutrHolePunch`
//! and the root policy judges the destination (SPIKE-004 R12.4). Here
//! a success is a success the moment a direct connection lands; whether
//! it becomes the peer's PATH is the runtime's, once it has held for
//! the stability interval (§13's ten seconds, step 9) -- this wrapper
//! keeps the other half of §4: a punched connection that closes sooner
//! is a stability failure and cools the peer down, one that holds is
//! an upgrade counted.
//!
//! Every claim above with a `never` or `only` is pinned in this file's
//! tests, and the dials and outcomes on real sockets by
//! `tests/connectivity/tests/dcutr.rs`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

use either::Either;
use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::dcutr;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm, dummy,
};
use libp2p::{Multiaddr, PeerId};

use interweave_transport_runtime::reachability::{CandidateRefusal, is_punchable_address};

/// How long an attempt may stay in flight before it is counted failed
/// and its permit returned: the crate's handler bounds each stream at
/// ten seconds and the initiator retries up to three times, each retry
/// a fresh dial under the Swarm's own connect timeout, and the
/// responding side of a failed punch is told nothing at all. Ninety
/// seconds is past every one of those; `an_attempt_past_the_horizon_
/// is_counted_failed_and_releases_its_permit` pins the release.
pub const ATTEMPT_HORIZON_MS: u64 = 90_000;

/// Peers in cooldown are pruned as they expire; this bounds the map
/// between prunes against a flood of peers that each fail once.
pub const MAX_COOLDOWN_PEERS: usize = 1024;

/// §13's bounds, as the wrapper enforces them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HolePunchBudgets {
    /// Attempts in flight across all peers.
    pub max_inflight: usize,
    /// Attempts in flight toward one peer.
    pub max_inflight_per_peer: usize,
    /// How long a peer waits after a failed attempt.
    pub cooldown_ms: u64,
    /// How long a punched direct connection must hold before the
    /// upgrade counts (`DCUTR.md` §4): one that closes sooner is a
    /// stability failure, and the peer cools down as for any failure.
    pub stability_ms: u64,
}

impl Default for HolePunchBudgets {
    /// §13's defaults: 4, 1, five minutes and ten seconds.
    fn default() -> Self {
        Self {
            max_inflight: 4,
            max_inflight_per_peer: 1,
            cooldown_ms: 5 * 60_000,
            stability_ms: 10_000,
        }
    }
}

/// Why a relayed connection was not given a DCUtR handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Decline {
    /// A direct connection to the peer already exists (§2: "no stable
    /// preferred direct path already exists").
    DirectExists,
    /// The peer failed within the cooldown.
    Cooldown,
    /// The per-peer ceiling is reached.
    PeerBusy,
    /// The global ceiling is reached.
    Busy,
}

impl Decline {
    /// §8's `outcome` label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DirectExists => "declined_direct_exists",
            Self::Cooldown => "declined_cooldown",
            Self::PeerBusy => "declined_peer_busy",
            Self::Busy => "declined_busy",
        }
    }
}

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// The crate established a direct connection.
    Succeeded,
    /// The crate gave up, with its reason.
    Failed(String),
    /// Nothing was reported within [`ATTEMPT_HORIZON_MS`].
    TimedOut,
    /// The relayed connection closed while the attempt was in flight.
    Abandoned,
    /// A punch dial carried a candidate outside the address-class
    /// boundary (`DCUTR.md` §6) and was refused before any socket.
    RefusedByClass(CandidateRefusal),
}

impl Ending {
    /// §8's `outcome` label.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed(_) => "failed",
            Self::TimedOut => "timed_out",
            Self::Abandoned => "abandoned",
            Self::RefusedByClass(_) => "refused_by_class",
        }
    }
}

/// The refusal a punch dial gets at the pending hook, as the Swarm
/// reports it to the crate.
#[derive(Debug)]
pub struct RefusedCandidate(pub CandidateRefusal);

impl std::fmt::Display for RefusedCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "hole-punch candidate refused by address class: {}",
            self.0.label()
        )
    }
}

impl std::error::Error for RefusedCandidate {}

/// The denial a crate punch dial gets when the wrapper reissues it
/// without its refused candidates: not a refusal of the punch, and
/// the outbound gate reads the type to take its ticket back without
/// writing a refusal down.
#[derive(Debug)]
pub struct DialReissued;

impl std::fmt::Display for DialReissued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("hole-punch dial reissued without its refused candidates")
    }
}

impl std::error::Error for DialReissued {}

/// What the wrapper reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolePunchEvent {
    /// A relayed connection was not given a DCUtR handler.
    Declined {
        /// The peer at the far end of the circuit.
        peer: PeerId,
        /// Which of section 13's bounds declined it.
        reason: Decline,
    },
    /// An attempt began on a relayed connection.
    Started {
        /// The peer.
        peer: PeerId,
    },
    /// An attempt ended.
    Ended {
        /// The peer.
        peer: PeerId,
        /// How.
        ending: Ending,
    },
    /// A punched direct connection closed before the stability
    /// interval elapsed (`DCUTR.md` §4): the peer is in cooldown.
    Unstable {
        /// The peer.
        peer: PeerId,
    },
}

/// §8's counters, readable outside the Swarm task through
/// [`HolePunchCounterHandle`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HolePunchCounters {
    /// `dcutr_attempts_total{outcome}`, by [`Ending::label`].
    pub attempts_ended: std::collections::BTreeMap<&'static str, u64>,
    /// Relayed connections not given a handler, by [`Decline::label`].
    pub declined: std::collections::BTreeMap<&'static str, u64>,
    /// `dcutr_inflight`.
    pub inflight: usize,
    /// `dcutr_cooldown_peers`.
    pub cooldown_peers: usize,
    /// Candidates a peer's Identify observed this profile on that the
    /// boundary kept from the crate, by [`CandidateRefusal::label`].
    pub candidates_withheld: std::collections::BTreeMap<&'static str, u64>,
    /// Listeners this profile bound that are offered to the crate as
    /// candidates right now -- the `offered` set's size, which follows
    /// the bound listeners.
    pub listeners_offered: usize,
    /// Candidates the far end named that were removed from a punch dial
    /// by the boundary, by [`CandidateRefusal::label`] -- apart from
    /// `refused_by_class`, which is the attempt-level outcome when
    /// nothing survives.
    pub candidates_removed: std::collections::BTreeMap<&'static str, u64>,
    /// `direct_upgrade_success_total`: punched direct connections that
    /// held for the stability interval.
    pub upgrades_stable: u64,
    /// `direct_upgrade_stability_failures_total`: punched direct
    /// connections that closed before it.
    pub stability_failures: u64,
    /// The backstop fired: a wrapper-issued dial reached the hook with
    /// a refused address in it -- the filter missed one, or the
    /// boundary moved between the two hooks (a private listener
    /// forgotten in between).
    pub backstop_refusals: u64,
}

/// A handle on the counters that outlives the move into the Swarm --
/// the shape `ProbeCounterHandle` has, for the same reason.
#[derive(Debug, Clone, Default)]
pub struct HolePunchCounterHandle {
    inner: Arc<Mutex<HolePunchCounters>>,
}

impl HolePunchCounterHandle {
    /// The counters as they stand.
    #[must_use]
    pub fn snapshot(&self) -> HolePunchCounters {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, HolePunchCounters> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One attempt in flight, keyed by its relayed connection.
#[derive(Debug, Clone, Copy)]
struct Attempt {
    peer: PeerId,
    started_ms: u64,
    /// Given up by a network change: the crate's rounds on the relayed
    /// connection run on regardless -- the wrapper cannot stop them --
    /// so the attempt keeps its per-peer permit until the crate's
    /// outcome, the relayed close or the horizon ends it, and whatever
    /// that is, it ends `Abandoned` with no cooldown.
    abandoned: bool,
}

/// The pinned DCUtR behaviour under §13's attempt lifecycle.
pub struct HolePunchScope {
    inner: dcutr::Behaviour,
    budgets: HolePunchBudgets,
    now_ms: u64,
    /// Direct connections per peer, both directions, as this wrapper
    /// handed them to the crate: the crate tracks the same set
    /// privately and `expect`s every close to match, so the wrapper
    /// forwards a direct connection's close only when it forwarded its
    /// establishment. Bounded by the Swarm's connections.
    direct: HashMap<PeerId, HashSet<ConnectionId>>,
    /// Attempts in flight, by relayed connection. Bounded by
    /// `budgets.max_inflight`.
    attempts: HashMap<ConnectionId, Attempt>,
    /// Peers in cooldown and when it ends. Pruned on tick; bounded by
    /// [`MAX_COOLDOWN_PEERS`] between prunes.
    cooldown: HashMap<PeerId, u64>,
    events: VecDeque<HolePunchEvent>,
    counters: HolePunchCounterHandle,
    /// Listeners this profile bound and offered to the crate as
    /// candidates, each once while it is bound: an address leaves when
    /// its listener does (`forget_listener`, at the runtime's expired-
    /// address and listener-closed events), so the set holds at most
    /// the addresses currently bound -- the runtime's active-listener
    /// ceiling times the addresses a listener reports. The crate's own
    /// candidate cache keeps what it was told (an LRU of twenty), so a
    /// stale listener may still be sent in a CONNECT until it ages out.
    /// `a_bound_listener_is_offered_once_and_forgotten_with_its_listener`
    /// pins the set.
    offered: HashSet<Multiaddr>,
    /// Direct connections whose establishment ended an attempt -- the
    /// punched ones -- until the runtime reads them (`take_punched`),
    /// which it does for every connection it is told of. Bounded by
    /// attempts.
    punched: HashSet<ConnectionId>,
    /// Punched direct connections in their stability interval, with
    /// the peer and when they were established: closed sooner, a
    /// stability failure; older, an upgrade that held. Bounded by
    /// attempts, pruned on the tick.
    stabilising: HashMap<ConnectionId, (PeerId, u64)>,
    /// The crate's own dials, by the connection id it minted, from the
    /// `ToSwarm::Dial` it emitted until the pending hook sees them:
    /// the hook is asked about every dial in the Swarm and judges only
    /// these. Bounded by attempts times the crate's retry ceiling; an
    /// entry the Swarm refuses before the hook is forgotten on its
    /// `DialFailure` (`a_punch_dial_the_swarm_refused_before_the_hook_
    /// is_forgotten` pins it).
    punch_dials: HashMap<ConnectionId, PeerId>,
    /// The crate's dials this wrapper denied and reissued without the
    /// refused candidates: their `DialFailure` is the denial's own and
    /// is not the crate's to retry. Forgotten on that failure.
    replaced: HashSet<ConnectionId>,
    /// The wrapper's own reissued dials, by the id it minted, until the
    /// hook judges them (the backstop). Bounded by attempts.
    replacement_dials: HashMap<ConnectionId, PeerId>,
    /// The reissued dials the hook admitted, on the wire: their
    /// failure ends the attempt (the crate knows nothing of them, so
    /// nothing else would), their establishment is the punch. Removed
    /// on either, and with the attempt whichever way it ends -- the
    /// class gate can withhold an establishment from this wrapper (a
    /// trust revoked between the two hooks), so the attempt's end is
    /// what bounds the map to live attempts.
    /// `a_reissued_dial_leaves_the_map_with_its_attempt` pins it.
    reissued_in_flight: HashMap<ConnectionId, PeerId>,
    /// Dials to hand the Swarm before the crate's next action.
    actions: VecDeque<libp2p::swarm::dial_opts::DialOpts>,
}

impl HolePunchScope {
    /// Wrap `inner` under `budgets`.
    #[must_use]
    pub fn new(inner: dcutr::Behaviour, budgets: HolePunchBudgets) -> Self {
        Self {
            inner,
            budgets,
            now_ms: 0,
            direct: HashMap::new(),
            attempts: HashMap::new(),
            cooldown: HashMap::new(),
            events: VecDeque::new(),
            counters: HolePunchCounterHandle::default(),
            offered: HashSet::new(),
            punched: HashSet::new(),
            stabilising: HashMap::new(),
            punch_dials: HashMap::new(),
            replaced: HashSet::new(),
            replacement_dials: HashMap::new(),
            reissued_in_flight: HashMap::new(),
            actions: VecDeque::new(),
        }
    }

    /// Whether `address` is inside the boundary, given the listeners
    /// this profile bound.
    fn within_boundary(&self, address: &Multiaddr) -> Result<(), CandidateRefusal> {
        let text = address.to_string();
        let own: Vec<String> = self.offered.iter().map(ToString::to_string).collect();
        is_punchable_address(&text, own.iter().map(String::as_str))
    }

    /// Offer an address THIS PROFILE BOUND to the crate as a candidate
    /// it sends in its CONNECT, through the same door the Swarm's
    /// observed candidates use. The crate learns an address only from
    /// `NewExternalAddrCandidate`, i.e. from what a peer's Identify
    /// observed -- and a relay reached BEFORE this profile listened
    /// observed an ephemeral port, since the TCP transport reuses the
    /// listen port only once there is one, so the punch would name a
    /// port nobody listens on. A listener is what a peer on the same
    /// network can reach directly; behind a NAT it is one candidate
    /// among the observed ones. Each is offered once; returns whether
    /// it was.
    pub fn offer_listener(&mut self, address: &Multiaddr) -> bool {
        // INSIDE THE BOUNDARY, judged with itself among the listeners:
        // a private listener is what makes a private candidate
        // legitimate, and a loopback one is never offered -- the far
        // end would refuse it, and this profile refuses the far end's.
        let text = address.to_string();
        let own: Vec<String> = self
            .offered
            .iter()
            .map(ToString::to_string)
            .chain(std::iter::once(text.clone()))
            .collect();
        if is_punchable_address(&text, own.iter().map(String::as_str)).is_err()
            || !self.offered.insert(address.clone())
        {
            return false;
        }
        self.inner
            .on_swarm_event(FromSwarm::NewExternalAddrCandidate(
                libp2p::swarm::behaviour::NewExternalAddrCandidate { addr: address },
            ));
        self.publish();
        true
    }

    /// A listener's address went away: it may be offered again if it
    /// is bound again. Returns whether it was held.
    pub fn forget_listener(&mut self, address: &Multiaddr) -> bool {
        let held = self.offered.remove(address);
        self.publish();
        held
    }

    /// A handle on the counters.
    #[must_use]
    pub fn counter_handle(&self) -> HolePunchCounterHandle {
        self.counters.clone()
    }

    /// Whether an attempt toward `peer` is in flight.
    #[must_use]
    pub fn is_punching(&self, peer: &PeerId) -> bool {
        self.attempts.values().any(|a| a.peer == *peer)
    }

    /// Whether `connection`'s establishment ended an attempt -- what
    /// the runtime reads, once, when it is told of the connection, to
    /// name the path change a punch. The wrapper learns of the
    /// connection first (the Swarm consults the behaviours before it
    /// reports), so "in flight" is already false by then. `now_ms` is
    /// the runtime's clock at the establishment, and it becomes the
    /// interval's start here too: the wrapper's own clock is the last
    /// tick, up to one tick behind, and measured from it the wrapper
    /// would count an upgrade held and stop watching it a tick before
    /// the runtime announces the path -- a close in that window
    /// neither a stability failure nor a cooldown (PR #103 round 1).
    /// Pinned by `a_punched_connection_that_closes_within_the_interval_cools_the_peer_down`.
    pub fn take_punched(&mut self, connection: ConnectionId, now_ms: u64) -> bool {
        if let Some((_, since)) = self.stabilising.get_mut(&connection) {
            *since = now_ms;
        }
        self.punched.remove(&connection)
    }

    /// The network changed (`transport/libp2p/CONNECTIVITY.md` §14 and
    /// `contracts/CONNECTIVITY.md`: DCUtR attempts are runtime state,
    /// rebuilt after a network change). Every attempt in flight is
    /// GIVEN UP -- its CONNECT carried addresses of a network this
    /// profile has left, and a failure it would now meet is not the far
    /// end's -- but not removed: the crate's rounds on the relayed
    /// connection run on, since the runtime closes nothing and the
    /// wrapper cannot stop a handler, so the attempt keeps its per-peer
    /// permit (a second circuit from the peer is `PeerBusy`, as
    /// before) and ends `Abandoned`, with no cooldown, when the crate's
    /// outcome, the relayed close or the horizon reaches it -- removed
    /// at once, the crate's late outcome would have been charged to the
    /// peer's NEXT attempt and re-earned the cooldown (PR #104 round
    /// 1). Every cooldown is lifted, so a peer that could not be
    /// punched from the old network is tried from the new one on its
    /// next circuit; every punched connection still in its interval
    /// stops being judged, since its close, if it comes, is the
    /// interface's and not the path's. The listener set starts over
    /// (the runtime re-offers what is bound). Pinned by
    /// `a_network_change_abandons_attempts_lifts_cooldowns_and_stops_judging`.
    pub fn network_changed(&mut self) {
        for attempt in self.attempts.values_mut() {
            attempt.abandoned = true;
        }
        self.cooldown.clear();
        self.stabilising.clear();
        self.offered.clear();
        self.publish();
    }

    /// Advance the clock: time out attempts past the horizon and prune
    /// cooldowns that have elapsed.
    pub fn tick(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        let expired: Vec<ConnectionId> = self
            .attempts
            .iter()
            .filter(|(_, a)| now_ms.saturating_sub(a.started_ms) >= ATTEMPT_HORIZON_MS)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.end(id, Ending::TimedOut);
        }
        self.cooldown.retain(|_, until| *until > now_ms);
        let held: Vec<ConnectionId> = self
            .stabilising
            .iter()
            .filter(|(_, (_, since))| now_ms.saturating_sub(*since) >= self.budgets.stability_ms)
            .map(|(id, _)| *id)
            .collect();
        for id in held {
            self.stabilising.remove(&id);
            self.counters.lock().upgrades_stable += 1;
        }
        self.publish();
    }

    fn inflight_toward(&self, peer: &PeerId) -> usize {
        self.attempts.values().filter(|a| a.peer == *peer).count()
    }

    /// §13's eligibility for a relayed connection to `peer`, decided
    /// once at its establishment.
    fn admit(&mut self, id: ConnectionId, peer: PeerId) -> Result<(), Decline> {
        let decline = if self.direct.get(&peer).is_some_and(|set| !set.is_empty()) {
            Some(Decline::DirectExists)
        } else if self
            .cooldown
            .get(&peer)
            .is_some_and(|until| *until > self.now_ms)
        {
            Some(Decline::Cooldown)
        } else if self.inflight_toward(&peer) >= self.budgets.max_inflight_per_peer {
            Some(Decline::PeerBusy)
        } else if self.attempts.len() >= self.budgets.max_inflight {
            Some(Decline::Busy)
        } else {
            None
        };
        if let Some(reason) = decline {
            *self
                .counters
                .lock()
                .declined
                .entry(reason.label())
                .or_default() += 1;
            self.events
                .push_back(HolePunchEvent::Declined { peer, reason });
            return Err(reason);
        }
        self.attempts.insert(
            id,
            Attempt {
                peer,
                started_ms: self.now_ms,
                abandoned: false,
            },
        );
        self.events.push_back(HolePunchEvent::Started { peer });
        self.publish();
        Ok(())
    }

    /// End the attempt on `relayed`, if one is in flight. One given up
    /// by a network change ends `Abandoned` whatever reached it.
    fn end(&mut self, relayed: ConnectionId, ending: Ending) {
        let Some(attempt) = self.attempts.remove(&relayed) else {
            return;
        };
        let ending = if attempt.abandoned {
            Ending::Abandoned
        } else {
            ending
        };
        self.reissued_in_flight.retain(|_, p| *p != attempt.peer);
        match ending {
            Ending::Succeeded => {
                self.cooldown.remove(&attempt.peer);
            }
            Ending::Failed(_) | Ending::TimedOut | Ending::RefusedByClass(_) => {
                self.cool_down(attempt.peer);
            }
            Ending::Abandoned => {}
        }
        *self
            .counters
            .lock()
            .attempts_ended
            .entry(ending.label())
            .or_default() += 1;
        self.events.push_back(HolePunchEvent::Ended {
            peer: attempt.peer,
            ending,
        });
        self.publish();
    }

    /// A direct connection to `peer` came up while an attempt toward it
    /// was in flight: that IS the punch, whichever end's dial landed.
    /// The crate reports a success only for its OWN dial (`behaviour.rs`,
    /// `handle_established_outbound_connection`), so the initiating end
    /// of a punch the responder's dial completed would otherwise keep
    /// retrying its own stalled dial to the crate's ceiling and report
    /// the attempt FAILED beside a working direct path -- measured on
    /// loopback, where the initiator's role-overridden connect lands on
    /// a listener and stalls. `a_direct_connection_during_an_attempt_
    /// is_the_success_whichever_end_dialled_it` pins it.
    fn punched(&mut self, peer: PeerId, direct: ConnectionId) {
        if let Some(relayed) = self.attempt_toward(&peer) {
            self.end(relayed, Ending::Succeeded);
            self.punched.insert(direct);
            self.stabilising.insert(direct, (peer, self.now_ms));
        }
    }

    /// A punched direct connection closed: within the stability
    /// interval it is a stability failure, and the peer cools down as
    /// for any failure (`DCUTR.md` §4); past it, the upgrade held and
    /// was counted on the tick. WHY it closed is not read: a close the
    /// runtime itself made -- the connection refused at establishment
    /// for a ceiling, or a revocation that landed mid-handshake -- is
    /// a stability failure and a cooldown too, since the punch the
    /// crate would open next meets the same condition, and the
    /// cooldown is what stops it from opening one CONNECT round per
    /// refusal. Pinned by
    /// `a_punched_connection_that_closes_within_the_interval_cools_the_peer_down`,
    /// which closes without a reason.
    fn punched_closed(&mut self, direct: ConnectionId) {
        let Some((peer, since)) = self.stabilising.remove(&direct) else {
            return;
        };
        if self.now_ms.saturating_sub(since) < self.budgets.stability_ms {
            self.cool_down(peer);
            self.counters.lock().stability_failures += 1;
            self.events.push_back(HolePunchEvent::Unstable { peer });
        } else {
            self.counters.lock().upgrades_stable += 1;
        }
        self.publish();
    }

    /// Put `peer` in cooldown, bounding the map.
    fn cool_down(&mut self, peer: PeerId) {
        if self.cooldown.len() >= MAX_COOLDOWN_PEERS
            && let Some(soonest) = self
                .cooldown
                .iter()
                .min_by_key(|(_, until)| **until)
                .map(|(p, _)| *p)
        {
            // The soonest to expire goes, so a flood of failing peers
            // cannot hold the map open.
            self.cooldown.remove(&soonest);
        }
        self.cooldown
            .insert(peer, self.now_ms.saturating_add(self.budgets.cooldown_ms));
    }

    /// The attempt in flight toward `peer`, if any: the crate's event
    /// names the peer and not the relayed connection.
    fn attempt_toward(&self, peer: &PeerId) -> Option<ConnectionId> {
        self.attempts
            .iter()
            .find(|(_, a)| a.peer == *peer)
            .map(|(id, _)| *id)
    }

    fn publish(&self) {
        let mut c = self.counters.lock();
        c.inflight = self.attempts.len();
        c.cooldown_peers = self.cooldown.len();
        c.listeners_offered = self.offered.len();
    }
}

fn is_relayed(address: &Multiaddr) -> bool {
    address.iter().any(|p| matches!(p, Protocol::P2pCircuit))
}

impl NetworkBehaviour for HolePunchScope {
    type ConnectionHandler = <dcutr::Behaviour as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = HolePunchEvent;

    /// A relayed inbound is an attempt if §13 admits one -- the crate's
    /// handler initiates the CONNECT -- and a protocol-less connection
    /// otherwise; a direct inbound is recorded and handed to the crate.
    fn handle_established_inbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        if is_relayed(local) {
            if self.admit(id, peer).is_err() {
                return Ok(Either::Right(dummy::ConnectionHandler));
            }
        } else {
            self.direct.entry(peer).or_default().insert(id);
            self.punched(peer, id);
        }
        self.inner
            .handle_established_inbound_connection(id, peer, local, remote)
    }

    /// The same for an outbound: over a circuit the crate's handler
    /// awaits the remote's CONNECT; direct, the crate reads whether it
    /// was its own punch dial and reports the success.
    fn handle_established_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role: Endpoint,
        port: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        if is_relayed(addr) {
            if self.admit(id, peer).is_err() {
                return Ok(Either::Right(dummy::ConnectionHandler));
            }
        } else {
            self.reissued_in_flight.remove(&id);
            self.direct.entry(peer).or_default().insert(id);
            self.punched(peer, id);
        }
        self.inner
            .handle_established_outbound_connection(id, peer, addr, role, port)
    }

    fn handle_pending_inbound_connection(
        &mut self,
        id: ConnectionId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.inner
            .handle_pending_inbound_connection(id, local, remote)
    }

    /// `DCUTR.md` §6's boundary on a punch dial, before any socket, as
    /// a FILTER (ADR-0052 rule 5): a dial of the crate's own whose list
    /// carries a refused candidate is denied and reissued with the
    /// survivors as the wrapper's dial, the removed candidates counted
    /// by class; a list with no survivor ends the attempt
    /// `refused_by_class`. The wrapper's own dial is judged here too,
    /// as the backstop: a refused address in it is the filter's defect,
    /// counted under its own label, and the attempt ends. Every other
    /// dial in the Swarm passes through untouched. Refusals name the
    /// class and never the address.
    ///
    /// # Errors
    /// [`ConnectionDenied`] carrying the [`RefusedCandidate`], which the
    /// Swarm reports to the crate as a `DialFailure` this wrapper does
    /// not forward -- the attempt is over, or the dial was reissued.
    fn handle_pending_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: Option<PeerId>,
        addresses: &[Multiaddr],
        role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if let Some(target) = self.punch_dials.remove(&id) {
            let mut kept = Vec::new();
            let mut refused = Vec::new();
            for address in addresses {
                match self.within_boundary(address) {
                    Ok(()) => kept.push(address.clone()),
                    Err(class) => refused.push(class),
                }
            }
            let Some(first) = refused.first().copied() else {
                return self
                    .inner
                    .handle_pending_outbound_connection(id, peer, addresses, role);
            };
            if kept.is_empty() {
                if let Some(relayed) = self.attempt_toward(&target) {
                    self.end(relayed, Ending::RefusedByClass(first));
                }
                return Err(ConnectionDenied::new(RefusedCandidate(first)));
            }
            {
                let mut c = self.counters.lock();
                for class in &refused {
                    *c.candidates_removed.entry(class.label()).or_default() += 1;
                }
            }
            // EVERY OPTION OF THE CRATE'S KEPT: the peer, `Always`, and
            // the role the crate's dial carried -- `role` here IS its
            // `role_override`, `Listener` on the initiating end, which
            // is what makes the simultaneous open's Noise roles agree.
            let mut opts = libp2p::swarm::dial_opts::DialOpts::peer_id(target)
                .addresses(kept)
                .condition(libp2p::swarm::dial_opts::PeerCondition::Always);
            if role == Endpoint::Listener {
                opts = opts.override_role();
            }
            let opts = opts.build();
            self.replacement_dials.insert(opts.connection_id(), target);
            self.replaced.insert(id);
            self.actions.push_back(opts);
            return Err(ConnectionDenied::new(DialReissued));
        }
        if let Some(target) = self.replacement_dials.remove(&id) {
            if let Some(class) = addresses
                .iter()
                .find_map(|address| self.within_boundary(address).err())
            {
                self.counters.lock().backstop_refusals += 1;
                if let Some(relayed) = self.attempt_toward(&target) {
                    self.end(relayed, Ending::RefusedByClass(class));
                }
                return Err(ConnectionDenied::new(RefusedCandidate(class)));
            }
            // The wrapper's own dial, admitted: on the wire from here,
            // its failure or establishment the attempt's. The crate is
            // not asked about it, as it would not be asked about any
            // other behaviour's.
            self.reissued_in_flight.insert(id, target);
            return Ok(Vec::new());
        }
        self.inner
            .handle_pending_outbound_connection(id, peer, addresses, role)
    }

    /// A relayed connection's close ends its attempt; a direct
    /// connection's close reaches the crate only if its establishment
    /// did, since the crate `expect`s the pair to match; a punch dial's
    /// failure reaches the crate only while the attempt is in flight,
    /// since the crate answers one with another CONNECT round and the
    /// attempt it belonged to may already have succeeded by the other
    /// end's dial (or ended any other way).
    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        // A CANDIDATE OUTSIDE THE BOUNDARY IS NEVER LEARNED: what the
        // crate holds is what it sends in a CONNECT, and a peer's
        // Identify observed this profile on loopback or a link-local
        // address as readily as on a public one.
        if let FromSwarm::NewExternalAddrCandidate(candidate) = &event
            && let Err(class) = self.within_boundary(candidate.addr)
        {
            *self
                .counters
                .lock()
                .candidates_withheld
                .entry(class.label())
                .or_default() += 1;
            return;
        }
        if let FromSwarm::DialFailure(failure) = &event {
            self.punch_dials.remove(&failure.connection_id);
            // THE DENIAL OF A DIAL THIS WRAPPER REISSUED is not the
            // crate's to answer with another CONNECT round: its
            // survivors are on the wire under the wrapper's own dial.
            if self.replaced.remove(&failure.connection_id) {
                return;
            }
            // THE WRAPPER'S OWN DIAL FAILED: the survivors did not
            // connect, and the crate knows nothing of this dial, so the
            // attempt ends here as a failure rather than waiting out the
            // horizon.
            if let Some(target) = self
                .replacement_dials
                .remove(&failure.connection_id)
                .or_else(|| self.reissued_in_flight.remove(&failure.connection_id))
            {
                if let Some(relayed) = self.attempt_toward(&target) {
                    self.end(relayed, Ending::Failed(failure.error.to_string()));
                }
                return;
            }
            if let Some(peer) = failure.peer_id
                && self.attempt_toward(&peer).is_none()
            {
                return;
            }
        }
        if let FromSwarm::ConnectionClosed(closed) = &event {
            self.punched.remove(&closed.connection_id);
            if closed.endpoint.is_relayed() {
                self.end(closed.connection_id, Ending::Abandoned);
            } else {
                self.punched_closed(closed.connection_id);
                let forwarded = self
                    .direct
                    .get_mut(&closed.peer_id)
                    .is_some_and(|set| set.remove(&closed.connection_id));
                if self
                    .direct
                    .get(&closed.peer_id)
                    .is_some_and(HashSet::is_empty)
                {
                    self.direct.remove(&closed.peer_id);
                }
                if !forwarded {
                    return;
                }
            }
        }
        self.inner.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.inner.on_connection_handler_event(peer, id, event);
    }

    /// The crate's outcome ends the attempt; the wrapper's own events
    /// come first; every other action passes.
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        loop {
            if let Some(event) = self.events.pop_front() {
                return Poll::Ready(ToSwarm::GenerateEvent(event));
            }
            if let Some(opts) = self.actions.pop_front() {
                return Poll::Ready(ToSwarm::Dial { opts });
            }
            match self.inner.poll(cx) {
                Poll::Ready(ToSwarm::GenerateEvent(dcutr::Event {
                    remote_peer_id,
                    result,
                })) => {
                    if let Some(relayed) = self.attempt_toward(&remote_peer_id) {
                        let ending = match result {
                            Ok(_) => Ending::Succeeded,
                            Err(e) => Ending::Failed(e.to_string()),
                        };
                        self.end(relayed, ending);
                    }
                }
                Poll::Ready(ToSwarm::Dial { opts }) => {
                    // THE CRATE'S OWN DIAL, remembered by the id it
                    // minted so the pending hook knows which of the
                    // Swarm's dials to judge.
                    if let Some(peer) = opts.get_peer_id() {
                        self.punch_dials.insert(opts.connection_id(), peer);
                    }
                    return Poll::Ready(ToSwarm::Dial { opts });
                }
                Poll::Ready(other) => {
                    // Not `GenerateEvent`: handled above. `map_out`
                    // calls this closure for that variant alone, so it
                    // maps an event that cannot arrive to an ending that
                    // says so rather than to nothing.
                    return Poll::Ready(other.map_out(|event| HolePunchEvent::Ended {
                        peer: event.remote_peer_id,
                        ending: Ending::Failed("unmatched outcome".to_owned()),
                    }));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use libp2p::core::ConnectedPoint;
    use libp2p::swarm::behaviour::ConnectionClosed;

    fn peer() -> PeerId {
        libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id()
    }

    fn scope(budgets: HolePunchBudgets) -> HolePunchScope {
        HolePunchScope::new(dcutr::Behaviour::new(peer()), budgets)
    }

    /// The subject's own id, for the circuit's trailing component.
    const ME: &str = "12D3KooWCLxLXFHqvfsHVLDcNsSpZBQq1M1KMRgQRLLLnHTv7oQD";

    fn circuit(relay: PeerId, me: PeerId) -> Multiaddr {
        format!("/ip4/192.0.2.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{me}")
            .parse()
            .expect("a circuit address")
    }

    /// A global address: inside the boundary whatever this node
    /// listens on.
    fn direct() -> Multiaddr {
        "/ip4/93.184.216.34/tcp/4001".parse().expect("an address")
    }

    fn loopback() -> Multiaddr {
        "/ip4/127.0.0.1/tcp/4001".parse().expect("an address")
    }

    fn lan() -> Multiaddr {
        "/ip4/192.168.7.20/tcp/4001".parse().expect("an address")
    }

    /// A relayed inbound from `peer`, as the Swarm hands it over.
    fn relayed_inbound(
        scope: &mut HolePunchScope,
        id: usize,
        peer: PeerId,
    ) -> THandler<HolePunchScope> {
        let relay = self::peer();
        scope
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(id),
                peer,
                &circuit(relay, ME.parse().expect("a peer id")),
                &format!("/p2p/{peer}").parse().expect("an address"),
            )
            .expect("never denied")
    }

    fn direct_inbound(scope: &mut HolePunchScope, id: usize, peer: PeerId) {
        let _ = scope
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(id),
                peer,
                &direct(),
                &direct(),
            )
            .expect("never denied");
    }

    /// Poll past the wrapper's events to its next dial.
    fn next_dial(scope: &mut HolePunchScope) -> libp2p::swarm::dial_opts::DialOpts {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        loop {
            match scope.poll(&mut cx) {
                Poll::Ready(ToSwarm::GenerateEvent(_)) => {}
                Poll::Ready(ToSwarm::Dial { opts }) => return opts,
                other => panic!("expected a dial, got {other:?}"),
            }
        }
    }

    fn drain(scope: &mut HolePunchScope) -> Vec<HolePunchEvent> {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        let mut out = Vec::new();
        while let Poll::Ready(ToSwarm::GenerateEvent(e)) = scope.poll(&mut cx) {
            out.push(e);
        }
        out
    }

    fn closed<'a>(id: usize, peer: PeerId, endpoint: &'a ConnectedPoint) -> FromSwarm<'a> {
        FromSwarm::ConnectionClosed(ConnectionClosed {
            peer_id: peer,
            connection_id: ConnectionId::new_unchecked(id),
            endpoint,
            cause: None,
            remaining_established: 0,
        })
    }

    #[test]
    fn a_relayed_connection_is_an_attempt_and_the_ceilings_decline_the_next() {
        let mut s = scope(HolePunchBudgets {
            max_inflight: 2,
            max_inflight_per_peer: 1,
            cooldown_ms: 1_000,
            ..HolePunchBudgets::default()
        });
        let a = peer();
        let b = peer();
        let c = peer();
        // The first relayed connection to `a` is an attempt with the
        // crate's handler; the second toward `a` is declined per peer
        // and gets a protocol-less handler.
        assert!(matches!(relayed_inbound(&mut s, 1, a), Either::Left(_)));
        assert!(s.is_punching(&a));
        assert!(matches!(relayed_inbound(&mut s, 2, a), Either::Right(_)));
        // `b` fills the global ceiling; `c` is declined for it.
        assert!(matches!(relayed_inbound(&mut s, 3, b), Either::Left(_)));
        assert!(matches!(relayed_inbound(&mut s, 4, c), Either::Right(_)));
        assert!(!s.is_punching(&c));
        assert_eq!(
            drain(&mut s),
            vec![
                HolePunchEvent::Started { peer: a },
                HolePunchEvent::Declined {
                    peer: a,
                    reason: Decline::PeerBusy
                },
                HolePunchEvent::Started { peer: b },
                HolePunchEvent::Declined {
                    peer: c,
                    reason: Decline::Busy
                },
            ]
        );
        let counters = s.counter_handle().snapshot();
        assert_eq!(counters.inflight, 2);
        assert_eq!(counters.declined.get("declined_peer_busy"), Some(&1));
        assert_eq!(counters.declined.get("declined_busy"), Some(&1));
    }

    #[test]
    fn a_peer_with_a_direct_connection_is_never_punched_toward() {
        let mut s = scope(HolePunchBudgets::default());
        let a = peer();
        direct_inbound(&mut s, 1, a);
        assert!(matches!(relayed_inbound(&mut s, 2, a), Either::Right(_)));
        assert_eq!(
            drain(&mut s),
            vec![HolePunchEvent::Declined {
                peer: a,
                reason: Decline::DirectExists
            }]
        );
        // THE CONTROL: the direct connection closes and the next relayed
        // one is an attempt.
        let endpoint = ConnectedPoint::Listener {
            local_addr: direct(),
            send_back_addr: direct(),
        };
        s.on_swarm_event(closed(1, a, &endpoint));
        assert!(matches!(relayed_inbound(&mut s, 3, a), Either::Left(_)));
    }

    #[test]
    fn a_failure_starts_the_cooldown_and_a_success_or_abandonment_does_not() {
        let mut s = scope(HolePunchBudgets {
            cooldown_ms: 1_000,
            ..HolePunchBudgets::default()
        });
        let a = peer();
        s.tick(10);
        assert!(matches!(relayed_inbound(&mut s, 1, a), Either::Left(_)));
        s.end(
            ConnectionId::new_unchecked(1),
            Ending::Failed("no".to_owned()),
        );
        assert!(!s.is_punching(&a));
        // In cooldown: declined, until the cooldown elapses.
        assert!(matches!(relayed_inbound(&mut s, 2, a), Either::Right(_)));
        assert_eq!(s.counter_handle().snapshot().cooldown_peers, 1);
        s.tick(1_010);
        assert_eq!(s.counter_handle().snapshot().cooldown_peers, 0);
        assert!(matches!(relayed_inbound(&mut s, 3, a), Either::Left(_)));
        // A success clears whatever cooldown stood.
        s.end(ConnectionId::new_unchecked(3), Ending::Succeeded);
        assert!(matches!(relayed_inbound(&mut s, 4, a), Either::Left(_)));
        // The relayed connection closing abandons the attempt without a
        // cooldown (DCUTR.md section 7).
        let endpoint = ConnectedPoint::Listener {
            local_addr: circuit(peer(), peer()),
            send_back_addr: format!("/p2p/{a}").parse().expect("an address"),
        };
        s.on_swarm_event(closed(4, a, &endpoint));
        assert!(!s.is_punching(&a));
        assert!(matches!(relayed_inbound(&mut s, 5, a), Either::Left(_)));
        let events = drain(&mut s);
        let endings: Vec<&Ending> = events
            .iter()
            .filter_map(|e| match e {
                HolePunchEvent::Ended { ending, .. } => Some(ending),
                _ => None,
            })
            .collect();
        assert_eq!(
            endings,
            vec![
                &Ending::Failed("no".to_owned()),
                &Ending::Succeeded,
                &Ending::Abandoned
            ]
        );
        let counters = s.counter_handle().snapshot();
        assert_eq!(counters.attempts_ended.get("failed"), Some(&1));
        assert_eq!(counters.attempts_ended.get("succeeded"), Some(&1));
        assert_eq!(counters.attempts_ended.get("abandoned"), Some(&1));
    }

    #[test]
    fn an_attempt_past_the_horizon_is_counted_failed_and_releases_its_permit() {
        let mut s = scope(HolePunchBudgets {
            max_inflight: 1,
            ..HolePunchBudgets::default()
        });
        let a = peer();
        let b = peer();
        s.tick(0);
        assert!(matches!(relayed_inbound(&mut s, 1, a), Either::Left(_)));
        assert!(matches!(relayed_inbound(&mut s, 2, b), Either::Right(_)));
        s.tick(ATTEMPT_HORIZON_MS - 1);
        assert!(
            s.is_punching(&a),
            "one short of the horizon is still in flight"
        );
        s.tick(ATTEMPT_HORIZON_MS);
        assert!(!s.is_punching(&a));
        assert!(
            matches!(relayed_inbound(&mut s, 3, b), Either::Left(_)),
            "the permit is back"
        );
        assert!(
            matches!(relayed_inbound(&mut s, 4, a), Either::Right(_)),
            "and the timed-out peer is in cooldown"
        );
        assert_eq!(
            s.counter_handle()
                .snapshot()
                .attempts_ended
                .get("timed_out"),
            Some(&1)
        );
    }

    #[test]
    fn a_close_the_crate_was_never_told_the_establishment_of_never_reaches_it() {
        // The crate `expect`s a direct connection's close to match an
        // establishment it saw; a connection the class gate withheld
        // from this wrapper has no such establishment, and its close
        // must stop here. Forwarding it is a panic in the Swarm task.
        let mut s = scope(HolePunchBudgets::default());
        let a = peer();
        let endpoint = ConnectedPoint::Listener {
            local_addr: direct(),
            send_back_addr: direct(),
        };
        s.on_swarm_event(closed(7, a, &endpoint));
        // THE CONTROL: a forwarded establishment's close reaches the
        // crate and its bookkeeping matches.
        direct_inbound(&mut s, 8, a);
        s.on_swarm_event(closed(8, a, &endpoint));
        assert!(!s.direct.contains_key(&a));
    }

    #[test]
    fn a_direct_connection_during_an_attempt_is_the_success_whichever_end_dialled_it() {
        let mut s = scope(HolePunchBudgets::default());
        let a = peer();
        assert!(matches!(relayed_inbound(&mut s, 1, a), Either::Left(_)));
        // The OTHER end's dial lands here as a direct inbound: the
        // attempt succeeds, the cooldown (had there been one) clears.
        s.cooldown.insert(a, u64::MAX);
        direct_inbound(&mut s, 2, a);
        assert!(!s.is_punching(&a));
        assert!(!s.cooldown.contains_key(&a));
        assert!(
            s.take_punched(ConnectionId::new_unchecked(2), 0),
            "the runtime is told this connection was the punch"
        );
        assert!(!s.take_punched(ConnectionId::new_unchecked(2), 0), "once");
        assert_eq!(
            drain(&mut s),
            vec![
                HolePunchEvent::Started { peer: a },
                HolePunchEvent::Ended {
                    peer: a,
                    ending: Ending::Succeeded
                }
            ]
        );
        // THE CONTROL: a direct inbound with no attempt in flight ends
        // nothing and counts nothing.
        let b = peer();
        direct_inbound(&mut s, 3, b);
        assert!(drain(&mut s).is_empty());
        assert!(!s.take_punched(ConnectionId::new_unchecked(3), 0));
        assert_eq!(
            s.counter_handle()
                .snapshot()
                .attempts_ended
                .get("succeeded"),
            Some(&1)
        );
    }

    #[test]
    fn a_bound_listener_is_offered_once_and_forgotten_with_its_listener() {
        let mut s = scope(HolePunchBudgets::default());
        let listener = direct();
        assert!(s.offer_listener(&listener));
        assert!(!s.offer_listener(&listener), "once");
        assert!(
            !s.offer_listener(&circuit(peer(), peer())),
            "a relayed one never"
        );
        assert_eq!(s.offered.len(), 1);
        // THE BOUND: a listener that comes and goes on a fresh port each
        // time leaves nothing behind.
        for port in 1..=64u16 {
            let address: Multiaddr = format!("/ip4/93.184.216.5/tcp/{port}")
                .parse()
                .expect("an address");
            assert!(s.offer_listener(&address));
            assert!(s.forget_listener(&address));
        }
        assert_eq!(s.offered.len(), 1, "only the listener still bound is held");
        assert!(s.forget_listener(&listener));
        assert!(s.offer_listener(&listener), "bound again, offered again");
    }

    /// The boundary on what this profile offers and learns: a loopback
    /// listener is never offered, a private one is (it is its own
    /// private listener), a global one is; and a candidate the Swarm
    /// reports outside the boundary is withheld from the crate and
    /// counted by class, one inside it passes.
    #[test]
    fn a_loopback_listener_is_never_offered_and_a_loopback_candidate_never_learned() {
        let mut s = scope(HolePunchBudgets::default());
        assert!(!s.offer_listener(&loopback()));
        assert!(s.offer_listener(&lan()));
        assert!(s.offer_listener(&direct()));
        assert_eq!(s.offered.len(), 2);
        let withheld = |s: &HolePunchScope| {
            s.counter_handle()
                .snapshot()
                .candidates_withheld
                .values()
                .sum::<u64>()
        };
        for (address, expected) in [(loopback(), 1), (direct(), 1), (lan(), 1), (loopback(), 2)] {
            s.on_swarm_event(FromSwarm::NewExternalAddrCandidate(
                libp2p::swarm::behaviour::NewExternalAddrCandidate { addr: &address },
            ));
            assert_eq!(withheld(&s), expected, "{address}");
        }
        assert_eq!(
            s.counter_handle()
                .snapshot()
                .candidates_withheld
                .get("special_use"),
            Some(&2)
        );
        // A private candidate on a node with no private listener is
        // withheld too, by its own class.
        let mut s = scope(HolePunchBudgets::default());
        s.on_swarm_event(FromSwarm::NewExternalAddrCandidate(
            libp2p::swarm::behaviour::NewExternalAddrCandidate { addr: &lan() },
        ));
        assert_eq!(
            s.counter_handle()
                .snapshot()
                .candidates_withheld
                .get("private_without_private_listener"),
            Some(&1)
        );
    }

    /// The boundary at the pending hook as a FILTER: a punch dial whose
    /// list carries a loopback candidate beside an admitted one is
    /// denied and reissued with the admitted one alone -- the same
    /// peer, `Always`, the role override when this end initiates --
    /// the removed candidate counted by class, the attempt still in
    /// flight; the denial's own `DialFailure` is swallowed; and the
    /// reissued dial, judged at the same hook, passes. A dial that is
    /// not the crate's passes whatever it carries.
    #[test]
    fn a_punch_dial_carrying_a_refused_candidate_is_reissued_without_it() {
        let mut s = scope(HolePunchBudgets::default());
        let a = peer();
        assert!(matches!(relayed_inbound(&mut s, 1, a), Either::Left(_)));
        let dial = ConnectionId::new_unchecked(2);
        s.punch_dials.insert(dial, a);
        // The crate's dial on the initiating end carries the role
        // override, which the hook sees as `Listener`.
        let denied = s.handle_pending_outbound_connection(
            dial,
            Some(a),
            &[direct(), loopback()],
            Endpoint::Listener,
        );
        assert!(denied.is_err(), "the crate's dial is denied");
        assert!(s.is_punching(&a), "the attempt goes on");
        assert!(s.replaced.contains(&dial));
        assert_eq!(s.replacement_dials.len(), 1, "one dial reissued");
        assert_eq!(
            s.counter_handle()
                .snapshot()
                .candidates_removed
                .get("special_use"),
            Some(&1)
        );
        // The reissued dial reaches the Swarm from poll, before the
        // crate's own actions, with the admitted address alone, the
        // role override the crate's dial carried, and `Always` -- under
        // the builder's default condition the Swarm would refuse it,
        // the peer being connected over the circuit.
        let opts = next_dial(&mut s);
        assert_eq!(opts.get_peer_id(), Some(a));
        let reissued = opts.connection_id();
        assert!(s.replacement_dials.contains_key(&reissued));
        assert!(
            format!("{opts:?}").contains("Listener"),
            "the initiator's role override is kept: {opts:?}"
        );
        assert!(
            format!("{opts:?}").contains("Always"),
            "the dial condition is Always: {opts:?}"
        );
        assert!(
            !format!("{opts:?}").contains("127.0.0.1"),
            "the refused candidate is gone: {opts:?}"
        );
        // The denial's own failure does not reach the crate.
        let error = libp2p::swarm::DialError::Aborted;
        s.on_swarm_event(FromSwarm::DialFailure(
            libp2p::swarm::behaviour::DialFailure {
                peer_id: Some(a),
                error: &error,
                connection_id: dial,
            },
        ));
        assert!(s.is_punching(&a), "the swallowed denial ends nothing");
        assert!(s.replaced.is_empty(), "and the denied dial is forgotten");
        // The reissued dial at the hook: admitted, on the wire from
        // here, and the backstop did not fire.
        assert!(
            s.handle_pending_outbound_connection(
                reissued,
                Some(a),
                &[direct()],
                Endpoint::Listener
            )
            .is_ok()
        );
        assert_eq!(s.counter_handle().snapshot().backstop_refusals, 0);
        assert!(s.reissued_in_flight.contains_key(&reissued));
        // AND ITS FAILURE AFTER THE HOOK ends the attempt: the Swarm
        // reports it under the same id, the crate knows nothing of it.
        s.on_swarm_event(FromSwarm::DialFailure(
            libp2p::swarm::behaviour::DialFailure {
                peer_id: Some(a),
                error: &error,
                connection_id: reissued,
            },
        ));
        assert!(
            !s.is_punching(&a),
            "the reissued dial's failure ends the attempt"
        );
        assert!(s.cooldown.contains_key(&a));
        assert!(s.reissued_in_flight.is_empty());
        assert!(matches!(
            drain(&mut s).last(),
            Some(HolePunchEvent::Ended {
                ending: Ending::Failed(_),
                ..
            })
        ));
        // THE RESPONDING END's crate dial carries no override, and
        // neither does the reissue.
        let b = peer();
        let mut s = scope(HolePunchBudgets::default());
        let relay = peer();
        let _ = s
            .handle_established_outbound_connection(
                ConnectionId::new_unchecked(3),
                b,
                &circuit(relay, b),
                Endpoint::Dialer,
                PortUse::Reuse,
            )
            .expect("never denied");
        let dial = ConnectionId::new_unchecked(4);
        s.punch_dials.insert(dial, b);
        assert!(
            s.handle_pending_outbound_connection(
                dial,
                Some(b),
                &[direct(), loopback()],
                Endpoint::Dialer
            )
            .is_err()
        );
        let opts = next_dial(&mut s);
        assert!(
            format!("{opts:?}").contains("Dialer"),
            "the responder dials as a dialer: {opts:?}"
        );
        // THE CONTROL: a dial that is not the crate's passes with a
        // loopback address in it -- the boundary is the punch's.
        assert!(
            s.handle_pending_outbound_connection(
                ConnectionId::new_unchecked(7),
                Some(b),
                &[loopback()],
                Endpoint::Dialer
            )
            .is_ok()
        );
    }

    /// No survivor: the attempt ends `refused_by_class` naming the
    /// class and the peer is in cooldown; a private candidate on a node
    /// with no private listener is such a case, and beside a private
    /// listener it is admitted; and the BACKSTOP: a reissued dial that
    /// reaches the hook with a refused address is the filter's defect,
    /// counted under its own label, and ends the attempt.
    #[test]
    fn a_punch_dial_with_no_admitted_candidate_ends_the_attempt() {
        let mut s = scope(HolePunchBudgets::default());
        let a = peer();
        assert!(matches!(relayed_inbound(&mut s, 1, a), Either::Left(_)));
        let dial = ConnectionId::new_unchecked(2);
        s.punch_dials.insert(dial, a);
        assert!(
            s.handle_pending_outbound_connection(dial, Some(a), &[loopback()], Endpoint::Dialer)
                .is_err()
        );
        assert!(!s.is_punching(&a));
        assert!(s.cooldown.contains_key(&a), "the peer is in cooldown");
        assert!(s.replacement_dials.is_empty(), "nothing to reissue");
        assert_eq!(
            drain(&mut s).last(),
            Some(&HolePunchEvent::Ended {
                peer: a,
                ending: Ending::RefusedByClass(CandidateRefusal::SpecialUse)
            })
        );
        assert_eq!(
            s.counter_handle()
                .snapshot()
                .attempts_ended
                .get("refused_by_class"),
            Some(&1)
        );
        // A private candidate on a node with no private listener:
        // refused for that; beside a private listener: admitted.
        let b = peer();
        let mut s = scope(HolePunchBudgets::default());
        assert!(matches!(relayed_inbound(&mut s, 3, b), Either::Left(_)));
        let dial = ConnectionId::new_unchecked(4);
        s.punch_dials.insert(dial, b);
        assert!(
            s.handle_pending_outbound_connection(dial, Some(b), &[lan()], Endpoint::Dialer)
                .is_err()
        );
        assert!(matches!(
            drain(&mut s).last(),
            Some(HolePunchEvent::Ended {
                ending: Ending::RefusedByClass(CandidateRefusal::PrivateWithoutPrivateListener),
                ..
            })
        ));
        let c = peer();
        let mut s = scope(HolePunchBudgets::default());
        assert!(s.offer_listener(&lan()));
        assert!(matches!(relayed_inbound(&mut s, 5, c), Either::Left(_)));
        let dial = ConnectionId::new_unchecked(6);
        s.punch_dials.insert(dial, c);
        assert!(
            s.handle_pending_outbound_connection(dial, Some(c), &[lan()], Endpoint::Dialer)
                .is_ok()
        );
        assert!(s.is_punching(&c), "admitted: the attempt goes on");
        // THE BACKSTOP.
        let d = peer();
        let mut s = scope(HolePunchBudgets::default());
        assert!(matches!(relayed_inbound(&mut s, 7, d), Either::Left(_)));
        let reissued = ConnectionId::new_unchecked(8);
        s.replacement_dials.insert(reissued, d);
        assert!(
            s.handle_pending_outbound_connection(
                reissued,
                Some(d),
                &[direct(), loopback()],
                Endpoint::Dialer
            )
            .is_err()
        );
        assert!(!s.is_punching(&d));
        assert_eq!(s.counter_handle().snapshot().backstop_refusals, 1);
        assert_eq!(
            s.counter_handle()
                .snapshot()
                .attempts_ended
                .get("refused_by_class"),
            Some(&1)
        );
        // And a reissued dial whose survivors fail ends the attempt as
        // a failure rather than waiting out the horizon.
        let e = peer();
        let mut s = scope(HolePunchBudgets::default());
        assert!(matches!(relayed_inbound(&mut s, 9, e), Either::Left(_)));
        let reissued = ConnectionId::new_unchecked(10);
        s.replacement_dials.insert(reissued, e);
        let error = libp2p::swarm::DialError::Aborted;
        s.on_swarm_event(FromSwarm::DialFailure(
            libp2p::swarm::behaviour::DialFailure {
                peer_id: Some(e),
                error: &error,
                connection_id: reissued,
            },
        ));
        assert!(!s.is_punching(&e));
        assert!(s.cooldown.contains_key(&e));
        assert!(s.replacement_dials.is_empty());
    }

    /// A punched direct connection that closes within the stability
    /// interval is a stability failure: the peer cools down and the
    /// event says so; one that holds is an upgrade that held, counted
    /// on the tick or at its later close; an unpunched direct
    /// connection's close is neither.
    #[test]
    fn a_punched_connection_that_closes_within_the_interval_cools_the_peer_down() {
        let mut s = scope(HolePunchBudgets {
            stability_ms: 1_000,
            ..HolePunchBudgets::default()
        });
        let a = peer();
        s.tick(0);
        assert!(matches!(relayed_inbound(&mut s, 1, a), Either::Left(_)));
        direct_inbound(&mut s, 2, a);
        // THE RUNTIME'S CLOCK, not the last tick's: established at 900
        // on the runtime's clock while the wrapper's stands at 0, the
        // interval runs from 900, so a close at 1_000 is still inside
        // it -- measured from the tick it would have counted as held.
        assert!(s.take_punched(ConnectionId::new_unchecked(2), 900));
        assert_eq!(
            s.stabilising.get(&ConnectionId::new_unchecked(2)),
            Some(&(a, 900))
        );
        let endpoint = ConnectedPoint::Listener {
            local_addr: direct(),
            send_back_addr: direct(),
        };
        s.tick(1_000);
        assert!(
            s.stabilising.contains_key(&ConnectionId::new_unchecked(2)),
            "not yet held on the runtime's clock"
        );
        s.on_swarm_event(closed(2, a, &endpoint));
        assert!(s.cooldown.contains_key(&a), "the peer cools down");
        assert_eq!(s.counter_handle().snapshot().stability_failures, 1);
        assert!(matches!(
            drain(&mut s).last(),
            Some(HolePunchEvent::Unstable { peer }) if *peer == a
        ));
        // THE CONTROL: one that holds is counted on the tick and its
        // later close is nothing.
        let b = peer();
        let mut s = scope(HolePunchBudgets {
            stability_ms: 1_000,
            ..HolePunchBudgets::default()
        });
        s.tick(0);
        assert!(matches!(relayed_inbound(&mut s, 3, b), Either::Left(_)));
        direct_inbound(&mut s, 4, b);
        s.tick(1_000);
        assert_eq!(s.counter_handle().snapshot().upgrades_stable, 1);
        assert!(s.stabilising.is_empty(), "pruned once held");
        s.on_swarm_event(closed(4, b, &endpoint));
        assert!(!s.cooldown.contains_key(&b));
        assert_eq!(s.counter_handle().snapshot().stability_failures, 0);
        // And an unpunched direct connection's close is neither.
        let c = peer();
        direct_inbound(&mut s, 5, c);
        s.on_swarm_event(closed(5, c, &endpoint));
        assert!(!s.cooldown.contains_key(&c));
        assert_eq!(s.counter_handle().snapshot().stability_failures, 0);
    }

    #[test]
    fn a_reissued_dial_leaves_the_map_with_its_attempt() {
        let mut s = scope(HolePunchBudgets::default());
        let a = peer();
        assert!(matches!(relayed_inbound(&mut s, 1, a), Either::Left(_)));
        s.reissued_in_flight
            .insert(ConnectionId::new_unchecked(2), a);
        // The attempt ends some other way -- the relayed connection
        // closes -- with the reissued dial's establishment withheld.
        let endpoint = ConnectedPoint::Listener {
            local_addr: circuit(peer(), peer()),
            send_back_addr: format!("/p2p/{a}").parse().expect("an address"),
        };
        s.on_swarm_event(closed(1, a, &endpoint));
        assert!(s.reissued_in_flight.is_empty(), "gone with the attempt");
    }

    #[test]
    fn a_punch_dial_the_swarm_refused_before_the_hook_is_forgotten() {
        let mut s = scope(HolePunchBudgets::default());
        let a = peer();
        let dial = ConnectionId::new_unchecked(2);
        s.punch_dials.insert(dial, a);
        let error = libp2p::swarm::DialError::Aborted;
        s.on_swarm_event(FromSwarm::DialFailure(
            libp2p::swarm::behaviour::DialFailure {
                peer_id: Some(a),
                error: &error,
                connection_id: dial,
            },
        ));
        assert!(
            s.punch_dials.is_empty(),
            "the entry is gone with the failure"
        );
    }

    #[test]
    fn a_cooldown_flood_is_bounded() {
        let mut s = scope(HolePunchBudgets {
            max_inflight: 8,
            ..HolePunchBudgets::default()
        });
        for i in 0..(MAX_COOLDOWN_PEERS + 8) {
            let p = peer();
            assert!(matches!(relayed_inbound(&mut s, i, p), Either::Left(_)));
            s.end(
                ConnectionId::new_unchecked(i),
                Ending::Failed("no".to_owned()),
            );
        }
        assert_eq!(s.cooldown.len(), MAX_COOLDOWN_PEERS);
    }

    /// A network change gives up what was in flight -- the attempt
    /// keeps its permit while the crate's rounds run and ends
    /// `Abandoned` without a cooldown whatever the crate then reports
    /// -- lifts every cooldown, and stops judging punched connections
    /// in their interval; the next circuit to a peer that failed before
    /// starts a fresh attempt.
    #[test]
    fn a_network_change_abandons_attempts_lifts_cooldowns_and_stops_judging() {
        let mut s = scope(HolePunchBudgets {
            stability_ms: 1_000,
            ..HolePunchBudgets::default()
        });
        let a = peer();
        let b = peer();
        let c = peer();
        s.tick(0);
        // `a` failed and is in cooldown; `b` is in flight; `c` punched
        // and stabilising.
        assert!(matches!(relayed_inbound(&mut s, 1, a), Either::Left(_)));
        s.end(ConnectionId::new_unchecked(1), Ending::TimedOut);
        assert!(s.cooldown.contains_key(&a));
        assert!(matches!(relayed_inbound(&mut s, 2, b), Either::Left(_)));
        assert!(matches!(relayed_inbound(&mut s, 3, c), Either::Left(_)));
        direct_inbound(&mut s, 4, c);
        assert!(s.take_punched(ConnectionId::new_unchecked(4), 0));
        assert!(s.stabilising.contains_key(&ConnectionId::new_unchecked(4)));
        let _ = s.offer_listener(&direct());
        drain(&mut s);

        s.network_changed();
        assert!(s.cooldown.is_empty(), "every cooldown lifted");
        assert!(s.stabilising.is_empty(), "nothing judged");
        assert!(s.offered.is_empty(), "the listener set starts over");
        // THE PERMIT IS KEPT: `b`'s attempt is still in flight for the
        // ceiling, so a second circuit from `b` is busy, not a new
        // attempt beside the crate's running rounds.
        assert_eq!(s.inflight_toward(&b), 1);
        assert!(matches!(relayed_inbound(&mut s, 5, b), Either::Right(_)));
        assert_eq!(
            s.counter_handle()
                .snapshot()
                .declined
                .get("declined_peer_busy"),
            Some(&1)
        );
        // AND ENDS ABANDONED whatever the crate reports -- here a
        // failure, which would have cooled `b` down -- with no cooldown.
        s.end(
            ConnectionId::new_unchecked(2),
            Ending::Failed("late".to_owned()),
        );
        assert!(
            !s.cooldown.contains_key(&b),
            "no cooldown for a given-up attempt"
        );
        let events = drain(&mut s);
        assert!(
            events.iter().any(|e| matches!(
                e,
                HolePunchEvent::Ended { peer, ending: Ending::Abandoned } if *peer == b
            )),
            "abandoned, not failed: {events:?}"
        );
        assert_eq!(
            s.counter_handle()
                .snapshot()
                .attempts_ended
                .get("abandoned"),
            Some(&1)
        );
        assert_eq!(
            s.counter_handle().snapshot().attempts_ended.get("failed"),
            None
        );
        assert!(s.attempts.is_empty(), "and now nothing in flight");
        // A close of the punched connection now is neither a failure
        // nor a cooldown; and `a`'s next circuit begins an attempt.
        let endpoint = ConnectedPoint::Listener {
            local_addr: direct(),
            send_back_addr: direct(),
        };
        s.tick(500);
        s.on_swarm_event(closed(4, c, &endpoint));
        assert!(!s.cooldown.contains_key(&c));
        assert_eq!(s.counter_handle().snapshot().stability_failures, 0);
        assert!(
            matches!(relayed_inbound(&mut s, 6, a), Either::Left(_)),
            "punchable again"
        );
    }
}
