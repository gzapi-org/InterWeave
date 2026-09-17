// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The AutoNAT v2 client's adapter: the pinned behaviour on one side,
//! `ReachabilityManager` on the other, and this module deciding nothing
//! either of them already decides.
//!
//! # What sits where (`AUTONAT.md` §2)
//!
//! The Swarm owns the behaviour; the manager owns policy and evidence.
//! This driver is the seam: it builds the behaviour from configuration,
//! feeds every probe outcome to the manager, and turns the manager's
//! verdict into what the Swarm advertises. It also holds the one thing
//! neither side can -- WHEN an address goes back to the sweep -- because
//! the crate has no schedule beyond its tick and the manager reads no
//! clock.
//!
//! # The client never dials, and this driver does (route 2)
//!
//! A probe is a request over a connection this profile already opened
//! (CLAUDE.md §1: every `ToSwarm` the client emits is a confirmation, an
//! event or a handler notification). Reaching a static server this
//! profile is not connected to is therefore an ordinary `attempt_dial`
//! under `DialOrigin::AutonatProbe`, admitted by the root gate like any
//! other dial and slowed by the gate's own backoff when the server will
//! not connect. Under `use_authorized_identify_servers`, an AUTHORIZED
//! peer whose Identify -- on any connection, an inbound included --
//! advertises the dial-request protocol is dialled the same way, to
//! the listen addresses it reported, so the client gains an outbound
//! connection it can probe over; the set of such peers is bounded like
//! the static list. With the knob off, no such dial is made, and an
//! Identify-learned server is only ever a peer already connected for
//! another reason (`AUTONAT.md` §3, Amendment 2026-09-09: the knob
//! governs CONNECTION, not selection).
//!
//! # Who is a server (`AUTONAT.md` §3, Amendment 2026-09-09)
//!
//! The client picks its server at random among the connections THIS
//! PROFILE DIALLED whose remote advertised the dial-request protocol;
//! nothing here can rank or veto that pick. So the eligible set the
//! manager is told about is exactly that: a peer with an outbound
//! connection whose Identify carries the server protocol, offered as
//! `Static` when configured and `Identify` otherwise. A report from any
//! other peer is refused by the manager by name (`RefusedReport`), and
//! the refusal is counted here under §9's `refused_*` outcomes -- an
//! adapter that offered the wrong set would otherwise sit `unknown`
//! forever with nothing to say so.
//!
//! # The tick is the crate's; the cadence is the manager's (§4, note of 2026-09-17)
//!
//! `Config::with_probe_interval` is left at its five-second default and
//! `with_max_candidates` is the one knob set from configuration. What
//! this driver schedules is `retest`: a verified address is returned to
//! the sweep every `refresh_interval`; an address one server affirmed
//! while the threshold wants more goes back after the base retry delay
//! (`second_observer`); an address a server called unreachable goes back
//! under the dial gate's 30 s / 5 min backoff (`retry`). A server that
//! connects and never answers is the crate's `Io` loop and reaches
//! nothing here; its rate is the tick times `max_candidates`, as §4 says.
//!
//! # What the Swarm advertises is the verdict, not a confirmation
//!
//! `ScopedCandidates` swallows the crate's `ExternalAddrConfirmed`. An
//! address enters the Swarm's external set when the manager's verdict
//! lists it as verified and leaves when it no longer does -- which is the
//! withdrawal ADR-0051 records as the adapter's, since the crate never
//! emits `ExternalAddrExpired`.
//!
//! # Route 3: the dial-back is an inbound from an infrastructure peer
//!
//! A server answers a probe by dialling us back, and that connection is
//! inbound from a peer that may be `ConnectivityInfrastructureOnly`. The
//! retention arm in `dialing.rs` asks under `DialOrigin::Manual` and
//! refuses that class outright; for a peer this driver holds as a server
//! it asks under `AutonatProbe` instead, and the connection is retained
//! -- class-gated, so it is offered Identify and the autonat protocols
//! and nothing else. "A peer this driver holds as a server" rather than
//! "a peer with an outstanding probe", because the crate emits no
//! probe-start event and the manager holds no in-flight state: what is
//! knowable is who was dialled and offered. The owner chose this on
//! 2026-09-17 over a window the adapter would track itself.

use std::collections::{BTreeMap, HashMap};

use interweave_profile_config::connectivity::AutonatClientConfig;
use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::reachability::{
    ConnectivityChanged, ProbeOutcome, ReachabilityConfig, ReachabilityManager,
    ReachabilityVerdict, RefusedReport, ServerSource, is_probeable_address,
};
use interweave_transport_runtime::{
    ConnectionManager, DialOrigin, RETRY_BASE_MS, retry_backoff_ms,
};
use libp2p::autonat::v2::client::{self as client, Behaviour as ClientBehaviour};
use libp2p::swarm::ConnectionId;
use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;
use libp2p::{Multiaddr, PeerId, identify, multiaddr::Protocol};
use rand::rngs::OsRng;

use super::dialing::{OpenConnection, attempt_dial};
use super::messages::{DialRefusal, SwarmEvent};
use crate::behaviour::SubstrateBehaviourEvent;
use crate::candidate_scope::ScopedCandidates;
use crate::gated_swarm::GatedSwarm;
use crate::outbound_gate::InFlightTickets;

/// The dial-request protocol a server advertises through Identify --
/// the crate's own constant is `pub(crate)`, so it is restated here and
/// pinned against the vendored source by a test.
pub const DIAL_REQUEST_PROTOCOL: &str = "/libp2p/autonat/2/dial-request";

/// The Display prefix of the crate's `AddressNotReachable` error.
///
/// `client::Event::result` is `Result<(), Error>` with an `Error` the
/// crate does not export (upstream 0.15.0 has the same shape), so the
/// one outcome class that reaches an event cannot be matched by
/// variant. It is matched by its `#[error]` text instead, pinned against
/// the vendored source by a test -- rather than mapping every `Err` to
/// "unreachable", which would let a re-vendor that started emitting
/// `Io` count a timeout as a server's failure vote.
pub const ADDRESS_NOT_REACHABLE_PREFIX: &str = "Address is not reachable";

/// One configured server: its identity and the address to dial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticServer {
    /// The server's PeerId, from the address's `/p2p/` component.
    pub peer: TransportIdentity,
    /// The whole configured multiaddr, as given.
    pub address: String,
}

/// The client's settings, translated from `profile-config` here rather
/// than there: the neutral configuration crate names no libp2p type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutonatClientSettings {
    /// Servers this profile guarantees to DIAL under `AutonatProbe`.
    pub static_servers: Vec<StaticServer>,
    /// Whether an Identify-learned server may be dialled for autonat
    /// purposes. Off: only static servers are dialled.
    pub use_authorized_identify_servers: bool,
    /// `ReachabilityConfig::required_distinct_successes`.
    pub required_distinct_successes: u32,
    /// `ReachabilityConfig::success_evidence_ttl_ms`.
    pub success_evidence_ttl_ms: u64,
    /// The manager's cadence for re-testing a verified address.
    pub refresh_interval_ms: u64,
    /// `Config::with_max_candidates` -- the one crate knob set here.
    pub max_candidate_addresses_per_cycle: usize,
}

impl AutonatClientSettings {
    /// Translate the validated profile block.
    ///
    /// # Errors
    /// A static server address that is not a multiaddr, or carries no
    /// `/p2p/` component, or whose PeerId the neutral grammar refuses.
    /// `profile-config` checks the shape already; this is the boundary
    /// that needs the libp2p parse, and a value that passed there and
    /// fails here is a defect in one of the two, so it is named.
    pub fn from_profile(config: &AutonatClientConfig) -> Result<Self, &'static str> {
        let mut static_servers = Vec::with_capacity(config.static_servers.len());
        for address in &config.static_servers {
            let multiaddr: Multiaddr = address
                .parse()
                .map_err(|_| "autonat static_servers: not a multiaddr")?;
            let peer_id = multiaddr
                .iter()
                .find_map(|p| match p {
                    Protocol::P2p(id) => Some(id),
                    _ => None,
                })
                .ok_or("autonat static_servers: no /p2p/ component")?;
            let peer = TransportIdentity::parse(peer_id.to_base58())
                .map_err(|_| "autonat static_servers: PeerId outside the neutral grammar")?;
            static_servers.push(StaticServer {
                peer,
                address: address.clone(),
            });
        }
        let settings = Self {
            static_servers,
            use_authorized_identify_servers: config.use_authorized_identify_servers,
            required_distinct_successes: config.required_distinct_successes,
            success_evidence_ttl_ms: u64::from(config.success_evidence_ttl_ms),
            refresh_interval_ms: u64::from(config.refresh_interval_ms),
            max_candidate_addresses_per_cycle: config.max_candidate_addresses_per_cycle as usize,
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Refuse a configuration the driver cannot honour.
    ///
    /// The same ceilings `config.schema.yaml` states, because this
    /// boundary is reachable without `profile-config` (a composition root
    /// building settings by hand) and a zero here is a manager that
    /// refuses to build or a crate that probes nothing.
    ///
    /// # Errors
    /// The first field outside its range, named.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=4).contains(&self.required_distinct_successes) {
            return Err("autonat required_distinct_successes must be 1..=4");
        }
        if !(60_000..=3_600_000).contains(&self.success_evidence_ttl_ms) {
            return Err("autonat success_evidence_ttl must be 1m..=1h");
        }
        if !(60_000..=1_800_000).contains(&self.refresh_interval_ms) {
            return Err("autonat refresh_interval must be 1m..=30m");
        }
        if !(1..=16).contains(&self.max_candidate_addresses_per_cycle) {
            return Err("autonat max_candidate_addresses_per_cycle must be 1..=16");
        }
        if self.static_servers.len() > 16 {
            return Err("autonat static_servers holds at most 16 entries");
        }
        Ok(())
    }

    fn reachability(&self) -> ReachabilityConfig {
        ReachabilityConfig {
            required_distinct_successes: self.required_distinct_successes,
            success_evidence_ttl_ms: self.success_evidence_ttl_ms,
        }
    }
}

/// Build the client from settings -- and ONLY `with_max_candidates`.
///
/// `with_probe_interval` is deliberately not called: the tick stays at
/// the crate's default (`AUTONAT.md` §4, note of 2026-09-17). Pinned by
/// `the_client_is_built_with_max_candidates_and_the_default_tick`.
#[must_use]
pub fn build_behaviour(settings: &AutonatClientSettings) -> ScopedCandidates<ClientBehaviour> {
    ScopedCandidates::new(ClientBehaviour::new(
        OsRng,
        client::Config::default().with_max_candidates(settings.max_candidate_addresses_per_cycle),
    ))
}

/// Why an address is being returned to the sweep -- §9's
/// `autonat_retests_total{reason}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetestReason {
    /// A verified address, re-tested every `refresh_interval`.
    Refresh,
    /// One server affirmed it and the threshold wants more.
    SecondObserver,
    /// A server called it unreachable; back under the gate's backoff.
    Retry,
}

/// How long a candidate may go with no outcome before it is returned
/// to the sweep: the gate's base retry delay, which comfortably covers
/// the crate's 10-second request timeout plus a server's dial-back.
///
/// ADR-0051: "reaching a verdict requires the caller's own timeout,
/// because silence is indistinguishable from a probe still in flight".
/// This is that timeout. A candidate can be silent for three reasons
/// the crate never reports -- the connection its request rode closed,
/// the server answered success with no dial-back landing, the handler's
/// ten-slot request map was full -- and each leaves it `Pending`, which
/// the crate's tick never sweeps. Pinned by
/// `a_candidate_with_no_outcome_is_retested_after_the_silence_bound`.
pub const SILENCE_MS: u64 = RETRY_BASE_MS;

/// What the driver knows about one candidate's probing.
///
/// One entry per candidate the wrapper forwards, pruned to that set on
/// every tick, so bounded with it. The failure count lives HERE and not
/// on the pending re-test, because a re-test is taken off the schedule
/// when it fires and its outcome arrives later: an earlier version kept
/// `attempts` on the schedule entry, deleted the entry at `retest`, and
/// so restarted every backoff at 30 s. Review finding on PR #89.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tracked {
    /// The last moment anything happened for this candidate: an
    /// outcome arrived, a re-test was issued, or it was first seen.
    last_activity_ms: u64,
    /// Consecutive reported failures, for the retry backoff. Reset by a
    /// success.
    failures: u32,
    /// A re-test waiting to fire, if any.
    due: Option<(u64, RetestReason)>,
}

impl Tracked {
    const fn seen(now_ms: u64) -> Self {
        Self {
            last_activity_ms: now_ms,
            failures: 0,
            due: None,
        }
    }
}

/// Most servers learned through Identify this driver will dial --
/// the same ceiling `config.schema.yaml` puts on `static_servers`.
pub const MAX_LEARNED_SERVERS: usize = 16;

/// Most addresses kept per learned server: Identify's `listen_addrs`
/// is peer-asserted and unbounded, and every other peer-asserted
/// address list in this runtime is capped at the address book's eight.
pub const MAX_TARGET_ADDRESSES: usize = 8;

/// A server this driver dials on its tick: where, why, and how the
/// last attempt went.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DialTarget {
    /// The addresses to try, in order; the configured one for a static
    /// server, the Identify-reported listen addresses for a learned one.
    addresses: Vec<String>,
    source: ServerSource,
    /// A dial this driver started and has not seen settle.
    in_flight: bool,
    /// Not before this moment; the gate's backoff applied here too,
    /// because the reconnect scheduler re-dials under an origin the
    /// gate refuses for an infrastructure peer, so nobody else will.
    next_attempt_at_ms: u64,
    attempts: u32,
}

impl DialTarget {
    fn new(addresses: Vec<String>, source: ServerSource) -> Self {
        Self {
            addresses,
            source,
            in_flight: false,
            next_attempt_at_ms: 0,
            attempts: 0,
        }
    }
}

/// The driver's state: the manager, the schedule, and the counters.
#[derive(Debug)]
pub struct AutonatState {
    settings: AutonatClientSettings,
    manager: ReachabilityManager,
    /// Per candidate address, pruned to the wrapper's set each tick.
    schedule: BTreeMap<String, Tracked>,
    /// Servers this driver dials: every static one, plus -- under the
    /// knob -- at most [`MAX_LEARNED_SERVERS`] learned through Identify.
    targets: BTreeMap<TransportIdentity, DialTarget>,
    /// External addresses this driver has added to the Swarm.
    advertised: Vec<String>,
    /// The listener set at the last tick, to notice a network change.
    listeners: Vec<String>,
    /// The wrapper's truncation count at the last tick, to notice a
    /// new refusal and say so once.
    truncated_seen: usize,
    last_truncation_report_ms: u64,
    refused_unknown_server: usize,
    refused_untracked_address: usize,
    retests: [usize; 3],
}

impl AutonatState {
    /// Build the driver's state.
    ///
    /// # Errors
    /// The manager's own refusal of a zero bound.
    pub fn new(settings: &AutonatClientSettings) -> Result<Self, &'static str> {
        let manager = ReachabilityManager::new(settings.reachability())
            .map_err(|_| "autonat: the reachability configuration has a zero bound")?;
        // ONE TARGET PER PEER, every address kept: two entries naming
        // one PeerId are two routes to one server, not a second server,
        // and an earlier version kept only the last.
        let mut targets: BTreeMap<TransportIdentity, DialTarget> = BTreeMap::new();
        for server in &settings.static_servers {
            targets
                .entry(server.peer.clone())
                .and_modify(|t| {
                    if !t.addresses.contains(&server.address) {
                        t.addresses.push(server.address.clone());
                    }
                })
                .or_insert_with(|| {
                    DialTarget::new(vec![server.address.clone()], ServerSource::Static)
                });
        }
        Ok(Self {
            settings: settings.clone(),
            manager,
            schedule: BTreeMap::new(),
            targets,
            advertised: Vec::new(),
            listeners: Vec::new(),
            truncated_seen: 0,
            last_truncation_report_ms: 0,
            refused_unknown_server: 0,
            refused_untracked_address: 0,
            retests: [0; 3],
        })
    }

    /// The manager's verdict.
    #[must_use]
    pub fn verdict(&self) -> &ReachabilityVerdict {
        self.manager.state()
    }

    /// Whether `peer` is a server this driver offered to the manager --
    /// the route-3 question the retention arm asks.
    #[must_use]
    pub fn is_server(&self, peer: &TransportIdentity) -> bool {
        self.manager.is_server(peer)
    }

    /// §9 `autonat_probes_total{outcome=refused_unknown_server}`.
    #[must_use]
    pub const fn refused_unknown_server(&self) -> usize {
        self.refused_unknown_server
    }

    /// §9 `autonat_probes_total{outcome=refused_untracked_address}`.
    #[must_use]
    pub const fn refused_untracked_address(&self) -> usize {
        self.refused_untracked_address
    }

    /// §9 `autonat_retests_total{reason}`.
    #[must_use]
    pub const fn retests(&self, reason: RetestReason) -> usize {
        self.retests[reason as usize]
    }

    /// Whether `peer` should be offered as a server: it has an outbound
    /// connection this profile opened, and it advertised the protocol.
    fn offer_server(
        &mut self,
        peer: &TransportIdentity,
        protocols: &[libp2p::StreamProtocol],
        open: &HashMap<ConnectionId, OpenConnection>,
    ) -> bool {
        if !protocols
            .iter()
            .any(|p| p.as_ref() == DIAL_REQUEST_PROTOCOL)
        {
            return false;
        }
        // DIALLED, not merely connected: the client installs its
        // dial-request handler only on a connection this profile
        // opened, so a peer that dialled us is never its server.
        let dialled = open.values().any(|c| c.peer == *peer && c.origin.is_some());
        if !dialled {
            return false;
        }
        let source = match self.targets.get(peer) {
            Some(t) if t.source == ServerSource::Static => ServerSource::Static,
            _ => ServerSource::Identify,
        };
        self.manager.add_server(peer.clone(), source)
    }

    /// Under `use_authorized_identify_servers`: a peer that advertised
    /// the protocol and is authorized becomes a dial target, so the
    /// client gains an outbound connection to probe over. Returns
    /// whether it was added.
    fn learn_server(
        &mut self,
        peer: &TransportIdentity,
        class: interweave_transport_runtime::ConnectionClass,
        protocols: &[libp2p::StreamProtocol],
        listen_addrs: &[Multiaddr],
    ) -> bool {
        if !self.settings.use_authorized_identify_servers
            || class == interweave_transport_runtime::ConnectionClass::Unauthorized
            || !protocols
                .iter()
                .any(|p| p.as_ref() == DIAL_REQUEST_PROTOCOL)
        {
            return false;
        }
        let addresses: Vec<String> = listen_addrs
            .iter()
            .take(MAX_TARGET_ADDRESSES)
            .map(ToString::to_string)
            .collect();
        if addresses.is_empty() {
            return false;
        }
        // A KNOWN LEARNED TARGET TAKES THE FRESH ADDRESSES (§3: "on
        // fresh evidence"); a static one keeps its configured route.
        if let Some(target) = self.targets.get_mut(peer) {
            if target.source == ServerSource::Identify {
                target.addresses = addresses;
            }
            return false;
        }
        // BOUNDED: the learned set can be grown by any authorized peer
        // that advertises the protocol, so it is capped at the static
        // list's own ceiling, first come.
        let learned = self
            .targets
            .values()
            .filter(|t| t.source == ServerSource::Identify)
            .count();
        if learned >= MAX_LEARNED_SERVERS {
            return false;
        }
        self.targets.insert(
            peer.clone(),
            DialTarget::new(addresses, ServerSource::Identify),
        );
        true
    }

    /// Fold one probe outcome in; `Some` when the verdict changed.
    fn record(
        &mut self,
        address: &str,
        server: &TransportIdentity,
        outcome: ProbeOutcome,
        now_ms: u64,
    ) -> Result<Option<ConnectivityChanged>, RefusedReport> {
        let result = self
            .manager
            .record_outcome(address, server, outcome, now_ms);
        match &result {
            Err(RefusedReport::UnknownServer) => self.refused_unknown_server += 1,
            Err(RefusedReport::UntrackedAddress) => self.refused_untracked_address += 1,
            Ok(_) => self.reschedule(address, outcome, now_ms),
        }
        result
    }

    /// Decide when `address` goes back to the sweep after `outcome`.
    fn reschedule(&mut self, address: &str, outcome: ProbeOutcome, now_ms: u64) {
        // BOUNDED WITH THE CANDIDATE SET: an address reaches here only
        // through a report the manager accepted, which it does only for
        // a tracked candidate, and the tick prunes this map to that set.
        let verified = self
            .manager
            .state()
            .verified_addresses()
            .iter()
            .any(|a| a == address);
        // A SECOND OBSERVER IS ASKED FOR ONLY WHEN ONE CAN ANSWER: with
        // fewer distinct servers known than the threshold needs, a
        // re-test every 30 s would be a dial-back at the same server
        // forever, for a verdict it cannot reach. Such an address waits
        // the refresh interval instead.
        let enough_servers =
            self.manager.dial_order().len() >= self.settings.required_distinct_successes as usize;
        let refresh = self.settings.refresh_interval_ms;
        let entry = self
            .schedule
            .entry(address.to_owned())
            .or_insert_with(|| Tracked::seen(now_ms));
        entry.last_activity_ms = now_ms;
        entry.due = Some(match outcome {
            ProbeOutcome::Reachable if verified || !enough_servers => {
                entry.failures = 0;
                (now_ms.saturating_add(refresh), RetestReason::Refresh)
            }
            ProbeOutcome::Reachable => {
                entry.failures = 0;
                (
                    now_ms.saturating_add(RETRY_BASE_MS),
                    RetestReason::SecondObserver,
                )
            }
            ProbeOutcome::Unreachable => {
                let delay = retry_backoff_ms(entry.failures);
                entry.failures = entry.failures.saturating_add(1);
                (now_ms.saturating_add(delay), RetestReason::Retry)
            }
        });
    }

    /// Return every tracked candidate to the sweep now: the answer to a
    /// network change, whose evidence the manager has just forgotten.
    fn retest_all(&mut self, swarm: &mut GatedSwarm, now_ms: u64) {
        let addresses: Vec<String> = self.schedule.keys().cloned().collect();
        let Some(client) = swarm.autonat_client_mut() else {
            return;
        };
        for address in addresses {
            if let Ok(multiaddr) = address.parse::<Multiaddr>()
                && client.inner_mut().retest(&multiaddr)
            {
                self.retests[RetestReason::Retry as usize] += 1;
            }
            if let Some(entry) = self.schedule.get_mut(&address) {
                entry.last_activity_ms = now_ms;
                entry.failures = 0;
                entry.due = None;
            }
        }
    }
}

/// Map the crate's outcome onto the manager's vocabulary.
///
/// `UnsupportedProtocol` and `Io` never arrive as an `Event` -- the
/// crate resets the candidate and returns without emitting (ADR-0051)
/// -- so they are `None` here rather than an outcome, and a version that
/// began emitting them would be counted, not miscounted.
fn outcome_of<E: std::fmt::Display>(result: &Result<(), E>) -> Option<ProbeOutcome> {
    match result {
        Ok(()) => Some(ProbeOutcome::Reachable),
        Err(e) if e.to_string().starts_with(ADDRESS_NOT_REACHABLE_PREFIX) => {
            Some(ProbeOutcome::Unreachable)
        }
        Err(_) => None,
    }
}

/// Whether the driver consumed the event.
pub(super) enum AutonatHandled {
    /// Handled here; nothing further sees it.
    Consumed,
    /// Peeked or ignored; the rest of the pipeline sees it.
    Passed(Box<Libp2pSwarmEvent<SubstrateBehaviourEvent>>),
}

/// The Swarm event path: outcomes are consumed, Identify and connection
/// events are peeked.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_autonat(
    event: Libp2pSwarmEvent<SubstrateBehaviourEvent>,
    swarm: &mut GatedSwarm,
    state: &mut AutonatState,
    manager: &ConnectionManager,
    open: &HashMap<ConnectionId, OpenConnection>,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) -> AutonatHandled {
    match &event {
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::AutonatClient(client::Event {
            tested_addr,
            server,
            result,
            ..
        })) => {
            let Some(outcome) = outcome_of(result) else {
                return AutonatHandled::Consumed;
            };
            let Ok(server) = TransportIdentity::parse(server.to_base58()) else {
                return AutonatHandled::Consumed;
            };
            let address = tested_addr.to_string();
            match state.record(&address, &server, outcome, now_ms) {
                Ok(Some(change)) => publish(state, swarm, &change, out),
                Ok(None) => {}
                Err(reason) => out.push(SwarmEvent::ReachabilityReportRefused {
                    server,
                    address,
                    reason,
                }),
            }
            AutonatHandled::Consumed
        }
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Identify(
            identify::Event::Received { peer_id, info, .. },
        )) => {
            if let Ok(peer) = TransportIdentity::parse(peer_id.to_base58()) {
                let _ = state.offer_server(&peer, &info.protocols, open);
                let _ = state.learn_server(
                    &peer,
                    manager.classify(&peer),
                    &info.protocols,
                    &info.listen_addrs,
                );
            }
            AutonatHandled::Passed(Box::new(event))
        }
        Libp2pSwarmEvent::ConnectionEstablished { peer_id, .. } => {
            settle_static(state, peer_id, true);
            AutonatHandled::Passed(Box::new(event))
        }
        Libp2pSwarmEvent::OutgoingConnectionError {
            peer_id: Some(peer_id),
            ..
        } => {
            settle_static(state, peer_id, false);
            AutonatHandled::Passed(Box::new(event))
        }
        _ => AutonatHandled::Passed(Box::new(event)),
    }
}

/// A static server's dial settled: connected resets the backoff, a
/// failure advances it.
fn settle_static(state: &mut AutonatState, peer_id: &PeerId, connected: bool) {
    let Ok(peer) = TransportIdentity::parse(peer_id.to_base58()) else {
        return;
    };
    if let Some(dial) = state.targets.get_mut(&peer) {
        dial.in_flight = false;
        if connected {
            dial.attempts = 0;
        }
    }
}

/// What one tick of the adapter reads from the runtime.
pub(super) struct AutonatTick<'a> {
    /// The gate's tickets, for the static-server dials this tick makes.
    pub(super) in_flight: &'a InFlightTickets,
    /// Every connection open right now, with its origin.
    pub(super) open: &'a HashMap<ConnectionId, OpenConnection>,
    /// Every address a listener has bound.
    pub(super) listeners: Vec<Multiaddr>,
    /// The runtime's clock.
    pub(super) now_ms: u64,
}

/// The tick: expire evidence, sync candidates, dial servers, re-test.
pub(super) fn reconcile(
    state: &mut AutonatState,
    swarm: &mut GatedSwarm,
    manager: &mut ConnectionManager,
    tick: AutonatTick<'_>,
    out: &mut Vec<SwarmEvent>,
) {
    let AutonatTick {
        in_flight,
        open,
        listeners,
        now_ms,
    } = tick;
    // A NETWORK CHANGE IS A CHANGE IN WHAT THIS PROFILE LISTENS ON, and
    // `AUTONAT.md` §5 sends it to `unknown`: every observation was about
    // addresses that may no longer exist. The listener set is the one
    // signal the runtime has for it; an interface change that leaves
    // the bound set intact is not seen here and is Phase 7's. AND THEN
    // EVERY CANDIDATE IS RE-TESTED, because `unknown` is where §5's
    // table starts, not where it ends: the crate's map still holds the
    // old candidates as tested, its tick would never sweep them again,
    // and an earlier version left a reachable profile `unknown` for
    // the rest of its life after one interface flap. Review finding on
    // PR #89. The wrapper's listener set starts over too, so the old
    // bound addresses stop being offered; its observed set is pruned by
    // age below rather than reset, since a peer's claim outlives the
    // interface it was made on.
    let mut now_listening: Vec<String> = listeners.iter().map(ToString::to_string).collect();
    now_listening.sort_unstable();
    now_listening.dedup();
    if !state.listeners.is_empty() && state.listeners != now_listening {
        if let Some(change) = state.manager.network_changed() {
            publish(state, swarm, &change, out);
        }
        if let Some(client) = swarm.autonat_client_mut() {
            client.reset_listeners();
        }
        state.retest_all(swarm, now_ms);
    }
    state.listeners = now_listening;

    // A SERVER THAT LOST ITS AUTHORIZATION TAKES ITS EVIDENCE WITH IT.
    // §3 counts authorized servers only; a trust change that dropped
    // one from both sets is applied here, on the tick, the same way the
    // gate re-evaluates the connections it holds.
    for server in state.manager.dial_order() {
        if manager.classify(&server) == interweave_transport_runtime::ConnectionClass::Unauthorized
            && let Some(change) = state.manager.remove_server(&server, now_ms)
        {
            publish(state, swarm, &change, out);
        }
    }
    // AND A LEARNED TARGET GOES WITH ITS AUTHORIZATION: the gate would
    // refuse every dial to it forever, once per backoff step, and its
    // slot in the learned set would be spent for good. A static one
    // stays -- it is configured, and the gate's refusal is the
    // operator's diagnostic that the configuration and the trust sets
    // disagree.
    state.targets.retain(|peer, target| {
        target.source == ServerSource::Static
            || manager.classify(peer) != interweave_transport_runtime::ConnectionClass::Unauthorized
    });

    // BOUND LISTENERS ARE CANDIDATES TOO (§6: "listener/address-registry
    // candidates"). The crate learns an address only through the
    // Swarm's candidate path, so a listener nobody has observed us on
    // is offered through the same door -- and the same wrapper, which
    // refuses a private or loopback one before the crate sees it.
    for address in listeners {
        if is_probeable_address(&address.to_string()) {
            swarm.offer_autonat_candidate(&address);
        }
    }
    // THE MANAGER COUNTS WHAT THE WRAPPER FORWARDED, and nothing else;
    // a peer's claim older than the evidence TTL leaves the wrapper's
    // set first, so a quota one peer spent is not spent forever.
    let ttl = state.settings.success_evidence_ttl_ms;
    let (candidates, truncated): (Vec<String>, usize) = swarm
        .autonat_client_mut()
        .map(|c| {
            c.prune_observed(now_ms, ttl);
            (c.candidates().map(str::to_owned).collect(), c.truncated())
        })
        .unwrap_or_default();
    if let Some(change) = state.manager.set_candidates(&candidates, now_ms) {
        publish(state, swarm, &change, out);
    }
    // A CANDIDATE THE WRAPPER REFUSED FOR ROOM IS SAID ONCE PER
    // SILENCE BOUND, not once per tick: a peer that keeps pushing
    // claims would otherwise turn the diagnostic into its own flood.
    if truncated > state.truncated_seen
        && now_ms.saturating_sub(state.last_truncation_report_ms) >= SILENCE_MS
    {
        state.last_truncation_report_ms = now_ms;
        out.push(SwarmEvent::ReachabilityCandidatesTruncated { total: truncated });
    }
    state.truncated_seen = truncated;
    // THE SCHEDULE FOLLOWS THE CANDIDATE SET: an address that left is
    // forgotten, one that arrived is seen now, so the silence bound
    // below counts from its first sighting.
    state.schedule.retain(|a, _| candidates.contains(a));
    for address in &candidates {
        state
            .schedule
            .entry(address.clone())
            .or_insert_with(|| Tracked::seen(now_ms));
    }
    if let Some(change) = state.manager.expire_evidence(now_ms) {
        publish(state, swarm, &change, out);
    }

    // SERVERS ARE DIALLED HERE, under the origin that names what the
    // connection is for. The reconnect scheduler cannot: it redials
    // under `ConnectionManager`, which the gate refuses toward an
    // infrastructure-only peer, so a server that dropped would never be
    // reached again. Every static one, and every learned one the knob
    // admitted. A peer this profile already holds an OUTBOUND
    // connection to needs none: that is the connection the client
    // probes over. An inbound alone does not count, for the same
    // reason a peer that dialled us is never a server.
    let peers: Vec<TransportIdentity> = state.targets.keys().cloned().collect();
    for peer in peers {
        let dialled = open.values().any(|c| c.peer == peer && c.origin.is_some());
        let Some(target) = state.targets.get_mut(&peer) else {
            continue;
        };
        if dialled || target.in_flight || now_ms < target.next_attempt_at_ms {
            continue;
        }
        // Every address in turn until one is ticketed, like the retry
        // scheduler; a refusal that settles the peer stops the walk.
        let mut last: Option<DialRefusal> = None;
        let mut ticketed = false;
        for address in target.addresses.clone() {
            match attempt_dial(
                swarm,
                manager,
                in_flight,
                &peer,
                &address,
                DialOrigin::AutonatProbe,
                now_ms,
            ) {
                Ok(()) => {
                    ticketed = true;
                    last = None;
                    break;
                }
                Err(refusal) => last = Some(refusal),
            }
        }
        // A TICKETED DIAL IS AN ATTEMPT; a refusal by the gate is not.
        // The gate's own quarantine refuses this driver's next try at
        // the address for the gate's backoff, and counting that refusal
        // here too would compound the two ladders. A refused peer is
        // asked again after the base delay, which is when the gate's
        // first quarantine lifts.
        if ticketed {
            target.in_flight = true;
            target.next_attempt_at_ms = now_ms.saturating_add(retry_backoff_ms(target.attempts));
            target.attempts = target.attempts.saturating_add(1);
        } else {
            target.next_attempt_at_ms = now_ms.saturating_add(RETRY_BASE_MS);
            if let Some(refusal) = last
                && !matches!(refusal, DialRefusal::Backend(_))
            {
                // REFUSED BY THE GATE -- backoff, quarantine, a class
                // that no longer authorizes it. Reported like a
                // scheduled retry's refusal is, because nobody asked
                // for this dial and so nobody else will see it fail.
                out.push(SwarmEvent::DialFailed {
                    peer: Some(peer.clone()),
                    detail: format!("autonat server: {refusal:?}"),
                });
            }
        }
    }

    // DUE RE-TESTS go back to the sweep; the crate issues them on its
    // next tick, at most five seconds away. AND SILENT CANDIDATES TOO:
    // one with nothing pending and no activity for `SILENCE_MS` is the
    // stuck `Pending` the crate never sweeps, so it is returned to the
    // sweep under `retry` -- a re-issue after a probe that reached no
    // outcome. The pending re-test is cleared when it fires; the
    // failure count stays, for the next outcome's backoff.
    let mut fire: Vec<(String, RetestReason)> = Vec::new();
    for (address, entry) in &state.schedule {
        match entry.due {
            Some((at, reason)) if now_ms >= at => fire.push((address.clone(), reason)),
            None if now_ms.saturating_sub(entry.last_activity_ms) >= SILENCE_MS => {
                fire.push((address.clone(), RetestReason::Retry));
            }
            _ => {}
        }
    }
    if let Some(client) = swarm.autonat_client_mut() {
        for (address, reason) in fire {
            let Some(entry) = state.schedule.get_mut(&address) else {
                continue;
            };
            entry.due = None;
            entry.last_activity_ms = now_ms;
            if let Ok(multiaddr) = address.parse::<Multiaddr>()
                && client.inner_mut().retest(&multiaddr)
            {
                state.retests[reason as usize] += 1;
            }
        }
    }
}

/// The verdict changed: advertise what is verified, withdraw what is
/// not, and tell the consumer.
fn publish(
    state: &mut AutonatState,
    swarm: &mut GatedSwarm,
    change: &ConnectivityChanged,
    out: &mut Vec<SwarmEvent>,
) {
    let verified: Vec<String> = change.to.verified_addresses().to_vec();
    for gone in state
        .advertised
        .iter()
        .filter(|a| !verified.contains(a))
        .cloned()
        .collect::<Vec<_>>()
    {
        if let Ok(addr) = gone.parse::<Multiaddr>() {
            swarm.remove_external_address(&addr);
        }
    }
    for fresh in verified.iter().filter(|a| !state.advertised.contains(a)) {
        if let Ok(addr) = fresh.parse::<Multiaddr>() {
            swarm.add_external_address(addr);
        }
    }
    state.advertised = verified.clone();
    out.push(SwarmEvent::ConnectivityChanged {
        direct_inbound: change.to.state(),
        verified_addresses: verified,
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use interweave_transport_api::DirectInboundState;

    const S1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const S2: &str = "12D3KooWHyNGMf9HTd3Zj6dStdkcc5ycsubW1rEgQSp6k6yfZBoy";

    /// A connection for these tests, holding a slot from a throwaway
    /// manager; the fields are `pub(super)` to the runtime, which this
    /// module is part of.
    fn connection(peer: TransportIdentity, origin: Option<DialOrigin>) -> OpenConnection {
        let mut manager =
            ConnectionManager::new(interweave_transport_runtime::ConnectionPolicy::new(8, 8), 8);
        let slot = manager.admit_inbound().expect("a fresh manager has a slot");
        OpenConnection {
            peer,
            slot,
            origin,
            admitted_class:
                interweave_transport_runtime::ConnectionClass::ConnectivityInfrastructureOnly,
        }
    }

    fn settings() -> AutonatClientSettings {
        AutonatClientSettings {
            static_servers: vec![StaticServer {
                peer: TransportIdentity::parse(S1).expect("valid"),
                address: format!("/ip4/8.8.8.8/tcp/4001/p2p/{S1}"),
            }],
            use_authorized_identify_servers: false,
            required_distinct_successes: 2,
            success_evidence_ttl_ms: 15 * 60 * 1000,
            refresh_interval_ms: 5 * 60 * 1000,
            max_candidate_addresses_per_cycle: 4,
        }
    }

    #[test]
    fn the_protocol_name_is_the_vendored_crates() {
        // The crate keeps its constant `pub(crate)`; this is the copy
        // Identify is matched against, so it is read back from the
        // vendored source rather than trusted.
        const V2: &str = include_str!("../../../../../third_party/libp2p-autonat/src/v2.rs");
        assert!(
            V2.contains(&format!("StreamProtocol::new(\"{DIAL_REQUEST_PROTOCOL}\")")),
            "the dial-request protocol string moved in the vendored crate"
        );
    }

    #[test]
    fn the_client_is_built_with_max_candidates_and_the_default_tick() {
        // The build site is the one place `with_probe_interval` could
        // be called from configuration; this reads the source of this
        // module and asserts it is not (`AUTONAT.md` §4, note of
        // 2026-09-17). A behavioural test cannot see the interval -- the
        // field is private to the crate -- so the call site is the
        // mechanism.
        const SELF: &str = include_str!("autonat_driver.rs");
        let build = SELF
            .split("pub fn build_behaviour")
            .nth(1)
            .and_then(|s| s.split("\n}\n").next())
            .expect("the build function");
        assert!(build.contains("with_max_candidates("));
        assert!(
            !build.contains("with_probe_interval("),
            "the tick is the crate's default, not a configured value"
        );
        // And the value is the configured one, not the crate's 10.
        let _ = build_behaviour(&settings());
    }

    #[test]
    fn from_profile_parses_the_peer_out_of_each_static_server() {
        let mut config = AutonatClientConfig {
            static_servers: vec![format!("/ip4/8.8.8.8/tcp/4001/p2p/{S1}")],
            ..AutonatClientConfig::default()
        };
        let settings = AutonatClientSettings::from_profile(&config).expect("valid");
        assert_eq!(settings.static_servers.len(), 1);
        assert_eq!(settings.static_servers[0].peer.as_str(), S1);
        assert_eq!(settings.refresh_interval_ms, 300_000);
        assert_eq!(settings.max_candidate_addresses_per_cycle, 4);

        config.static_servers = vec!["/ip4/8.8.8.8/tcp/4001".to_owned()];
        assert_eq!(
            AutonatClientSettings::from_profile(&config),
            Err("autonat static_servers: no /p2p/ component")
        );
        config.static_servers = vec!["not a multiaddr".to_owned()];
        assert_eq!(
            AutonatClientSettings::from_profile(&config),
            Err("autonat static_servers: not a multiaddr")
        );
    }

    #[test]
    fn validate_refuses_each_bound_at_its_edge_and_accepts_inside() {
        let ok = settings();
        assert_eq!(ok.validate(), Ok(()));
        let mut s = ok.clone();
        s.required_distinct_successes = 0;
        assert!(s.validate().is_err());
        s = ok.clone();
        s.required_distinct_successes = 5;
        assert!(s.validate().is_err());
        s = ok.clone();
        s.success_evidence_ttl_ms = 59_999;
        assert!(s.validate().is_err());
        s = ok.clone();
        s.refresh_interval_ms = 1_800_001;
        assert!(s.validate().is_err());
        s = ok.clone();
        s.max_candidate_addresses_per_cycle = 17;
        assert!(s.validate().is_err());
        s = ok.clone();
        s.max_candidate_addresses_per_cycle = 0;
        assert!(s.validate().is_err());
        s = ok;
        s.static_servers = (0..17).map(|_| s.static_servers[0].clone()).collect();
        assert!(s.validate().is_err());
    }

    #[test]
    fn a_server_is_offered_only_when_dialled_and_advertising_the_protocol() {
        let mut state = AutonatState::new(&settings()).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let dial_request = libp2p::StreamProtocol::new(DIAL_REQUEST_PROTOCOL);
        let other = libp2p::StreamProtocol::new("/ipfs/id/1.0.0");
        let mut open = HashMap::new();
        // S1 dialled by us; S2 dialled us.
        open.insert(
            ConnectionId::new_unchecked(1),
            connection(s1.clone(), Some(DialOrigin::AutonatProbe)),
        );
        open.insert(ConnectionId::new_unchecked(2), connection(s2.clone(), None));
        // Advertising, dialled: offered as Static (it is configured).
        assert!(state.offer_server(&s1, std::slice::from_ref(&dial_request), &open));
        assert!(state.is_server(&s1));
        assert_eq!(state.manager.dial_order(), std::slice::from_ref(&s1));
        // Advertising, but it dialled US: never a server.
        assert!(!state.offer_server(&s2, std::slice::from_ref(&dial_request), &open));
        assert!(!state.is_server(&s2));
        // Dialled, not advertising: not a server either.
        let mut state2 = AutonatState::new(&settings()).expect("builds");
        assert!(!state2.offer_server(&s1, &[other], &open));
        assert!(!state2.is_server(&s1));
        // Dialled, advertising, not configured: offered as Identify.
        open.insert(
            ConnectionId::new_unchecked(3),
            connection(s2.clone(), Some(DialOrigin::Manual)),
        );
        assert!(state.offer_server(&s2, &[dial_request], &open));
        assert!(state.is_server(&s2));
    }

    #[test]
    fn the_unreachable_prefix_is_the_vendored_crates_error_text() {
        const DIAL_REQUEST: &str = include_str!(
            "../../../../../third_party/libp2p-autonat/src/v2/client/handler/dial_request.rs"
        );
        assert!(
            DIAL_REQUEST.contains(&format!(
                "#[error(\"{ADDRESS_NOT_REACHABLE_PREFIX}: {{error}}\")]"
            )),
            "the AddressNotReachable error text moved in the vendored crate"
        );
        // And the other two arms return before emitting an event, which
        // is why an error that is not this one is not an outcome.
        const BEHAVIOUR: &str =
            include_str!("../../../../../third_party/libp2p-autonat/src/v2/client/behaviour.rs");
        for arm in [
            "Err(dial_request::Error::UnsupportedProtocol)",
            "Err(dial_request::Error::Io(",
        ] {
            let after = BEHAVIOUR.split(arm).nth(1).expect("the arm exists");
            let body = after
                .split("Err(dial_request::Error::")
                .next()
                .expect("body");
            assert!(
                body.contains("return;"),
                "{arm} no longer returns without emitting"
            );
        }
    }

    #[test]
    fn only_the_unreachable_error_is_an_outcome() {
        assert_eq!(outcome_of::<String>(&Ok(())), Some(ProbeOutcome::Reachable));
        assert_eq!(
            outcome_of(&Err(format!(
                "{ADDRESS_NOT_REACHABLE_PREFIX}: dial back failed"
            ))),
            Some(ProbeOutcome::Unreachable)
        );
        assert_eq!(outcome_of(&Err("IO error: timed out".to_owned())), None);
        assert_eq!(
            outcome_of(&Err(
                "Peer does not support AutoNAT dial-request protocol".to_owned()
            )),
            None
        );
    }
    /// A `GatedSwarm` with the client configured, for the seam tests:
    /// the event path and the verdict's effect on the Swarm.
    fn swarm_with_client(settings: &AutonatClientSettings) -> GatedSwarm {
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let manager =
            ConnectionManager::new(interweave_transport_runtime::ConnectionPolicy::default(), 8);
        let attribution = crate::attribution::DialAttribution::default();
        let outbound = crate::outbound_gate::OutboundAdmission::new(
            manager.handle(),
            InFlightTickets::default(),
            attribution,
            tokio::time::Instant::now(),
        );
        let class_policy = manager.handle();
        let autonat =
            libp2p::swarm::behaviour::toggle::Toggle::from(Some(build_behaviour(settings)));
        let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .expect("tcp")
            .with_behaviour(|key| {
                crate::behaviour::SubstrateBehaviour::new(
                    key,
                    interweave_transport_runtime::preauth::PreAuthLimits::default(),
                    outbound,
                    libp2p::swarm::behaviour::toggle::Toggle::from(None),
                    autonat,
                    class_policy,
                )
                .map_err(Box::<dyn std::error::Error + Send + Sync>::from)
            })
            .expect("behaviour")
            .build();
        GatedSwarm::new(swarm)
    }

    /// A connection manager that trusts nobody, for the event path:
    /// `handle_autonat` reads it only to classify a learned server.
    fn nobody() -> ConnectionManager {
        ConnectionManager::new(interweave_transport_runtime::ConnectionPolicy::default(), 8)
    }

    fn success(addr: &str, server: &str) -> Libp2pSwarmEvent<SubstrateBehaviourEvent> {
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::AutonatClient(client::Event {
            tested_addr: addr.parse().expect("a literal"),
            bytes_sent: 0,
            server: server.parse().expect("a peer id"),
            result: Ok(()),
        }))
    }

    #[tokio::test]
    async fn two_distinct_servers_verify_and_the_swarm_advertises_the_address_from_the_verdict() {
        // THE SEAM, over a real Swarm and a constructible event: the
        // crate's `Event` has public fields and `Ok(())` needs no
        // unnameable error, so a success can be pushed through the
        // same path a real probe's outcome takes. What this does not
        // prove is the wire -- the probe, the dial-back -- which
        // `tests/connectivity` covers as far as loopback allows.
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        assert!(state.manager.add_server(s1, ServerSource::Static));
        assert!(state.manager.add_server(s2, ServerSource::Identify));
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = state.manager.set_candidates([a], 0);
        let open = HashMap::new();
        let mut out = Vec::new();

        // One server: no verdict, nothing advertised, nothing emitted.
        let handled = handle_autonat(
            success(a, S1),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            1_000,
            &mut out,
        );
        assert!(matches!(handled, AutonatHandled::Consumed));
        assert!(out.is_empty());
        assert_eq!(swarm.external_addresses().count(), 0);
        assert_eq!(state.verdict().state(), DirectInboundState::Unknown);

        // A second, distinct server: verified, advertised, announced.
        let _ = handle_autonat(
            success(a, S2),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            2_000,
            &mut out,
        );
        assert_eq!(state.verdict().state(), DirectInboundState::VerifiedPublic);
        assert_eq!(
            swarm
                .external_addresses()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            [a.to_owned()]
        );
        assert!(matches!(
            out.as_slice(),
            [SwarmEvent::ConnectivityChanged {
                direct_inbound: DirectInboundState::VerifiedPublic,
                verified_addresses
            }] if verified_addresses == &[a.to_owned()]
        ));

        // And when the evidence lapses on a tick, the address is
        // WITHDRAWN -- the crate never does this (ADR-0051), so the
        // adapter must.
        out.clear();
        let manager_for_tick = &mut trusting(
            &TransportIdentity::parse(S1).expect("valid"),
            &TransportIdentity::parse(S2).expect("valid"),
        );
        reconcile(
            &mut state,
            &mut swarm,
            manager_for_tick,
            AutonatTick {
                in_flight: &InFlightTickets::default(),
                open: &open,
                // The candidate is still bound, so what withdraws the
                // address is the TTL and not the candidate leaving.
                listeners: vec![a.parse().expect("a literal")],
                now_ms: 2_000 + settings.success_evidence_ttl_ms + 1,
            },
            &mut out,
        );
        assert_eq!(state.verdict().state(), DirectInboundState::Unknown);
        assert_eq!(swarm.external_addresses().count(), 0);
        assert!(out.iter().any(|e| matches!(
            e,
            SwarmEvent::ConnectivityChanged {
                direct_inbound: DirectInboundState::Unknown,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn a_report_from_a_stranger_is_refused_by_name_and_advertises_nothing() {
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = state.manager.set_candidates([a], 0);
        let open = HashMap::new();
        let mut out = Vec::new();
        let _ = handle_autonat(
            success(a, S1),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            1_000,
            &mut out,
        );
        assert_eq!(swarm.external_addresses().count(), 0);
        assert!(matches!(
            out.as_slice(),
            [SwarmEvent::ReachabilityReportRefused {
                reason: RefusedReport::UnknownServer,
                ..
            }]
        ));
        assert_eq!(state.refused_unknown_server(), 1);
    }
    #[test]
    fn an_identify_learned_server_is_a_dial_target_only_under_the_knob_and_only_when_authorized() {
        use interweave_transport_runtime::ConnectionClass;
        let dial_request = libp2p::StreamProtocol::new(DIAL_REQUEST_PROTOCOL);
        let other = libp2p::StreamProtocol::new("/ipfs/id/1.0.0");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let addr: Multiaddr = "/ip4/8.8.4.4/tcp/4001".parse().expect("a literal");

        // Knob off: never, whatever the peer advertises.
        let mut off = AutonatState::new(&settings()).expect("builds");
        assert!(!off.learn_server(
            &s2,
            ConnectionClass::ConnectivityInfrastructureOnly,
            std::slice::from_ref(&dial_request),
            std::slice::from_ref(&addr),
        ));
        assert!(!off.targets.contains_key(&s2));

        // Knob on: an authorized, advertising peer with an address.
        let on = AutonatClientSettings {
            use_authorized_identify_servers: true,
            ..settings()
        };
        let mut state = AutonatState::new(&on).expect("builds");
        // Unauthorized: refused.
        assert!(!state.learn_server(
            &s2,
            ConnectionClass::Unauthorized,
            std::slice::from_ref(&dial_request),
            std::slice::from_ref(&addr),
        ));
        // Authorized but not advertising: refused.
        assert!(!state.learn_server(
            &s2,
            ConnectionClass::DataPlaneTrusted,
            std::slice::from_ref(&other),
            std::slice::from_ref(&addr),
        ));
        // Advertising, authorized, no address: refused.
        assert!(!state.learn_server(
            &s2,
            ConnectionClass::DataPlaneTrusted,
            std::slice::from_ref(&dial_request),
            &[],
        ));
        // The control: learned, as an Identify target.
        assert!(state.learn_server(
            &s2,
            ConnectionClass::DataPlaneTrusted,
            std::slice::from_ref(&dial_request),
            std::slice::from_ref(&addr),
        ));
        assert_eq!(
            state.targets.get(&s2).map(|t| t.source),
            Some(ServerSource::Identify)
        );
        // A static server is never re-learned as Identify.
        let s1 = TransportIdentity::parse(S1).expect("valid");
        assert!(!state.learn_server(
            &s1,
            ConnectionClass::ConnectivityInfrastructureOnly,
            std::slice::from_ref(&dial_request),
            std::slice::from_ref(&addr),
        ));
        assert_eq!(
            state.targets.get(&s1).map(|t| t.source),
            Some(ServerSource::Static)
        );
        // And the learned set is bounded at the static ceiling.
        for i in 0..MAX_LEARNED_SERVERS + 2 {
            let tail = format!("{i:044}").replace('0', "a");
            let peer = TransportIdentity::parse(format!("Qm{}", &tail[..44])).expect("valid");
            let _ = state.learn_server(
                &peer,
                ConnectionClass::DataPlaneTrusted,
                std::slice::from_ref(&dial_request),
                std::slice::from_ref(&addr),
            );
        }
        let learned = state
            .targets
            .values()
            .filter(|t| t.source == ServerSource::Identify)
            .count();
        assert_eq!(learned, MAX_LEARNED_SERVERS);
    }
    /// One tick of the adapter over `swarm`, with `a` as the bound
    /// listener and `manager` deciding classes.
    fn tick(
        state: &mut AutonatState,
        swarm: &mut GatedSwarm,
        manager: &mut ConnectionManager,
        listener: &str,
        now_ms: u64,
    ) -> Vec<SwarmEvent> {
        let mut out = Vec::new();
        reconcile(
            state,
            swarm,
            manager,
            AutonatTick {
                in_flight: &InFlightTickets::default(),
                open: &HashMap::new(),
                listeners: vec![listener.parse().expect("a literal")],
                now_ms,
            },
            &mut out,
        );
        out
    }

    /// Put `a` into the crate's map as TESTED, so a `retest` answers
    /// `true` and is counted -- the state a candidate is in after a
    /// probe.
    fn tested(swarm: &mut GatedSwarm, a: &str) {
        let addr: Multiaddr = a.parse().expect("a literal");
        swarm
            .autonat_client_mut()
            .expect("client")
            .inner_mut()
            .validate_addr(&addr);
    }

    /// A manager that holds S1 and S2 as infrastructure.
    fn trusting(s1: &TransportIdentity, s2: &TransportIdentity) -> ConnectionManager {
        let mut m =
            ConnectionManager::new(interweave_transport_runtime::ConnectionPolicy::default(), 8);
        let _ = m.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new(std::iter::empty()).expect("empty"),
                interweave_trust_api::InfrastructureSet::new([s1.clone(), s2.clone()])
                    .expect("two"),
            ),
            &[],
        );
        m
    }

    #[tokio::test]
    async fn a_reported_failure_backs_off_from_30s_and_doubles_across_the_ticks_that_fire_it() {
        // THE SHAPE PRODUCTION HOLDS: a failure, the tick that fires the
        // re-test, the next failure. An earlier test called `record` six
        // times with no tick between and passed while production
        // restarted at 30 s every time. Review finding on PR #89.
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 0);
        tested(&mut swarm, a);
        let mut now = 1_000;
        for expected in [30_000, 60_000, 120_000, 240_000, 300_000, 300_000] {
            assert!(state.record(a, &s1, ProbeOutcome::Unreachable, now).is_ok());
            let due = state.schedule.get(a).expect("tracked").due;
            assert_eq!(due, Some((now + expected, RetestReason::Retry)));
            // The tick that fires it -- and only when it is due.
            let _ = tick(&mut state, &mut swarm, &mut manager, a, now + expected - 1);
            assert!(state.schedule.get(a).expect("tracked").due.is_some());
            let _ = tick(&mut state, &mut swarm, &mut manager, a, now + expected);
            assert!(state.schedule.get(a).expect("tracked").due.is_none());
            now += expected;
            tested(&mut swarm, a);
        }
        assert_eq!(state.retests(RetestReason::Retry), 6);
        // A success resets the ladder; the next failure starts at 30 s.
        let _ = state.record(a, &s1, ProbeOutcome::Reachable, now);
        assert_eq!(state.schedule.get(a).expect("tracked").failures, 0);
        let _ = state.record(a, &s1, ProbeOutcome::Unreachable, now);
        assert_eq!(
            state.schedule.get(a).expect("tracked").due,
            Some((now + RETRY_BASE_MS, RetestReason::Retry))
        );
    }

    #[tokio::test]
    async fn a_candidate_with_no_outcome_is_retested_after_the_silence_bound() {
        // ADR-0051's "the caller's own timeout". A candidate the crate
        // left `Pending` forever -- no event -- goes back to the sweep
        // after SILENCE_MS, once, and again after another silence.
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 0);
        tested(&mut swarm, a);
        assert_eq!(state.schedule.get(a).expect("seen").last_activity_ms, 0);
        let _ = tick(&mut state, &mut swarm, &mut manager, a, SILENCE_MS - 1);
        assert_eq!(
            state.retests(RetestReason::Retry),
            0,
            "not yet silent enough"
        );
        let _ = tick(&mut state, &mut swarm, &mut manager, a, SILENCE_MS);
        assert_eq!(state.retests(RetestReason::Retry), 1);
        assert_eq!(
            state.schedule.get(a).expect("seen").last_activity_ms,
            SILENCE_MS
        );
        // Not again on the next tick -- the silence counts from the
        // re-test -- and again once it has been silent that long.
        tested(&mut swarm, a);
        let _ = tick(&mut state, &mut swarm, &mut manager, a, SILENCE_MS + 1_000);
        assert_eq!(state.retests(RetestReason::Retry), 1);
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 2 * SILENCE_MS);
        assert_eq!(state.retests(RetestReason::Retry), 2);
        // A CONTROL: an outcome is activity, so a candidate that
        // answered is not re-tested for silence.
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        let _ = state.record(a, &s1, ProbeOutcome::Reachable, 2 * SILENCE_MS + 1);
        tested(&mut swarm, a);
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 3 * SILENCE_MS);
        assert_eq!(state.retests(RetestReason::Retry), 2);
    }

    #[tokio::test]
    async fn a_second_observer_is_asked_for_only_when_enough_servers_are_known() {
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 0);
        // One server known, threshold two: a success waits the refresh
        // interval rather than asking every 30 s for an observer that
        // does not exist.
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        let _ = state.record(a, &s1, ProbeOutcome::Reachable, 1_000);
        assert_eq!(
            state.schedule.get(a).expect("tracked").due,
            Some((1_000 + 300_000, RetestReason::Refresh))
        );
        // Two known: the second observer is asked for.
        assert!(state.manager.add_server(s2.clone(), ServerSource::Identify));
        let _ = state.record(a, &s1, ProbeOutcome::Reachable, 2_000);
        assert_eq!(
            state.schedule.get(a).expect("tracked").due,
            Some((2_000 + RETRY_BASE_MS, RetestReason::SecondObserver))
        );
        // And once verified, refresh.
        let _ = state.record(a, &s2, ProbeOutcome::Reachable, 3_000);
        assert_eq!(state.verdict().state(), DirectInboundState::VerifiedPublic);
        assert_eq!(
            state.schedule.get(a).expect("tracked").due,
            Some((3_000 + 300_000, RetestReason::Refresh))
        );
    }

    #[tokio::test]
    async fn a_refused_report_is_counted_by_reason_and_schedules_nothing() {
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 0);
        assert_eq!(
            state.record(a, &s2, ProbeOutcome::Reachable, 0),
            Err(RefusedReport::UnknownServer)
        );
        assert_eq!(
            state.record("/ip4/9.9.9.9/tcp/4001", &s1, ProbeOutcome::Reachable, 0),
            Err(RefusedReport::UntrackedAddress)
        );
        assert_eq!(state.refused_unknown_server(), 1);
        assert_eq!(state.refused_untracked_address(), 1);
        assert!(state.schedule.get(a).expect("seen").due.is_none());
        assert_eq!(
            state.schedule.len(),
            1,
            "the untracked address is not tracked"
        );
        // The control: an accepted report schedules.
        assert!(matches!(
            state.record(a, &s1, ProbeOutcome::Reachable, 0),
            Ok(None)
        ));
        assert!(state.schedule.get(a).expect("tracked").due.is_some());
    }

    #[tokio::test]
    async fn a_changed_listener_set_forgets_every_observation_and_retests_every_candidate() {
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        assert!(state.manager.add_server(s2.clone(), ServerSource::Identify));
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 0);
        tested(&mut swarm, a);
        let open = HashMap::new();
        let mut out = Vec::new();
        let _ = handle_autonat(
            success(a, S1),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            1_000,
            &mut out,
        );
        let _ = handle_autonat(
            success(a, S2),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            2_000,
            &mut out,
        );
        assert_eq!(state.verdict().state(), DirectInboundState::VerifiedPublic);
        // Same listener: nothing moves (the control).
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 3_000);
        assert_eq!(state.verdict().state(), DirectInboundState::VerifiedPublic);
        assert_eq!(state.retests(RetestReason::Retry), 0);
        // A different listener set: unknown, address withdrawn, and the
        // surviving candidate RE-TESTED rather than left for dead -- an
        // earlier version stopped at unknown. The old listener is no
        // longer offered, so the candidate set is the new one.
        let b = "/ip4/8.8.4.4/tcp/4001";
        let events = tick(&mut state, &mut swarm, &mut manager, b, 4_000);
        assert_eq!(state.verdict().state(), DirectInboundState::Unknown);
        assert_eq!(swarm.external_addresses().count(), 0);
        assert!(events.iter().any(|e| matches!(
            e,
            SwarmEvent::ConnectivityChanged {
                direct_inbound: DirectInboundState::Unknown,
                ..
            }
        )));
        assert_eq!(
            state.retests(RetestReason::Retry),
            1,
            "the old candidate was re-tested"
        );
        assert_eq!(state.manager.candidates(), [b.to_owned()]);
        // And the new candidate verifies on fresh evidence.
        let _ = handle_autonat(
            success(b, S1),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            5_000,
            &mut out,
        );
        let _ = handle_autonat(
            success(b, S2),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            6_000,
            &mut out,
        );
        assert_eq!(state.verdict().state(), DirectInboundState::VerifiedPublic);
        assert_eq!(
            swarm
                .external_addresses()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            [b.to_owned()]
        );
    }

    #[tokio::test]
    async fn a_deauthorised_server_loses_its_evidence_and_a_learned_target_its_slot() {
        let on = AutonatClientSettings {
            use_authorized_identify_servers: true,
            ..settings()
        };
        let mut swarm = swarm_with_client(&on);
        let mut state = AutonatState::new(&on).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let dial_request = libp2p::StreamProtocol::new(DIAL_REQUEST_PROTOCOL);
        let addr: Multiaddr = "/ip4/8.8.4.4/tcp/4001".parse().expect("a literal");
        // S2 learned as a target while authorized.
        assert!(state.learn_server(
            &s2,
            interweave_transport_runtime::ConnectionClass::DataPlaneTrusted,
            std::slice::from_ref(&dial_request),
            std::slice::from_ref(&addr),
        ));
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        assert!(state.manager.add_server(s2.clone(), ServerSource::Identify));
        let a = "/ip4/8.8.8.8/tcp/4001";
        let mut manager = trusting(&s1, &s2);
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 0);
        let open = HashMap::new();
        let mut out = Vec::new();
        let _ = handle_autonat(
            success(a, S1),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            1_000,
            &mut out,
        );
        let _ = handle_autonat(
            success(a, S2),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            2_000,
            &mut out,
        );
        assert_eq!(state.verdict().state(), DirectInboundState::VerifiedPublic);
        assert!(state.targets.contains_key(&s2));
        // A tick under a manager that trusts nobody: both servers lose
        // their evidence, the learned target its slot, the static one
        // stays configured.
        let _ = tick(&mut state, &mut swarm, &mut nobody(), a, 3_000);
        assert_eq!(state.verdict().state(), DirectInboundState::Unknown);
        assert!(!state.is_server(&s1) && !state.is_server(&s2));
        assert!(
            !state.targets.contains_key(&s2),
            "the learned target is gone"
        );
        assert!(state.targets.contains_key(&s1), "the static target stays");
        assert_eq!(swarm.external_addresses().count(), 0);
    }

    #[test]
    fn a_learned_target_takes_fresh_addresses_capped_and_a_static_one_keeps_its_route() {
        use interweave_transport_runtime::ConnectionClass;
        let on = AutonatClientSettings {
            use_authorized_identify_servers: true,
            ..settings()
        };
        let mut state = AutonatState::new(&on).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let dial_request = libp2p::StreamProtocol::new(DIAL_REQUEST_PROTOCOL);
        let many: Vec<Multiaddr> = (1..=MAX_TARGET_ADDRESSES + 3)
            .map(|i| {
                format!("/ip4/8.8.8.{i}/tcp/4001")
                    .parse()
                    .expect("a literal")
            })
            .collect();
        assert!(state.learn_server(
            &s2,
            ConnectionClass::DataPlaneTrusted,
            std::slice::from_ref(&dial_request),
            &many,
        ));
        assert_eq!(
            state.targets.get(&s2).expect("learned").addresses.len(),
            MAX_TARGET_ADDRESSES
        );
        // Fresh Identify: the addresses are replaced, not frozen.
        let fresh: Multiaddr = "/ip4/9.9.9.9/tcp/4001".parse().expect("a literal");
        assert!(!state.learn_server(
            &s2,
            ConnectionClass::DataPlaneTrusted,
            std::slice::from_ref(&dial_request),
            std::slice::from_ref(&fresh),
        ));
        assert_eq!(
            state.targets.get(&s2).expect("learned").addresses,
            [fresh.to_string()]
        );
        // A static target keeps its configured route whatever Identify
        // says.
        assert!(!state.learn_server(
            &s1,
            ConnectionClass::ConnectivityInfrastructureOnly,
            std::slice::from_ref(&dial_request),
            std::slice::from_ref(&fresh),
        ));
        assert_eq!(
            state.targets.get(&s1).expect("static").addresses,
            [format!("/ip4/8.8.8.8/tcp/4001/p2p/{S1}")]
        );
    }

    #[test]
    fn two_static_entries_for_one_peer_are_two_routes_to_one_target() {
        let mut two = settings();
        two.static_servers.push(StaticServer {
            peer: TransportIdentity::parse(S1).expect("valid"),
            address: format!("/ip6/2001:4860:4860::8888/tcp/4001/p2p/{S1}"),
        });
        let state = AutonatState::new(&two).expect("builds");
        assert_eq!(state.targets.len(), 1);
        assert_eq!(
            state.targets.values().next().expect("one").addresses.len(),
            2
        );
    }

    #[tokio::test]
    async fn a_gate_refusal_is_reported_and_is_not_an_attempt() {
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        // The manager trusts nobody, so the static server's dial is
        // refused by policy: reported, not counted as an attempt, and
        // asked again after the base delay.
        let events = tick(
            &mut state,
            &mut swarm,
            &mut nobody(),
            "/ip4/8.8.8.8/tcp/4001",
            0,
        );
        assert!(events.iter().any(|e| matches!(
            e,
            SwarmEvent::DialFailed { peer: Some(p), .. } if *p == s1
        )));
        let target = state.targets.get(&s1).expect("static");
        assert_eq!(target.attempts, 0);
        assert!(!target.in_flight);
        assert_eq!(target.next_attempt_at_ms, RETRY_BASE_MS);
    }
}
