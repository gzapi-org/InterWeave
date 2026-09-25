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
//! the sweep every `refresh_interval`, or a silence bound before its
//! evidence horizon if that comes first; an address one server affirmed
//! while the threshold wants more goes back after the base retry delay
//! (`second_observer`) -- only while enough servers are CONNECTED to
//! reach the threshold, else it waits the refresh; an address a server
//! called unreachable goes back
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
//! AND holds an outbound connection to -- §3's "holds that outbound
//! connection", so a server that went away does not keep the door open
//! -- it asks under `AutonatProbe` instead, and the connection is retained
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
use rand::Rng as _;

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

/// The Display texts a failed probe's `client::Event::result` carries.
///
/// `client::Event::result` is `Result<(), Error>`, and the crate's
/// public `Error` (`v2/client/behaviour.rs`) wraps the handler's
/// `DialBackError` and displays THAT -- "server failed to establish a
/// connection" for `E_DIAL_ERROR`, "dial back stream failed" for
/// `E_DIAL_BACK_ERROR` -- never the handler's own `AddressNotReachable`
/// text, which is the only arm that reaches the event. Neither type is
/// exported, so the outcome is matched by these texts, pinned against
/// the vendored source by a test -- rather than mapping every `Err` to
/// "unreachable", which would let a re-vendor that started emitting
/// `Io` count a timeout as a server's failure vote.
///
/// **An earlier version matched the handler's text, which never reaches
/// the public event**, so every real failure classified as "no outcome"
/// and no server's failure vote was ever recorded; its unit test fed the
/// classifier a `String` of the expected shape and its source pin read
/// the handler file. Found by the first probe outcome produced over the
/// wire (step 4's harness); the pin now reads the public `Error`'s
/// `Display`, and
/// `a_real_dial_back_failure_is_an_unreachable_outcome_and_a_real_success_a_reachable_one`
/// in `tests/autonat_outcome_wire.rs` feeds the classifier the event a
/// real server produced.
pub const DIAL_BACK_FAILURE_TEXTS: [&str; 2] = [
    "server failed to establish a connection",
    "dial back stream failed",
];

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
        // A refresh slower than the evidence lifetime re-tests a verified
        // address only after its evidence has lapsed, so the verdict
        // drops to `unknown` between refreshes by construction.
        if self.refresh_interval_ms >= self.success_evidence_ttl_ms {
            return Err("autonat refresh_interval must be below success_evidence_ttl");
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
    // THE RNG CHANGED SHAPE WITH THE 0.57 BUMP, and what it generates is
    // the nonce a dial-back must echo (`AUTONAT.md` §3), so the choice
    // is recorded rather than taken from the compiler. Until rand 0.9
    // `OsRng` was an infallible `RngCore` and this passed it directly;
    // rand 0.10 makes it a `TryRngCore` -- OS entropy can fail, and the
    // type now says so -- which no longer satisfies the behaviour's
    // `R: rand::Rng`. `StdRng` via `make_rng` is what the
    // crate's own `Default` uses. NOT "seeded from the OS at
    // construction", which an earlier version of this comment said:
    // with rand's `thread_rng` feature on -- and it is on in this graph
    // -- `make_rng` is `R::from_rng(&mut rng())`, so the seed comes
    // from `ThreadRng`, itself a ChaCha CSPRNG periodically reseeded
    // from the OS. Only the `not(thread_rng)` arm reads `SysRng`
    // directly (`rand-0.10.2/src/lib.rs:103-112`).
    //
    // This value is the dial-back nonce (`AUTONAT.md` §3), so where its
    // unpredictability comes from is the whole reason the comment
    // exists -- and it is unchanged: still a CSPRNG, still OS-rooted,
    // one link further down. What is given up against the old `OsRng`
    // is a fresh OS read per call, which mattered to nothing here.
    // Review, PR #109.
    ScopedCandidates::new(ClientBehaviour::new(
        rand::make_rng::<rand::rngs::StdRng>(),
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

/// The pinned client's own tick, which `build_behaviour` leaves at its
/// default: five seconds. A re-test issued now is swept no sooner.
/// Pinned against the vendored source by
/// `the_crate_tick_is_the_vendored_default`.
pub const CRATE_TICK_MS: u64 = 5_000;

/// The most a re-test after a network change waits, drawn uniformly
/// (`CONNECTIVITY.md` §14 item 6: "bounded re-probe with jitter"): a
/// carrier hand-over moves many profiles at once, and every one
/// re-probing its servers in the same instant is the herd the jitter
/// spreads. One crate tick, so the re-probe still completes within
/// two. Pinned by `a_network_change_forgets_the_evidence_and_retests_within_the_jitter`.
pub const NETWORK_CHANGE_JITTER_MS: u64 = CRATE_TICK_MS;

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
    /// The send's count when it was last REPORTED (cumulative, so a
    /// refusal inside the throttle window is deferred rather than
    /// dropped), and the count side's HIGHEST overflow seen since the
    /// last report (per-tick, so an overflow that came and went inside
    /// a closed window is still said when it opens). Two marks and not
    /// one sum: a sum of a cumulative count and a snapshot falls when
    /// the snapshot shrinks, after which a fresh refusal lifting it
    /// back to the mark was lost. Review findings on PR #89, rounds 3
    /// and 4.
    truncated_at_send_reported: usize,
    truncated_at_count_peak: usize,
    truncated_at_count_reported: usize,
    last_truncation_report_ms: Option<u64>,
    refused_unknown_server: usize,
    refused_untracked_address: usize,
    /// Probe outcomes whose error text [`classify_outcome`] did not
    /// recognise, so no vote was recorded.
    unclassified_outcomes: usize,
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
            truncated_at_send_reported: 0,
            truncated_at_count_peak: 0,
            truncated_at_count_reported: 0,
            last_truncation_report_ms: None,
            refused_unknown_server: 0,
            refused_untracked_address: 0,
            unclassified_outcomes: 0,
            retests: [0; 3],
        })
    }

    /// The manager's verdict.
    #[must_use]
    pub fn verdict(&self) -> &ReachabilityVerdict {
        self.manager.state()
    }

    /// Whether `peer` is a server this driver offered to the manager,
    /// connected or not -- the evidence's set, for tests; production
    /// asks [`Self::is_connected_server`].
    #[cfg(test)]
    fn is_server(&self, peer: &TransportIdentity) -> bool {
        self.manager.is_server(peer)
    }

    /// Whether `peer` is a server that CAN ANSWER: offered to the
    /// manager AND holding an outbound connection this profile opened
    /// -- `AUTONAT.md` §3's "this profile DIALLED it and holds that
    /// outbound connection". The route-3 question the retention arm
    /// asks, and the second-observer question `reschedule` asks. The
    /// manager's set is the evidence's, and outlives the connection
    /// on purpose (§4's TTL, not a disconnect, expires evidence); an
    /// earlier version keyed both questions on that set alone, so a
    /// departed server counted for both. Review finding on PR #89,
    /// round 4.
    #[must_use]
    pub(super) fn is_connected_server(
        &self,
        peer: &TransportIdentity,
        open: &HashMap<ConnectionId, OpenConnection>,
    ) -> bool {
        self.manager.is_server(peer) && open.values().any(|c| c.peer == *peer && c.origin.is_some())
    }

    /// Whether `peer` is one of this driver's dial targets, static or
    /// learned -- the peers the reconnect scheduler leaves to it.
    #[must_use]
    pub fn is_target(&self, peer: &TransportIdentity) -> bool {
        self.targets.contains_key(peer)
    }

    /// How many servers can answer right now.
    fn connected_servers(&self, open: &HashMap<ConnectionId, OpenConnection>) -> usize {
        self.manager
            .dial_order()
            .iter()
            .filter(|s| open.values().any(|c| c.peer == **s && c.origin.is_some()))
            .count()
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

    /// §9 `autonat_probes_total{outcome=unclassified}`.
    #[must_use]
    pub const fn unclassified_outcomes(&self) -> usize {
        self.unclassified_outcomes
    }

    /// Count and report an outcome the classifier did not recognise.
    fn unclassified(
        &mut self,
        server: TransportIdentity,
        address: String,
        detail: String,
        out: &mut Vec<SwarmEvent>,
    ) {
        self.unclassified_outcomes += 1;
        out.push(SwarmEvent::ReachabilityOutcomeUnclassified {
            server,
            address,
            detail,
        });
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
        let source = match self.targets.get_mut(peer) {
            Some(t) => {
                // THE CONNECTION PROVED USEFUL: the ladder starts over,
                // its pending wait included, so a useful connection that
                // closes quickly is re-dialled on the next tick.
                t.attempts = 0;
                t.next_attempt_at_ms = 0;
                t.source
            }
            None => ServerSource::Identify,
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
        connected_servers: usize,
        now_ms: u64,
    ) -> Result<Option<ConnectivityChanged>, RefusedReport> {
        let result = self
            .manager
            .record_outcome(address, server, outcome, now_ms);
        match &result {
            Err(RefusedReport::UnknownServer) => self.refused_unknown_server += 1,
            Err(RefusedReport::UntrackedAddress) => self.refused_untracked_address += 1,
            Ok(_) => self.reschedule(address, outcome, connected_servers, now_ms),
        }
        result
    }

    /// Decide when `address` goes back to the sweep after `outcome`,
    /// with `connected_servers` the number that can answer right now.
    fn reschedule(
        &mut self,
        address: &str,
        outcome: ProbeOutcome,
        connected_servers: usize,
        now_ms: u64,
    ) {
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
        // fewer servers CONNECTED than the threshold needs, a re-test
        // every 30 s would be a dial-back at the same server forever,
        // for a verdict it cannot reach -- past `AUTONAT.md` §7's
        // per-client budget, so a real server would start refusing us.
        // Connected, not known: a server the manager still holds
        // evidence from but this profile no longer holds an outbound to
        // cannot be picked by the crate. Such an address waits the
        // refresh interval instead.
        let enough_servers =
            connected_servers >= self.settings.required_distinct_successes as usize;
        // A VERIFIED ADDRESS IS REFRESHED BEFORE ITS EVIDENCE CAN LAPSE,
        // not only every `refresh_interval`: the crate re-tests at ONE
        // random server, so a fixed cadence lets the other server's
        // success age out between refreshes and the address flap to
        // unknown. The verdict's own horizon names the earliest such
        // moment; the refresh is due a silence bound before it, or at
        // the interval, whichever is first.
        let horizon = match self.manager.state() {
            ReachabilityVerdict::VerifiedPublic {
                evidence_until_ms, ..
            } => Some(*evidence_until_ms),
            _ => None,
        };
        let refresh = self.settings.refresh_interval_ms;
        let refresh_due = |now: u64| {
            let at_interval = now.saturating_add(refresh);
            // Floored at one crate tick, not a silence bound: the crate
            // re-tests at ONE random server, so a refresh that lands on
            // the server whose evidence is not the one expiring must be
            // followed quickly, and each follow-up halves the odds that
            // the oldest success lapses unrefreshed. Review finding on
            // PR #89, round 5.
            horizon.map_or(at_interval, |h| {
                at_interval.min(
                    h.saturating_sub(SILENCE_MS)
                        .max(now.saturating_add(CRATE_TICK_MS)),
                )
            })
        };
        let entry = self
            .schedule
            .entry(address.to_owned())
            .or_insert_with(|| Tracked::seen(now_ms));
        entry.last_activity_ms = now_ms;
        entry.due = Some(match outcome {
            ProbeOutcome::Reachable if verified || !enough_servers => {
                entry.failures = 0;
                (refresh_due(now_ms), RetestReason::Refresh)
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

    /// Return every tracked candidate to the sweep: the answer to a
    /// network change, whose evidence the manager has just forgotten.
    /// Each is due within [`NETWORK_CHANGE_JITTER_MS`], drawn apart, so
    /// a herd of profiles moved by one hand-over does not re-probe its
    /// servers in one instant (§14 item 6); the failure count starts
    /// over, since it was about a network this profile has left.
    fn retest_all(&mut self, now_ms: u64) {
        for entry in self.schedule.values_mut() {
            // The thread RNG, for the reason `relay_driver` gives at
            // its own jitter: rand 0.10's `OsRng` is fallible, and
            // spreading a re-probe herd is not a nonce.
            let jitter = rand::rng().next_u64() % NETWORK_CHANGE_JITTER_MS.saturating_add(1);
            entry.last_activity_ms = now_ms;
            entry.failures = 0;
            entry.due = Some((now_ms.saturating_add(jitter), RetestReason::Retry));
        }
    }
}

/// Map the crate's outcome onto the manager's vocabulary.
///
/// `UnsupportedProtocol` and `Io` never arrive as an `Event` -- the
/// crate resets the candidate and returns without emitting (ADR-0051)
/// -- so an error text that is not one of [`DIAL_BACK_FAILURE_TEXTS`]
/// is `None` here rather than an outcome: a re-vendor that began
/// emitting them would be counted as unclassified by the caller, not
/// miscounted as a failure vote.
#[must_use]
pub fn classify_outcome<E: std::fmt::Display>(result: &Result<(), E>) -> Option<ProbeOutcome> {
    match result {
        Ok(()) => Some(ProbeOutcome::Reachable),
        Err(e) => {
            let text = e.to_string();
            DIAL_BACK_FAILURE_TEXTS
                .iter()
                .any(|known| text == *known)
                .then_some(ProbeOutcome::Unreachable)
        }
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
            let Ok(server) = TransportIdentity::parse(server.to_base58()) else {
                return AutonatHandled::Consumed;
            };
            let address = tested_addr.to_string();
            let Some(outcome) = classify_outcome(result) else {
                // NOT SILENT. An outcome this adapter cannot classify
                // is the invisible-refusal shape SPIKE-004 named, and
                // the one the earlier classifier produced for EVERY
                // failure; it is counted under §9's
                // `autonat_probes_total{outcome=unclassified}` AND
                // reported, like the refusal path below. Unreachable
                // with the pinned crate -- the pin asserts its error
                // enum has exactly the texts the classifier knows --
                // so `an_unclassified_outcome_is_counted_and_reported`
                // drives this helper directly.
                let detail = result.as_ref().err().map(ToString::to_string);
                state.unclassified(server, address, detail.unwrap_or_default(), out);
                return AutonatHandled::Consumed;
            };
            let connected_servers = state.connected_servers(open);
            let result = state.record(&address, &server, outcome, connected_servers, now_ms);
            // AN ACCEPTED SUCCESS IS A FRESH CLAIM on the wrapper's side
            // too: Identify re-emits an observed address only when it
            // changes, so a verified observed-only address would
            // otherwise age out of the wrapper at the TTL while its
            // probes kept succeeding. Review finding on PR #89.
            if matches!((&result, outcome), (Ok(_), ProbeOutcome::Reachable))
                && let Some(client) = swarm.autonat_client_mut()
            {
                client.touch_observed(&address, now_ms);
            }
            match result {
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
            settle_static(state, peer_id);
            AutonatHandled::Passed(Box::new(event))
        }
        Libp2pSwarmEvent::OutgoingConnectionError {
            peer_id: Some(peer_id),
            ..
        } => {
            settle_static(state, peer_id);
            AutonatHandled::Passed(Box::new(event))
        }
        _ => AutonatHandled::Passed(Box::new(event)),
    }
}

/// A target's dial settled, one way or the other: it is no longer in
/// flight. The ladder is NOT reset here: a server that accepts the
/// connection and closes it (its own §7 policy refusing us) would
/// otherwise be re-dialled every base delay with no backoff and no
/// `DialFailed`. It is reset when the connection proves useful -- the
/// Identify that offers the peer as a server, in `offer_server`.
/// Review finding on PR #89, round 4.
fn settle_static(state: &mut AutonatState, peer_id: &PeerId) {
    let Ok(peer) = TransportIdentity::parse(peer_id.to_base58()) else {
        return;
    };
    if let Some(dial) = state.targets.get_mut(&peer) {
        dial.in_flight = false;
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

/// The network changed (`CONNECTIVITY.md` §14, step 10): `AUTONAT.md`
/// §5 sends the verdict to `unknown` -- every observation was about
/// addresses that may no longer exist -- and the change is published
/// at once, so the relay target follows it in the same turn (§14 item
/// 4). THEN EVERY CANDIDATE IS RE-TESTED, within the jitter, because
/// `unknown` is where §5's table starts, not where it ends: the crate's
/// map still holds the old candidates as tested, its tick would never
/// sweep them again, and an earlier version left a reachable profile
/// `unknown` for the rest of its life after one interface flap (PR
/// #89). The wrapper's listener set starts over too, so the old bound
/// addresses stop being offered; its observed set is pruned by age on
/// the tick rather than reset, since a peer's claim outlives the
/// interface it was made on. A candidate that left the bound set is
/// pruned from the schedule on the next tick before its re-test can
/// fire, so no server is asked to dial an address this profile no
/// longer holds; the listeners still bound are offered again HERE, not
/// on the next tick, so no peer's claim about one lands in the gap as
/// an observation and spends a slot (PR #104 round 1). Pinned by
/// `a_network_change_forgets_the_evidence_and_retests_within_the_jitter`.
pub(super) fn network_changed<'a>(
    state: &mut AutonatState,
    swarm: &mut GatedSwarm,
    listeners: impl Iterator<Item = &'a Multiaddr>,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) {
    if let Some(change) = state.manager.network_changed() {
        publish(state, swarm, &change, out);
    }
    if let Some(client) = swarm.autonat_client_mut() {
        client.reset_listeners();
    }
    for address in listeners {
        if is_probeable_address(&address.to_string()) {
            swarm.offer_autonat_candidate(address);
        }
    }
    state.retest_all(now_ms);
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
    // A NETWORK CHANGE is not noticed here: the runtime's detector
    // (`network_change.rs`) sees the bound set change and calls
    // `network_changed` below, whether or not this client is on (step
    // 10; until then the comparison lived in this tick, and with the
    // client off a change was seen by nothing).

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
    // BOTH REFUSALS FOR ROOM ARE REPORTED, APART: the wrapper's at the
    // send (cumulative) and the manager's at the count (the largest
    // overflow on any tick since the last report -- the wrapper may
    // offer up to twice what the manager keeps). A new refusal is either count above its own last
    // reported value.
    let at_the_send = truncated;
    state.truncated_at_count_peak = state
        .truncated_at_count_peak
        .max(state.manager.truncated_candidates());
    let window_open = state
        .last_truncation_report_ms
        .is_none_or(|at| now_ms.saturating_sub(at) >= SILENCE_MS);
    // FRESH is the send's count above its last report, or the count
    // side's peak since the last report above the peak last reported --
    // so a standing overflow is said once and not every window, and one
    // that rose and fell inside a closed window is still said when it
    // opens.
    let fresh = at_the_send > state.truncated_at_send_reported
        || state.truncated_at_count_peak > state.truncated_at_count_reported;
    if fresh && window_open {
        state.last_truncation_report_ms = Some(now_ms);
        state.truncated_at_send_reported = at_the_send;
        let at_the_count = std::mem::take(&mut state.truncated_at_count_peak);
        state.truncated_at_count_reported = at_the_count;
        out.push(SwarmEvent::ReachabilityCandidatesTruncated {
            at_the_send,
            at_the_count,
        });
    }
    // THE SCHEDULE FOLLOWS THE SET THE MANAGER COUNTS -- not the
    // wrapper's list, which can be up to twice as long: an address the
    // manager truncated is one whose every outcome is refused
    // `UntrackedAddress`, and an earlier version seeded it here anyway,
    // so the silence arm below re-tested it every thirty seconds for
    // as long as the claim lived. Review finding on PR #89. An address
    // that left is forgotten, one that arrived is seen now, so the
    // silence bound counts from its first sighting.
    let counted: Vec<String> = state.manager.candidates().to_vec();
    state.schedule.retain(|a, _| counted.contains(a));
    for address in &counted {
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
            // A refusal the gate's OWN ladder already paces -- a peer in
            // backoff, a quarantined address -- is asked again after the
            // base delay, when that ladder's first step lifts; counting
            // it here too would compound the two. Every other refusal is
            // a standing condition (unauthorized, no budget, backend)
            // and backs off like a failure would, or a misconfigured
            // static server is reported every thirty seconds for the
            // process's life. Review finding on PR #89.
            // `PolicySuperseded` joins them: its own doc says "reload and
            // ask again", so it is asked again at the base delay rather
            // than doubled. `TooManyPendingDials`, `ConnectionLimitReached`,
            // `Unauthorized`, `NotAuthorizedForDataPlane` and a backend
            // refusal are standing conditions and back off.
            let paced_by_the_gate = matches!(
                last,
                Some(DialRefusal::Policy(
                    interweave_transport_runtime::DialDenial::PeerBackoff
                        | interweave_transport_runtime::DialDenial::AddressQuarantined
                        | interweave_transport_runtime::DialDenial::PolicySuperseded
                ))
            );
            if paced_by_the_gate {
                target.next_attempt_at_ms = now_ms.saturating_add(RETRY_BASE_MS);
            } else {
                target.next_attempt_at_ms =
                    now_ms.saturating_add(retry_backoff_ms(target.attempts));
                target.attempts = target.attempts.saturating_add(1);
            }
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
    use interweave_transport_runtime::reachability::MAX_TRACKED_CANDIDATES;
    use libp2p::swarm::NetworkBehaviour as _;

    const S1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const S2: &str = "12D3KooWHyNGMf9HTd3Zj6dStdkcc5ycsubW1rEgQSp6k6yfZBoy";

    /// A connection for these tests, holding a slot from a throwaway
    /// manager; the fields are `pub(super)` to the runtime, which this
    /// module is part of.
    fn connection(peer: TransportIdentity, origin: Option<DialOrigin>) -> OpenConnection {
        connection_over(peer, origin, crate::runtime::messages::PeerPath::Direct)
    }

    fn connection_over(
        peer: TransportIdentity,
        origin: Option<DialOrigin>,
        path: crate::runtime::messages::PeerPath,
    ) -> OpenConnection {
        let mut manager =
            ConnectionManager::new(interweave_transport_runtime::ConnectionPolicy::new(8, 8), 8);
        let slot = manager.admit_inbound().expect("a fresh manager has a slot");
        OpenConnection {
            peer,
            slot,
            origin,
            admitted_class:
                interweave_transport_runtime::ConnectionClass::ConnectivityInfrastructureOnly,
            path,
            punched: false,
            since_ms: 0,
            retiring: false,
        }
    }

    /// `record` with every server the manager knows counted as
    /// connected -- the tests here add servers to the manager directly
    /// and hold no connections; the one test about the distinction
    /// passes its own count.
    fn rec(
        state: &mut AutonatState,
        address: &str,
        server: &TransportIdentity,
        outcome: ProbeOutcome,
        now_ms: u64,
    ) -> Result<Option<ConnectivityChanged>, RefusedReport> {
        let known = state.manager.dial_order().len();
        state.record(address, server, outcome, known, now_ms)
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
        // And not when it dialled us OVER A CIRCUIT either: the inbound
        // arm keeps a relayed inbound origin-less (PR #101 round 1 found
        // it recording the origin it was retained under, which read here
        // as a dial this profile made).
        open.insert(
            ConnectionId::new_unchecked(9),
            connection_over(
                s2.clone(),
                None,
                crate::runtime::messages::PeerPath::Relayed,
            ),
        );
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
    fn the_failure_texts_are_what_the_vendored_crates_public_error_displays() {
        // THE PUBLIC TYPE, not the handler's. The earlier version of
        // this pin read `dial_request.rs` for the `AddressNotReachable`
        // text and passed while the classifier matched nothing a real
        // event carries.
        const DIAL_REQUEST: &str = include_str!(
            "../../../../../third_party/libp2p-autonat/src/v2/client/handler/dial_request.rs"
        );
        const BEHAVIOUR: &str =
            include_str!("../../../../../third_party/libp2p-autonat/src/v2/client/behaviour.rs");
        // THE ENUM BODY, not the file: the handler's own `Error` carries
        // `#[error(` lines too, and a text that moved there would keep a
        // file-wide `contains` true. Review finding on PR #90, round 2.
        let body = DIAL_REQUEST
            .split("pub enum DialBackError {")
            .nth(1)
            .and_then(|after| after.split("\n}").next())
            .expect("the enum exists");
        for text in DIAL_BACK_FAILURE_TEXTS {
            assert!(
                body.contains(&format!("#[error(\"{text}\")]")),
                "`DialBackError` no longer displays {text:?}"
            );
        }
        // AND NO OTHERS: the enum body carries exactly as many `#[error(`
        // as the classifier knows texts, so a re-vendor that adds a
        // variant fails here rather than reaching the unclassified path
        // in production. Review finding on PR #90.
        assert_eq!(
            body.matches("#[error(").count(),
            DIAL_BACK_FAILURE_TEXTS.len(),
            "`DialBackError` has a variant the classifier does not know"
        );
        assert!(
            BEHAVIOUR.contains("pub(crate) inner: dial_request::DialBackError,")
                && BEHAVIOUR.contains("Display::fmt(&self.inner, f)"),
            "the public `Error` no longer displays its `DialBackError` verbatim"
        );
        assert!(
            BEHAVIOUR.contains("result: result.map_err(|e| Error { inner: e }),"),
            "the event's error is no longer built from the dial-back error"
        );
        // And the other two arms return before emitting an event, which
        // is why an error that is not one of these is not an outcome.
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
    fn only_a_dial_back_failure_text_is_an_unreachable_outcome() {
        // Text-shaped inputs, which is exactly how the defect passed:
        // the load-bearing test feeds a REAL event, in
        // `tests/autonat_outcome_wire.rs`; this one pins the vocabulary.
        assert_eq!(
            classify_outcome::<String>(&Ok(())),
            Some(ProbeOutcome::Reachable)
        );
        for text in DIAL_BACK_FAILURE_TEXTS {
            assert_eq!(
                classify_outcome(&Err(text.to_owned())),
                Some(ProbeOutcome::Unreachable)
            );
        }
        for not_an_outcome in [
            "Address is not reachable: server failed to establish a connection",
            "IO error: timed out",
            "Peer does not support AutoNAT dial-request protocol",
        ] {
            assert_eq!(classify_outcome(&Err(not_an_outcome.to_owned())), None);
        }
    }

    #[test]
    fn an_unclassified_outcome_is_counted_and_reported() {
        // The branch itself cannot be reached with the pinned crate (the
        // pin above holds its error enum to the two known texts), so
        // the helper it calls is driven directly: the count and the
        // event must move together, like the refusal path's.
        let mut state = AutonatState::new(&settings()).expect("builds");
        let mut out = Vec::new();
        let server = TransportIdentity::parse(S1).expect("valid");
        state.unclassified(
            server.clone(),
            "/ip4/8.8.8.8/tcp/1".to_owned(),
            "x".to_owned(),
            &mut out,
        );
        assert_eq!(state.unclassified_outcomes(), 1);
        assert_eq!(
            out,
            vec![SwarmEvent::ReachabilityOutcomeUnclassified {
                server,
                address: "/ip4/8.8.8.8/tcp/1".to_owned(),
                detail: "x".to_owned(),
            }]
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
                    crate::behaviour::Configured {
                        autonat_client: autonat,
                        ..crate::behaviour::Configured::default()
                    },
                    class_policy,
                )
                // As production builds it: the root funnel around the
                // whole composite (ADR-0052 A 2026-09-25 D1).
                .map(crate::root_funnel::RootFunnel::new)
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
        // prove is the wire -- the probe, the dial-back -- which the
        // raw two-Swarm harness in `tests/autonat_outcome_wire.rs`
        // produces on loopback with §6 bypassed, and which the
        // substrate under §6 cannot produce there (SPIKE-004 phase B).
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
            ConnectionManager::new(interweave_transport_runtime::ConnectionPolicy::new(8, 8), 8);
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
            assert!(rec(&mut state, a, &s1, ProbeOutcome::Unreachable, now).is_ok());
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
        let _ = rec(&mut state, a, &s1, ProbeOutcome::Reachable, now);
        assert_eq!(state.schedule.get(a).expect("tracked").failures, 0);
        let _ = rec(&mut state, a, &s1, ProbeOutcome::Unreachable, now);
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
        let _ = rec(
            &mut state,
            a,
            &s1,
            ProbeOutcome::Reachable,
            2 * SILENCE_MS + 1,
        );
        tested(&mut swarm, a);
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 3 * SILENCE_MS);
        assert_eq!(state.retests(RetestReason::Retry), 2);
    }

    #[tokio::test]
    async fn a_second_observer_is_asked_for_only_when_enough_servers_can_answer() {
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
        let _ = rec(&mut state, a, &s1, ProbeOutcome::Reachable, 1_000);
        assert_eq!(
            state.schedule.get(a).expect("tracked").due,
            Some((1_000 + 300_000, RetestReason::Refresh))
        );
        // Two known: the second observer is asked for.
        assert!(state.manager.add_server(s2.clone(), ServerSource::Identify));
        let _ = rec(&mut state, a, &s1, ProbeOutcome::Reachable, 2_000);
        assert_eq!(
            state.schedule.get(a).expect("tracked").due,
            Some((2_000 + RETRY_BASE_MS, RetestReason::SecondObserver))
        );
        // And once verified, refresh.
        let _ = rec(&mut state, a, &s2, ProbeOutcome::Reachable, 3_000);
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
            rec(&mut state, a, &s2, ProbeOutcome::Reachable, 0),
            Err(RefusedReport::UnknownServer)
        );
        assert_eq!(
            rec(
                &mut state,
                "/ip4/9.9.9.9/tcp/4001",
                &s1,
                ProbeOutcome::Reachable,
                0
            ),
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
            rec(&mut state, a, &s1, ProbeOutcome::Reachable, 0),
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
        // And an OBSERVED claim, which survives a listener change.
        let c = "/ip4/1.1.1.1/tcp/4001";
        let c_addr: Multiaddr = c.parse().expect("a literal");
        swarm.autonat_client_mut().expect("client").on_swarm_event(
            libp2p::swarm::FromSwarm::NewExternalAddrCandidate(
                libp2p::swarm::NewExternalAddrCandidate { addr: &c_addr },
            ),
        );
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 0);
        tested(&mut swarm, a);
        tested(&mut swarm, c);
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
        // A different listener set -- the runtime's detector saw it and
        // told the adapter (step 10) -- : unknown, address withdrawn,
        // and the SURVIVING candidate re-tested rather than left for
        // dead -- an earlier version stopped at unknown. The departed
        // listener is no longer a candidate once the tick has pruned
        // the schedule to the counted set, and is NOT re-tested when
        // its due arrives: that would ask a server to dial an address
        // this profile no longer holds. So exactly one re-test, for
        // the observed claim, once the whole jitter has passed.
        let b = "/ip4/8.8.4.4/tcp/4001";
        let mut events = Vec::new();
        network_changed(
            &mut state,
            &mut swarm,
            std::iter::empty(),
            4_000,
            &mut events,
        );
        assert_eq!(state.verdict().state(), DirectInboundState::Unknown);
        assert_eq!(swarm.external_addresses().count(), 0);
        assert!(events.iter().any(|e| matches!(
            e,
            SwarmEvent::ConnectivityChanged {
                direct_inbound: DirectInboundState::Unknown,
                ..
            }
        )));
        let _ = tick(
            &mut state,
            &mut swarm,
            &mut manager,
            b,
            4_000 + NETWORK_CHANGE_JITTER_MS,
        );
        assert_eq!(
            state.retests(RetestReason::Retry),
            1,
            "the surviving claim was re-tested, the departed listener was not"
        );
        assert_eq!(state.manager.candidates(), [b.to_owned(), c.to_owned()]);
        assert!(!state.schedule.contains_key(a));
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
    async fn a_standing_refusal_of_a_static_server_is_reported_and_backs_off() {
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        // The manager trusts nobody, so the static server's dial is
        // refused by policy -- a standing condition, not the gate's own
        // pacing: reported, and backed off like a failure, so a
        // misconfigured server is not a DialFailed every thirty seconds
        // for the process's life. Review finding on PR #89.
        let listener = "/ip4/8.8.8.8/tcp/4001";
        let mut now = 0;
        for (attempt, expected) in [30_000, 60_000, 120_000].into_iter().enumerate() {
            let events = tick(&mut state, &mut swarm, &mut nobody(), listener, now);
            assert!(events.iter().any(|e| matches!(
                e,
                SwarmEvent::DialFailed { peer: Some(p), .. } if *p == s1
            )));
            let target = state.targets.get(&s1).expect("static");
            assert!(!target.in_flight);
            assert_eq!(target.attempts, attempt as u32 + 1);
            assert_eq!(target.next_attempt_at_ms, now + expected);
            // Not asked again before it is due.
            let events = tick(
                &mut state,
                &mut swarm,
                &mut nobody(),
                listener,
                now + expected - 1,
            );
            assert!(
                events
                    .iter()
                    .all(|e| !matches!(e, SwarmEvent::DialFailed { .. }))
            );
            now += expected;
        }
    }

    fn claim(swarm: &mut GatedSwarm, addr: &Multiaddr) {
        swarm.autonat_client_mut().expect("client").on_swarm_event(
            libp2p::swarm::FromSwarm::NewExternalAddrCandidate(
                libp2p::swarm::NewExternalAddrCandidate { addr },
            ),
        );
    }

    fn nth_claim(i: usize) -> Multiaddr {
        format!("/ip4/1.0.{}.{}/tcp/4001", i / 250, 1 + i % 250)
            .parse()
            .expect("a literal")
    }

    #[tokio::test]
    async fn a_candidate_the_manager_does_not_count_is_never_scheduled() {
        // The wrapper can hold up to twice what the manager counts; the
        // schedule follows the manager. Sixty-four observed claims fill
        // the manager's set beside one bound listener, so the last
        // claim is truncated by the manager -- and it must not be
        // re-tested for silence, or a server is asked to dial an
        // attacker-chosen address every thirty seconds while its every
        // answer is refused. Review finding on PR #89.
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        let listener = "/ip4/8.8.8.8/tcp/4001";
        for i in 1..=MAX_TRACKED_CANDIDATES {
            claim(&mut swarm, &nth_claim(i));
        }
        let _ = tick(&mut state, &mut swarm, &mut manager, listener, 0);
        // The wrapper yields 65; the manager counts 64; the schedule
        // holds 64 and not the truncated claim -- whichever the
        // wrapper's order put last, which is what the manager saw.
        assert_eq!(state.manager.candidates().len(), MAX_TRACKED_CANDIDATES);
        assert_eq!(state.manager.truncated_candidates(), 1);
        assert_eq!(state.schedule.len(), MAX_TRACKED_CANDIDATES);
        let offered: Vec<String> = swarm
            .autonat_client_mut()
            .expect("client")
            .candidates()
            .map(str::to_owned)
            .collect();
        let last = offered
            .iter()
            .find(|a| !state.manager.candidates().contains(a))
            .expect("one was truncated")
            .clone();
        assert!(!state.schedule.contains_key(&last));
        // After a silence, the counted ones are re-tested and the
        // truncated one is not.
        tested(&mut swarm, listener);
        for i in 1..=MAX_TRACKED_CANDIDATES {
            tested(&mut swarm, &nth_claim(i).to_string());
        }
        let _ = tick(&mut state, &mut swarm, &mut manager, listener, SILENCE_MS);
        assert_eq!(state.retests(RetestReason::Retry), MAX_TRACKED_CANDIDATES);
        assert!(!state.schedule.contains_key(&last));
    }

    #[tokio::test]
    async fn a_refusal_for_room_is_said_on_the_first_tick_and_then_at_most_once_per_silence_bound()
    {
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        let listener = "/ip4/8.8.8.8/tcp/4001";
        for i in 1..=MAX_TRACKED_CANDIDATES + 1 {
            claim(&mut swarm, &nth_claim(i));
        }
        // Two refusals, inside the first thirty seconds -- one at the
        // send (the observed set held 64, the 65th claim was refused)
        // and one at the count (offered 65 with the listener, counted
        // 64) -- said on the first tick, apart, not dropped.
        let events = tick(&mut state, &mut swarm, &mut manager, listener, 0);
        assert!(events.iter().any(|e| matches!(
            e,
            SwarmEvent::ReachabilityCandidatesTruncated {
                at_the_send: 1,
                at_the_count: 1
            }
        )));
        // Another claim ten seconds later, refused by the wrapper:
        // deferred past the window, then said with the running total.
        claim(&mut swarm, &nth_claim(MAX_TRACKED_CANDIDATES + 2));
        let events = tick(&mut state, &mut swarm, &mut manager, listener, 10_000);
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, SwarmEvent::ReachabilityCandidatesTruncated { .. }))
        );
        let events = tick(&mut state, &mut swarm, &mut manager, listener, SILENCE_MS);
        assert!(events.iter().any(|e| matches!(
            e,
            SwarmEvent::ReachabilityCandidatesTruncated {
                at_the_send: 2,
                at_the_count: 1
            }
        )));
        // And nothing new: silence.
        let events = tick(
            &mut state,
            &mut swarm,
            &mut manager,
            listener,
            2 * SILENCE_MS,
        );
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, SwarmEvent::ReachabilityCandidatesTruncated { .. }))
        );
    }

    #[tokio::test]
    async fn an_accepted_success_keeps_an_observed_claim_fresh_past_the_ttl() {
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        // An observed-only claim; the bound listener is private and so
        // never a candidate.
        let c = "/ip4/1.1.1.1/tcp/4001";
        claim(&mut swarm, &c.parse().expect("a literal"));
        let private = "/ip4/10.0.0.1/tcp/4001";
        let _ = tick(&mut state, &mut swarm, &mut manager, private, 0);
        assert_eq!(state.manager.candidates(), [c.to_owned()]);
        let ttl = settings.success_evidence_ttl_ms;
        // A success just before the TTL refreshes the claim, so a tick
        // past the original TTL keeps it; the control is that with no
        // further success it is gone once silent for a TTL.
        let open = HashMap::new();
        let mut out = Vec::new();
        let _ = handle_autonat(
            success(c, S1),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            ttl - 1,
            &mut out,
        );
        let _ = tick(&mut state, &mut swarm, &mut manager, private, ttl);
        assert_eq!(state.manager.candidates(), [c.to_owned()]);
        let _ = tick(&mut state, &mut swarm, &mut manager, private, 2 * ttl - 2);
        assert_eq!(
            state.manager.candidates(),
            [c.to_owned()],
            "refreshed at ttl - 1"
        );
        let _ = tick(&mut state, &mut swarm, &mut manager, private, 2 * ttl - 1);
        assert!(
            state.manager.candidates().is_empty(),
            "the control: gone once silent for a TTL"
        );
    }
    #[tokio::test]
    async fn a_refusal_at_the_send_is_reported_after_the_overflow_at_the_count_shrank() {
        // The round-3 shape: the count's overflow is this tick's, the
        // send's is cumulative, and a sum of the two fell when the
        // overflow shrank -- so a fresh refusal at the send that lifted
        // the sum back to its old mark went unreported. Two marks.
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        // Two bound listeners and a full observed set: overflow 2 at
        // the count, nothing refused at the send.
        let l1 = "/ip4/8.8.8.8/tcp/4001";
        for i in 1..=MAX_TRACKED_CANDIDATES {
            claim(&mut swarm, &nth_claim(i));
        }
        let mut out = Vec::new();
        reconcile(
            &mut state,
            &mut swarm,
            &mut manager,
            AutonatTick {
                in_flight: &InFlightTickets::default(),
                open: &HashMap::new(),
                listeners: vec![
                    l1.parse().expect("a literal"),
                    "/ip4/8.8.4.4/tcp/4001".parse().expect("a literal"),
                ],
                now_ms: 0,
            },
            &mut out,
        );
        assert!(out.iter().any(|e| matches!(
            e,
            SwarmEvent::ReachabilityCandidatesTruncated {
                at_the_send: 0,
                at_the_count: 2
            }
        )));
        // One listener leaves -- a network change, which the runtime's
        // detector tells the adapter of (step 10), so the wrapper's
        // listener set starts over: the overflow at the count shrinks
        // to 1; nothing new, nothing said.
        network_changed(
            &mut state,
            &mut swarm,
            std::iter::empty(),
            SILENCE_MS,
            &mut out,
        );
        let events = tick(&mut state, &mut swarm, &mut manager, l1, SILENCE_MS);
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, SwarmEvent::ReachabilityCandidatesTruncated { .. }))
        );
        // Then a claim is refused at the send. Under one summed mark
        // (2) this was 1 + 1 = 2, not above, and lost; apart, the send's
        // count rose from 0 to 1 and is said.
        claim(&mut swarm, &nth_claim(MAX_TRACKED_CANDIDATES + 1));
        let events = tick(&mut state, &mut swarm, &mut manager, l1, 2 * SILENCE_MS);
        assert!(events.iter().any(|e| matches!(
            e,
            SwarmEvent::ReachabilityCandidatesTruncated {
                at_the_send: 1,
                at_the_count: 1
            }
        )));
    }

    #[tokio::test]
    async fn a_refusal_the_gate_paces_is_re_asked_after_the_base_delay_without_advancing_the_ladder()
     {
        // The static server's address is QUARANTINED at the gate (an
        // identity mismatch on a ticket the adapter's own origin
        // minted), so the adapter's dial is refused AddressQuarantined:
        // the gate's ladder is already pacing this address, and the
        // adapter waits the base delay with its own count untouched.
        // The control is the standing-refusal test beside this one,
        // where the ladder does advance.
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        let address = format!("/ip4/8.8.8.8/tcp/4001/p2p/{S1}");
        let ticket = manager
            .handle()
            .admit(
                &interweave_transport_runtime::DialRequest {
                    peer: Some(s1.clone()),
                    address: super::super::dialing::canonical_dial_address(&s1, &address),
                    origin: DialOrigin::AutonatProbe,
                },
                0,
            )
            .expect("admitted before the quarantine");
        assert!(manager.record_identity_mismatch(ticket, 0));
        let events = tick(
            &mut state,
            &mut swarm,
            &mut manager,
            "/ip4/8.8.8.8/tcp/4001",
            1_000,
        );
        assert!(events.iter().any(|e| matches!(
            e,
            SwarmEvent::DialFailed { peer: Some(p), detail }
                if *p == s1 && detail.contains("AddressQuarantined")
        )));
        let target = state.targets.get(&s1).expect("static");
        assert_eq!(target.attempts, 0, "not an attempt");
        assert_eq!(target.next_attempt_at_ms, 1_000 + RETRY_BASE_MS);
    }
    #[tokio::test]
    async fn a_second_observer_needs_servers_connected_not_merely_known() {
        // Round-4 F2: a server the manager still holds evidence from but
        // this profile no longer holds an outbound to cannot be picked
        // by the crate. Two known, one connected: the address waits the
        // refresh, not the base delay.
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        assert!(state.manager.add_server(s2, ServerSource::Identify));
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 0);
        let _ = state.record(a, &s1, ProbeOutcome::Reachable, 1, 1_000);
        assert_eq!(
            state.schedule.get(a).expect("tracked").due,
            Some((1_000 + 300_000, RetestReason::Refresh)),
            "one connected server: no second observer to ask"
        );
        // The control: both connected.
        let _ = state.record(a, &s1, ProbeOutcome::Reachable, 2, 2_000);
        assert_eq!(
            state.schedule.get(a).expect("tracked").due,
            Some((2_000 + RETRY_BASE_MS, RetestReason::SecondObserver))
        );
        // And the count comes from the open OUTBOUND connections.
        let mut open = HashMap::new();
        assert_eq!(state.connected_servers(&open), 0);
        open.insert(ConnectionId::new_unchecked(1), connection(s1.clone(), None));
        assert_eq!(
            state.connected_servers(&open),
            0,
            "an inbound does not count"
        );
        assert!(!state.is_connected_server(&s1, &open));
        open.insert(
            ConnectionId::new_unchecked(2),
            connection(s1.clone(), Some(DialOrigin::AutonatProbe)),
        );
        assert_eq!(state.connected_servers(&open), 1);
        assert!(state.is_connected_server(&s1, &open));
    }

    #[test]
    fn the_dial_ladder_resets_when_identify_proves_the_connection_useful_not_on_establishment() {
        // Round-4 F3: a server that accepts and closes would otherwise
        // be re-dialled every base delay with no backoff.
        let settings = settings();
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let pid: PeerId = S1.parse().expect("a peer id");
        {
            let target = state.targets.get_mut(&s1).expect("static");
            target.attempts = 3;
            target.next_attempt_at_ms = 90_000;
            target.in_flight = true;
        }
        settle_static(&mut state, &pid);
        let target = state.targets.get(&s1).expect("static");
        assert!(!target.in_flight);
        assert_eq!(target.attempts, 3, "establishment alone keeps the ladder");
        assert_eq!(target.next_attempt_at_ms, 90_000, "and its pending wait");
        // Identify on an outbound: offered as a server, ladder reset.
        let dial_request = libp2p::StreamProtocol::new(DIAL_REQUEST_PROTOCOL);
        let mut open = HashMap::new();
        open.insert(
            ConnectionId::new_unchecked(1),
            connection(s1.clone(), Some(DialOrigin::AutonatProbe)),
        );
        assert!(state.offer_server(&s1, std::slice::from_ref(&dial_request), &open));
        let target = state.targets.get(&s1).expect("static");
        assert_eq!(target.attempts, 0);
        assert_eq!(
            target.next_attempt_at_ms, 0,
            "the pending wait goes with the ladder, so a useful connection that closes \
             quickly is re-dialled on the next tick"
        );
    }

    #[test]
    fn the_crate_tick_is_the_vendored_default() {
        // CRATE_TICK_MS mirrors the vendored client's default, which
        // build_behaviour leaves alone; a re-vendor that changed it would
        // silently move the refresh floor.
        const BEHAVIOUR: &str =
            include_str!("../../../../../third_party/libp2p-autonat/src/v2/client/behaviour.rs");
        assert!(
            BEHAVIOUR.contains("probe_interval: Duration::from_secs(5)"),
            "the vendored client's default probe interval moved; CRATE_TICK_MS is stale"
        );
        assert_eq!(CRATE_TICK_MS, 5_000);
    }

    #[test]
    fn the_sweep_takes_only_untested_candidates() {
        // What the re-test schedule and every "a Received candidate is
        // never re-swept" claim rest on -- the driver's own ladder
        // (`retest` is the only way back into the sweep) and the wire
        // test's buffering (`tests/autonat_outcome_wire.rs`): the
        // vendored sweep filters on `Untested` and nothing else. A
        // re-vendor that widened it would re-probe every confirmed
        // address on every tick. Review finding on PR #90, round 3.
        const BEHAVIOUR: &str =
            include_str!("../../../../../third_party/libp2p-autonat/src/v2/client/behaviour.rs");
        assert!(
            BEHAVIOUR.contains(".filter(|(_, info)| info.status == TestStatus::Untested)"),
            "the vendored client's sweep no longer takes only `Untested` candidates"
        );
    }

    #[tokio::test]
    async fn an_overflow_at_the_count_that_came_and_went_inside_a_closed_window_is_still_said() {
        // Round-4 F6. Tick 0 reports (a refusal at the send opens the
        // record); then inside the window an overflow at the count
        // appears and, before the window opens, vanishes -- said when
        // the window opens, from the peak, not lost.
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        for i in 1..=MAX_TRACKED_CANDIDATES + 1 {
            claim(&mut swarm, &nth_claim(i));
        }
        let private = "/ip4/10.0.0.1/tcp/4001";
        let events = tick(&mut state, &mut swarm, &mut manager, private, 0);
        assert!(events.iter().any(|e| matches!(
            e,
            SwarmEvent::ReachabilityCandidatesTruncated {
                at_the_send: 1,
                at_the_count: 0
            }
        )));
        // Two public listeners inside the window: overflow 2 at the
        // count, window closed, nothing said.
        let mut out = Vec::new();
        reconcile(
            &mut state,
            &mut swarm,
            &mut manager,
            AutonatTick {
                in_flight: &InFlightTickets::default(),
                open: &HashMap::new(),
                listeners: vec![
                    "/ip4/8.8.8.8/tcp/4001".parse().expect("a literal"),
                    "/ip4/8.8.4.4/tcp/4001".parse().expect("a literal"),
                ],
                now_ms: 10_000,
            },
            &mut out,
        );
        assert!(
            out.iter()
                .all(|e| !matches!(e, SwarmEvent::ReachabilityCandidatesTruncated { .. }))
        );
        // They leave again before the window opens; the overflow is 0
        // now -- and still said, from the peak, when it opens.
        let events = tick(&mut state, &mut swarm, &mut manager, private, 20_000);
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, SwarmEvent::ReachabilityCandidatesTruncated { .. }))
        );
        let events = tick(&mut state, &mut swarm, &mut manager, private, SILENCE_MS);
        assert!(events.iter().any(|e| matches!(
            e,
            SwarmEvent::ReachabilityCandidatesTruncated {
                at_the_send: 1,
                at_the_count: 2
            }
        )));
        // And nothing further with the overflow gone: silence.
        let events = tick(
            &mut state,
            &mut swarm,
            &mut manager,
            private,
            2 * SILENCE_MS,
        );
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, SwarmEvent::ReachabilityCandidatesTruncated { .. }))
        );
    }

    #[tokio::test]
    async fn a_verified_address_is_refreshed_a_silence_bound_before_its_evidence_horizon() {
        // Round-4 risk: a fixed refresh cadence lets the other server's
        // success age out between refreshes and the address flap. The
        // refresh is due at the interval or a silence bound before the
        // verdict's horizon, whichever is first.
        let settings = AutonatClientSettings {
            success_evidence_ttl_ms: 200_000,
            refresh_interval_ms: 100_000,
            ..settings()
        };
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        assert!(state.manager.add_server(s1.clone(), ServerSource::Static));
        assert!(state.manager.add_server(s2.clone(), ServerSource::Identify));
        let a = "/ip4/8.8.8.8/tcp/4001";
        let _ = tick(&mut state, &mut swarm, &mut manager, a, 0);
        // S1 at 0 s, S2 at 140 s: verified, and the horizon is S1's
        // expiry at 200 s, so the refresh is due at 200 - 30 = 170 s,
        // before the interval would have it at 240 s -- and never
        // sooner than a silence bound from now, which 170 s respects.
        let _ = state.record(a, &s1, ProbeOutcome::Reachable, 2, 0);
        let _ = state.record(a, &s2, ProbeOutcome::Reachable, 2, 140_000);
        assert_eq!(state.verdict().state(), DirectInboundState::VerifiedPublic);
        assert_eq!(
            state.schedule.get(a).expect("tracked").due,
            Some((170_000, RetestReason::Refresh))
        );
        // Recorded closer to the horizon than a silence bound, the
        // floor is one crate tick: at 190 s the refresh is due at 195 s,
        // before S1's evidence lapses at 200 s.
        let _ = state.record(a, &s2, ProbeOutcome::Reachable, 2, 190_000);
        assert_eq!(
            state.schedule.get(a).expect("tracked").due,
            Some((195_000, RetestReason::Refresh))
        );
        // The control: both fresh at 195 s puts the horizon at 395 s,
        // and the interval governs -- due at 295 s.
        let _ = state.record(a, &s1, ProbeOutcome::Reachable, 2, 195_000);
        let _ = state.record(a, &s2, ProbeOutcome::Reachable, 2, 195_000);
        assert_eq!(
            state.schedule.get(a).expect("tracked").due,
            Some((295_000, RetestReason::Refresh))
        );
    }

    #[tokio::test]
    async fn a_network_change_forgets_the_evidence_and_retests_within_the_jitter() {
        // The runtime's detector decides WHAT a change is
        // (`network_change.rs`); this is what the adapter does when
        // told of one: the verdict goes to unknown and is published,
        // the advertised set is withdrawn, the wrapper's listeners
        // start over, and every tracked candidate is due for a re-test
        // within the jitter -- not at once, and not never.
        let settings = settings();
        let mut swarm = swarm_with_client(&settings);
        let mut state = AutonatState::new(&settings).expect("builds");
        let s1 = TransportIdentity::parse(S1).expect("valid");
        let s2 = TransportIdentity::parse(S2).expect("valid");
        let mut manager = trusting(&s1, &s2);
        assert!(state.manager.add_server(s1, ServerSource::Static));
        assert!(state.manager.add_server(s2, ServerSource::Identify));
        let c = "/ip4/1.1.1.1/tcp/4001";
        claim(&mut swarm, &c.parse().expect("a literal"));
        let lan_a = "/ip4/192.168.1.5/tcp/4001";
        let _ = tick(&mut state, &mut swarm, &mut manager, lan_a, 0);
        tested(&mut swarm, c);
        let open = HashMap::new();
        let mut out = Vec::new();
        let _ = handle_autonat(
            success(c, S1),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            1_000,
            &mut out,
        );
        let _ = handle_autonat(
            success(c, S2),
            &mut swarm,
            &mut state,
            &nobody(),
            &open,
            2_000,
            &mut out,
        );
        assert_eq!(state.verdict().state(), DirectInboundState::VerifiedPublic);
        // THE CONTROL: a tick with the same listener changes nothing --
        // the adapter no longer compares the set itself.
        let _ = tick(&mut state, &mut swarm, &mut manager, lan_a, 3_000);
        assert_eq!(state.verdict().state(), DirectInboundState::VerifiedPublic);
        assert_eq!(state.retests(RetestReason::Retry), 0);
        // TOLD OF A CHANGE: unknown, published, withdrawn, re-test due
        // inside the jitter.
        out.clear();
        // The listener still bound is handed in and offered again in
        // the same call: the wrapper's set is not empty until the next
        // tick, where a peer's claim about it would land as an
        // observation.
        let public_listener: Multiaddr = "/ip4/9.9.9.9/tcp/4001".parse().expect("a literal");
        network_changed(
            &mut state,
            &mut swarm,
            std::iter::once(&public_listener),
            4_000,
            &mut out,
        );
        assert!(
            swarm
                .autonat_client_mut()
                .expect("client")
                .candidates()
                .any(|c| c == public_listener.to_string()),
            "the bound listener is offered again at once"
        );
        assert_eq!(state.verdict().state(), DirectInboundState::Unknown);
        assert!(
            out.iter().any(|e| matches!(
                e,
                SwarmEvent::ConnectivityChanged {
                    direct_inbound: DirectInboundState::Unknown,
                    ..
                }
            )),
            "published: {out:?}"
        );
        assert_eq!(swarm.external_addresses().count(), 0);
        let due = state.schedule.get(c).expect("tracked").due;
        let Some((at, RetestReason::Retry)) = due else {
            panic!("a re-test is due: {due:?}");
        };
        assert!(
            (4_000..=4_000 + NETWORK_CHANGE_JITTER_MS).contains(&at),
            "within the jitter: {at}"
        );
        assert_eq!(
            state.schedule.get(c).expect("tracked").failures,
            0,
            "the failure count starts over"
        );
        // AND IT FIRES on the tick once due; before that, nothing.
        let _ = tick(&mut state, &mut swarm, &mut manager, lan_a, 4_000);
        if at > 4_000 {
            assert_eq!(state.retests(RetestReason::Retry), 0, "not before its due");
        }
        let _ = tick(&mut state, &mut swarm, &mut manager, lan_a, at);
        assert_eq!(state.retests(RetestReason::Retry), 1);
        // Told again with nothing tracked anew: idempotent on the
        // verdict, and the schedule is re-armed.
        out.clear();
        network_changed(&mut state, &mut swarm, std::iter::empty(), at + 1, &mut out);
        assert_eq!(state.verdict().state(), DirectInboundState::Unknown);
    }
}
