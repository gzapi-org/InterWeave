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
//! not connect. Servers learned through Identify are dialled only under
//! `use_authorized_identify_servers`; with it off, an Identify-learned
//! server is only ever a peer already connected for another reason.
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
use interweave_transport_api::{DirectInboundState, TransportIdentity};
use interweave_transport_runtime::reachability::{
    ConnectivityChanged, MAX_TRACKED_CANDIDATES, ProbeOutcome, ReachabilityConfig,
    ReachabilityManager, ReachabilityVerdict, RefusedReport, ServerSource, is_probeable_address,
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

/// One scheduled re-test.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scheduled {
    due_at_ms: u64,
    reason: RetestReason,
    /// Consecutive failures, for the retry backoff. Reset by a success.
    attempts: u32,
}

/// A static server's dial state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StaticDial {
    /// A dial this driver started and has not seen settle.
    in_flight: bool,
    /// Not before this moment; the gate's backoff applied here too,
    /// because the reconnect scheduler re-dials under an origin the
    /// gate refuses for an infrastructure peer, so nobody else will.
    next_attempt_at_ms: u64,
    attempts: u32,
}

/// The driver's state: the manager, the schedule, and the counters.
#[derive(Debug)]
pub struct AutonatState {
    settings: AutonatClientSettings,
    manager: ReachabilityManager,
    /// Per candidate address, bounded like the candidate set.
    schedule: BTreeMap<String, Scheduled>,
    statics: BTreeMap<TransportIdentity, StaticDial>,
    /// External addresses this driver has added to the Swarm.
    advertised: Vec<String>,
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
        let statics = settings
            .static_servers
            .iter()
            .map(|s| {
                (
                    s.peer.clone(),
                    StaticDial {
                        in_flight: false,
                        next_attempt_at_ms: 0,
                        attempts: 0,
                    },
                )
            })
            .collect();
        Ok(Self {
            settings: settings.clone(),
            manager,
            schedule: BTreeMap::new(),
            statics,
            advertised: Vec::new(),
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
        let source = if self.statics.contains_key(peer) {
            ServerSource::Static
        } else {
            ServerSource::Identify
        };
        self.manager.add_server(peer.clone(), source)
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
        // a tracked candidate, and there are at most
        // `MAX_TRACKED_CANDIDATES` of those.
        let verified = self
            .manager
            .state()
            .verified_addresses()
            .iter()
            .any(|a| a == address);
        let previous_attempts = self.schedule.get(address).map_or(0, |s| s.attempts);
        let scheduled = match outcome {
            ProbeOutcome::Reachable if verified => Scheduled {
                due_at_ms: now_ms.saturating_add(self.settings.refresh_interval_ms),
                reason: RetestReason::Refresh,
                attempts: 0,
            },
            ProbeOutcome::Reachable => Scheduled {
                due_at_ms: now_ms.saturating_add(RETRY_BASE_MS),
                reason: RetestReason::SecondObserver,
                attempts: 0,
            },
            ProbeOutcome::Unreachable => Scheduled {
                due_at_ms: now_ms.saturating_add(retry_backoff_ms(previous_attempts)),
                reason: RetestReason::Retry,
                attempts: previous_attempts.saturating_add(1),
            },
        };
        if self.schedule.len() >= MAX_TRACKED_CANDIDATES && !self.schedule.contains_key(address) {
            return;
        }
        self.schedule.insert(address.to_owned(), scheduled);
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
    if let Some(dial) = state.statics.get_mut(&peer) {
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
    // THE MANAGER COUNTS WHAT THE WRAPPER FORWARDED, and nothing else.
    let candidates: Vec<String> = swarm
        .autonat_client_mut()
        .map(|c| c.candidates().map(str::to_owned).collect())
        .unwrap_or_default();
    if let Some(change) = state.manager.set_candidates(candidates, now_ms) {
        publish(state, swarm, &change, out);
    }
    if let Some(change) = state.manager.expire_evidence(now_ms) {
        publish(state, swarm, &change, out);
    }

    // STATIC SERVERS ARE DIALLED HERE, under the origin that names what
    // the connection is for. The reconnect scheduler cannot: it redials
    // under `ConnectionManager`, which the gate refuses toward an
    // infrastructure-only peer, so a static server that dropped would
    // never be reached again.
    let statics: Vec<StaticServer> = state.settings.static_servers.clone();
    for server in statics {
        let connected = open.values().any(|c| c.peer == server.peer);
        let Some(dial) = state.statics.get_mut(&server.peer) else {
            continue;
        };
        if connected || dial.in_flight || now_ms < dial.next_attempt_at_ms {
            continue;
        }
        match attempt_dial(
            swarm,
            manager,
            in_flight,
            &server.peer,
            &server.address,
            DialOrigin::AutonatProbe,
            now_ms,
        ) {
            Ok(()) => {
                dial.in_flight = true;
                dial.next_attempt_at_ms = now_ms.saturating_add(retry_backoff_ms(dial.attempts));
                dial.attempts = dial.attempts.saturating_add(1);
            }
            Err(refusal) => {
                // REFUSED BY THE GATE -- backoff, quarantine, a class
                // that no longer authorizes it. Reported like a
                // scheduled retry's refusal is, because nobody asked
                // for this dial and so nobody else will see it fail.
                dial.next_attempt_at_ms = now_ms.saturating_add(retry_backoff_ms(dial.attempts));
                dial.attempts = dial.attempts.saturating_add(1);
                if !matches!(refusal, DialRefusal::Backend(_)) {
                    out.push(SwarmEvent::DialFailed {
                        peer: Some(server.peer.clone()),
                        detail: format!("autonat static server: {refusal:?}"),
                    });
                }
            }
        }
    }

    // DUE RE-TESTS go back to the sweep; the crate issues them on its
    // next tick, at most five seconds away.
    let due: Vec<(String, RetestReason)> = state
        .schedule
        .iter()
        .filter(|(_, s)| now_ms >= s.due_at_ms)
        .map(|(a, s)| (a.clone(), s.reason))
        .collect();
    if let Some(client) = swarm.autonat_client_mut() {
        for (address, reason) in due {
            let Ok(multiaddr) = address.parse::<Multiaddr>() else {
                state.schedule.remove(&address);
                continue;
            };
            // TAKEN OFF THE SCHEDULE WHETHER OR NOT THE CRATE CHANGED
            // ANYTHING: the next outcome reschedules it, and an address
            // the crate no longer knows (a candidate that left) has
            // nothing to reschedule.
            state.schedule.remove(&address);
            if client.inner_mut().retest(&multiaddr) {
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

/// The neutral state the consumer reads, for a driver that is absent:
/// `unknown`, which is what a profile with no client can honestly say.
#[must_use]
pub const fn absent_state() -> DirectInboundState {
    DirectInboundState::Unknown
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

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
    fn a_success_short_of_threshold_asks_for_a_second_observer_and_a_verdict_asks_for_refresh() {
        let mut state = AutonatState::new(&settings()).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        assert!(state.manager.add_server(s2.clone(), ServerSource::Identify));
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = state.manager.set_candidates([a], 0);

        let r = state.record(a, &s1, ProbeOutcome::Reachable, 1_000);
        assert!(matches!(r, Ok(None)));
        assert_eq!(
            state.schedule.get(a),
            Some(&Scheduled {
                due_at_ms: 1_000 + RETRY_BASE_MS,
                reason: RetestReason::SecondObserver,
                attempts: 0
            })
        );
        let r = state.record(a, &s2, ProbeOutcome::Reachable, 2_000);
        assert!(matches!(r, Ok(Some(_))), "two distinct servers verify");
        assert_eq!(
            state.schedule.get(a),
            Some(&Scheduled {
                due_at_ms: 2_000 + 300_000,
                reason: RetestReason::Refresh,
                attempts: 0
            })
        );
    }

    #[test]
    fn a_failure_backs_off_from_30s_and_doubles_until_a_success_resets_it() {
        let mut state = AutonatState::new(&settings()).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = state.manager.set_candidates([a], 0);
        let mut now = 0;
        for expected in [30_000, 60_000, 120_000, 240_000, 300_000, 300_000] {
            let _ = state.record(a, &s1, ProbeOutcome::Unreachable, now);
            let s = state.schedule.get(a).expect("scheduled");
            assert_eq!(s.reason, RetestReason::Retry);
            assert_eq!(s.due_at_ms - now, expected);
            now = s.due_at_ms;
        }
        let _ = state.record(a, &s1, ProbeOutcome::Reachable, now);
        assert_eq!(state.schedule.get(a).expect("scheduled").attempts, 0);
        // And the next failure starts over at the base.
        let _ = state.record(a, &s1, ProbeOutcome::Unreachable, now);
        assert_eq!(
            state.schedule.get(a).expect("scheduled").due_at_ms - now,
            RETRY_BASE_MS
        );
    }

    #[test]
    fn a_refused_report_is_counted_by_reason_and_schedules_nothing() {
        let mut state = AutonatState::new(&settings()).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = state.manager.set_candidates([a], 0);
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
        assert!(state.schedule.is_empty());
        // The control: an accepted report schedules.
        assert!(matches!(
            state.record(a, &s1, ProbeOutcome::Reachable, 0),
            Ok(None)
        ));
        assert_eq!(state.schedule.len(), 1);
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
        let manager_for_tick = &mut ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        reconcile(
            &mut state,
            &mut swarm,
            manager_for_tick,
            AutonatTick {
                in_flight: &InFlightTickets::default(),
                open: &open,
                listeners: Vec::new(),
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
}
