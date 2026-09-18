// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Circuit Relay v2 CLIENT's adapter: the pinned client behaviour
//! and transport on one side, `ReservationManager` on the other, and
//! this module deciding nothing either of them already decides.
//!
//! # What sits where (`RELAY.md` §§3-5, note of 2026-09-18)
//!
//! The manager owns policy: which relays, how many, when to ask again,
//! and which relay-derived addresses this profile advertises. The crate
//! owns the wire: it obtains a reservation by LISTENING on
//! `<relay>/p2p/<relay-id>/p2p-circuit`, dials the relay's control
//! connection if it holds none, renews on its own, and reports the
//! reservation's addresses -- the relay's own external addresses, each
//! suffixed `/p2p-circuit/p2p/<self>` -- as listener addresses, and its
//! loss as the listener closing. This driver is the seam: it turns the
//! manager's `Reserve` into a listener and its `Release` into the
//! listener's removal, folds every listener outcome back into the
//! manager, and makes the Swarm advertise exactly what the manager says.
//!
//! # The reservation's dial is a behaviour dial (route 1)
//!
//! When the client holds no direct connection to the relay it emits
//! `ToSwarm::Dial` for it (`priv_client.rs`, the `ListenReq` arm), so
//! the field is wrapped in [`Attributing`] with
//! `always(DialOrigin::RelayReservation)`: the dial reaches the
//! outbound gate under the origin `RELAY.md` §2 names and is admitted
//! or refused by the root policy like every other behaviour dial
//! (SPIKE-004 R2/R6). A refusal is synchronous, the crate drops the
//! listener's channel on the `DialFailure`, and the listener closes --
//! which is the same outcome the manager sees for a relay that would
//! not connect, backed off on its ladder.
//!
//! # Every ask ends in a listener event
//!
//! `Requested` has no expiry in the manager, and needs none here: a
//! dial the gate refuses, a dial that fails, a relay that refuses the
//! reservation, a reservation request that times out (the handler's
//! own bound) and a relay whose connection closes all end in
//! `ListenerClosed`; an acceptance ends in `NewListenAddr`, one per
//! address the relay reported, and a reservation the relay accepted
//! with NO address closes the listener with an error (SPIKE-004 note
//! 10). The one close that is NOT a failure is the one this driver
//! caused: a listener removed for a `Release` closes too, and its id is
//! remembered until that close arrives so it is not reported back as a
//! loss -- the manager refuses such a report by name regardless.
//!
//! # What the Swarm advertises is the manager's set
//!
//! [`ReservationScope`] swallows the crate's own `ExternalAddrConfirmed`;
//! after every change the Swarm's external addresses are brought to
//! [`ReservationManager::advertised`] -- added on acceptance, removed on
//! loss, release or forget, in the same turn the manager recorded it
//! (`RELAY.md` §5; SPIKE-004 R10.10 measured the withdrawal within a
//! second of the loss, and here it is the same event).
//!
//! # Learning relays (`RELAY.md` §3)
//!
//! Under `use_authorized_identify_relays`, an AUTHORIZED peer whose
//! Identify advertises the hop protocol is offered to the manager as a
//! learned relay at the listen addresses it reported, minus any that
//! is itself a circuit; the manager keeps at most sixteen, eight
//! addresses each, and asks them only when the static relays cannot
//! fill the target. A learned relay that loses its authorization is
//! forgotten on the next tick, its addresses withdrawn; a static one
//! stays configured and the gate refuses its dial.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use interweave_profile_config::connectivity::RelayClientConfig;
use interweave_transport_api::{DirectInboundState, TransportIdentity};
use interweave_transport_runtime::relay::{
    Action, MAX_RESERVATIONS_CEILING, MAX_STATIC_RELAYS, RefusedRelayReport, RelaySource,
    ReservationConfig, ReservationManager, ReservationState, Standing,
};
use interweave_transport_runtime::{
    ConnectionClass, ConnectionManager, DialOrigin, SnapshotHandle,
};
use libp2p::core::transport::ListenerId;
use libp2p::relay::client::{Behaviour as Client, Event as ClientEvent};
use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::{Multiaddr, PeerId, identify, multiaddr::Protocol};
use rand::RngCore as _;
use rand::rngs::OsRng;

use super::messages::{RelayReservationOutcome, SwarmEvent};
use crate::attribution::{Attributing, DialAttribution, always};
use crate::behaviour::SubstrateBehaviourEvent;
use crate::class_gate::{ClassGated, Service};
use crate::gated_swarm::GatedSwarm;
use crate::reservation_scope::ReservationScope;

/// The hop protocol a relay advertises through Identify -- restated
/// here so a learned relay is recognised by name, and pinned against
/// the crate's own constant by `the_hop_protocol_is_the_crates`.
pub const HOP_PROTOCOL: &str = "/libp2p/circuit/relay/0.2.0/hop";

/// The client field's type in the composed behaviour.
pub type ClientField = Toggle<ClassGated<Attributing<ReservationScope<Client>>>>;

/// One configured relay: its identity and the address to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticRelay {
    /// The relay's PeerId, from the address's `/p2p/` component.
    pub peer: TransportIdentity,
    /// The whole configured multiaddr, as given.
    pub address: String,
}

/// The client's settings, translated from `profile-config` here rather
/// than there: the neutral configuration crate names no libp2p type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayClientSettings {
    /// Relays this profile reserves on first.
    pub static_relays: Vec<StaticRelay>,
    /// Whether an authorized peer advertising the hop protocol may be
    /// reserved on as well. Off: only static relays are asked.
    pub use_authorized_identify_relays: bool,
    /// The manager's targets and ladder.
    pub reservations: ReservationConfig,
}

impl RelayClientSettings {
    /// Translate the validated profile block.
    ///
    /// # Errors
    /// A static relay address that is not a multiaddr, carries no
    /// `/p2p/` component, is itself a circuit, or whose PeerId the
    /// neutral grammar refuses; or a block the manager would refuse
    /// ([`Self::validate`]).
    pub fn from_profile(config: &RelayClientConfig) -> Result<Self, &'static str> {
        let mut static_relays = Vec::with_capacity(config.static_relays.len());
        for address in &config.static_relays {
            let peer = relay_of(address)?;
            static_relays.push(StaticRelay {
                peer,
                address: address.clone(),
            });
        }
        let settings = Self {
            static_relays,
            use_authorized_identify_relays: config.use_authorized_identify_relays,
            reservations: ReservationConfig {
                target_private_or_unknown: config.target_reservations_private_or_unknown,
                target_public: config.target_reservations_public,
                max_reservations: config.max_reservations,
                retry_min_ms: u64::from(config.retry_min_ms),
                retry_max_ms: u64::from(config.retry_max_ms),
            },
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Refuse a configuration the driver cannot honour: the manager's
    /// own rules, restated so a composition root building settings by
    /// hand fails here rather than at the manager, and the static list's
    /// bound.
    ///
    /// # Errors
    /// The first rule broken, named.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.static_relays.len() > MAX_STATIC_RELAYS {
            return Err("relay static_relays holds at most 16 entries");
        }
        let r = &self.reservations;
        if r.max_reservations == 0 || r.max_reservations > MAX_RESERVATIONS_CEILING {
            return Err("relay max_reservations must be 1..=8");
        }
        if r.target_private_or_unknown > r.max_reservations
            || r.target_public > r.max_reservations
            || r.target_public > r.target_private_or_unknown
        {
            return Err(
                "relay targets must not exceed max_reservations, nor the public one the private",
            );
        }
        if r.retry_min_ms == 0 || r.retry_min_ms > r.retry_max_ms {
            return Err("relay retry_min must be positive and at most retry_max");
        }
        for relay in &self.static_relays {
            relay_of(&relay.address)?;
        }
        Ok(())
    }
}

impl Default for RelayClientSettings {
    /// No static relay, no learning, the manager's defaults.
    fn default() -> Self {
        Self {
            static_relays: Vec::new(),
            use_authorized_identify_relays: false,
            reservations: ReservationConfig::default(),
        }
    }
}

/// The relay a configured address names.
fn relay_of(address: &str) -> Result<TransportIdentity, &'static str> {
    let multiaddr: Multiaddr = address
        .parse()
        .map_err(|_| "relay static_relays: not a multiaddr")?;
    if multiaddr.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
        return Err("relay static_relays: a relay reached through a circuit is not a relay");
    }
    let peer_id = multiaddr
        .iter()
        .find_map(|p| match p {
            Protocol::P2p(id) => Some(id),
            _ => None,
        })
        .ok_or("relay static_relays: no /p2p/ component")?;
    TransportIdentity::parse(peer_id.to_base58())
        .map_err(|_| "relay static_relays: PeerId outside the neutral grammar")
}

/// Build the client field: the crate's client under the three wrappers,
/// announcing `RelayReservation` for its control dial into
/// `attribution`, class-gated for the infrastructure service so the
/// stop protocol -- the relay handing this profile an inbound circuit --
/// is offered to `DataPlaneTrusted` and `ConnectivityInfrastructureOnly`
/// peers and to nobody else. The client itself comes from the Swarm
/// builder, which is where the transport half of it is composed.
#[must_use]
pub fn build_behaviour(
    client: Client,
    attribution: DialAttribution,
    policy: SnapshotHandle,
) -> ClientField {
    Toggle::from(Some(ClassGated::for_service(
        Attributing::new(
            ReservationScope::new(client),
            always(DialOrigin::RelayReservation),
            attribution,
        ),
        policy,
        Service::ConnectivityInfrastructure,
    )))
}

/// What the driver holds beside the manager: the listener each ask
/// opened, the ones it closed itself, and what the Swarm advertises.
#[derive(Debug)]
pub struct RelayState {
    settings: RelayClientSettings,
    manager: ReservationManager,
    /// The relay each open listener reserves on. One per relay that is
    /// `Requested` or `Active`, so bounded by the manager's candidates.
    listeners: HashMap<ListenerId, TransportIdentity>,
    by_relay: BTreeMap<TransportIdentity, ListenerId>,
    /// Listeners this driver removed and whose close has not yet been
    /// reported; each entry leaves on that close. Bounded by
    /// `listeners`, from which every entry came.
    released: HashSet<ListenerId>,
    /// Which of a relay's addresses the next ask listens through, so a
    /// relay with several is tried at each in turn. Pruned with the
    /// relay.
    next_address: BTreeMap<TransportIdentity, usize>,
    /// The circuit addresses the Swarm currently advertises on this
    /// driver's account -- the manager's set as of the last sync.
    advertised: BTreeSet<String>,
    last_standing: Option<(Standing, usize, usize, usize, usize, usize)>,
    counters: RelayCounters,
}

/// `RELAY.md` §11's `relay_reservation_events_total{outcome}`, read
/// back by tests and status.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RelayCounters {
    /// Addresses newly advertised.
    pub accepted: usize,
    /// Acceptances that re-reported an advertised address.
    pub renewed: usize,
    /// Active reservations that closed.
    pub lost: usize,
    /// Asks that closed before an address was reported.
    pub failed: usize,
    /// Reservations this driver gave up.
    pub released: usize,
    /// Reports the manager refused, by reason.
    pub refused: BTreeMap<&'static str, usize>,
    /// The crate's renewal events seen.
    pub renewals_seen: usize,
    /// Circuit events seen and not acted on (step 7's).
    pub circuits_seen: usize,
}

impl RelayState {
    /// Build the driver's state from settings: the manager under the
    /// block's targets, offered every static relay.
    ///
    /// # Errors
    /// A block the manager refuses, or a static relay it refuses for
    /// room -- more than eight addresses for one relay.
    pub fn new(settings: &RelayClientSettings) -> Result<Self, &'static str> {
        settings.validate()?;
        let mut manager = ReservationManager::new(settings.reservations.clone())
            .map_err(|_| "relay: the manager refused the reservation block")?;
        for relay in &settings.static_relays {
            if !manager.add_static(relay.peer.clone(), &relay.address) {
                return Err("relay static_relays: more than eight addresses for one relay");
            }
        }
        Ok(Self {
            settings: settings.clone(),
            manager,
            listeners: HashMap::new(),
            by_relay: BTreeMap::new(),
            released: HashSet::new(),
            next_address: BTreeMap::new(),
            advertised: BTreeSet::new(),
            last_standing: None,
            counters: RelayCounters::default(),
        })
    }

    /// The manager, read-only.
    #[must_use]
    pub const fn manager(&self) -> &ReservationManager {
        &self.manager
    }

    /// The counters so far.
    #[must_use]
    pub const fn counters(&self) -> &RelayCounters {
        &self.counters
    }

    /// Whether this driver holds a listener for `relay`.
    #[must_use]
    pub fn is_listening_through(&self, relay: &TransportIdentity) -> bool {
        self.by_relay.contains_key(relay)
    }

    fn jitter_ms(&self) -> u64 {
        let span = self.settings.reservations.retry_min_ms.saturating_add(1);
        OsRng.next_u64() % span
    }
}

/// What `handle_relay` did with an event.
pub(super) enum RelayHandled {
    /// Consumed here; nothing below reads it.
    Consumed,
    /// Peeked, and passed on unchanged.
    Passed(Box<Libp2pSwarmEvent<SubstrateBehaviourEvent>>),
}

/// The tick: drop de-authorized learned relays, ask and release as the
/// manager says, and bring the Swarm's advertised set to the manager's.
pub(super) fn reconcile(
    state: &mut RelayState,
    swarm: &mut GatedSwarm,
    trust: &ConnectionManager,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) {
    forget_deauthorized(state, swarm, trust, now_ms, out);
    let actions = state.manager.tick(now_ms);
    act(state, swarm, actions, now_ms, out);
    sync(state, swarm, now_ms, out);
}

/// Forget every learned relay that is no longer authorized, with its
/// listener and its addresses. Called on a trust change, so the
/// withdrawal precedes the revoked connection's close, and on every
/// tick, so a class that changed by any other route is caught too. A
/// static relay is not forgotten: it stays configured, and the gate
/// refuses its next dial.
pub(super) fn forget_deauthorized(
    state: &mut RelayState,
    swarm: &mut GatedSwarm,
    trust: &ConnectionManager,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) {
    let stale: Vec<TransportIdentity> = state
        .manager
        .relays()
        .filter(|(relay, source)| {
            *source == RelaySource::Learned && !authorized(trust.classify(relay))
        })
        .map(|(relay, _)| relay.clone())
        .collect();
    if stale.is_empty() {
        return;
    }
    for relay in stale {
        forget(state, swarm, &relay, out);
    }
    sync(state, swarm, now_ms, out);
}

/// The direct-inbound verdict changed: the target follows it, and a
/// surplus is released now rather than on the next tick.
pub(super) fn set_direct_inbound(
    state: &mut RelayState,
    swarm: &mut GatedSwarm,
    verdict: DirectInboundState,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) {
    if state.manager.direct_inbound() == verdict {
        return;
    }
    let actions = state.manager.set_direct_inbound(verdict);
    act(state, swarm, actions, now_ms, out);
    sync(state, swarm, now_ms, out);
}

/// See one Swarm event: a listener outcome for a reservation is folded
/// into the manager and consumed; the client's own events are counted
/// and consumed; Identify is peeked for a relay to learn and passed on.
pub(super) fn handle_relay(
    event: Libp2pSwarmEvent<SubstrateBehaviourEvent>,
    swarm: &mut GatedSwarm,
    state: &mut RelayState,
    trust: &ConnectionManager,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) -> RelayHandled {
    match event {
        Libp2pSwarmEvent::NewListenAddr {
            listener_id,
            address,
        } if state.listeners.contains_key(&listener_id) => {
            let relay = state.listeners[&listener_id].clone();
            accepted(state, &relay, &address, now_ms, out);
            sync(state, swarm, now_ms, out);
            RelayHandled::Consumed
        }
        Libp2pSwarmEvent::ListenerClosed {
            listener_id,
            reason,
            ..
        } if state.released.remove(&listener_id) => {
            // THE CLOSE THIS DRIVER CAUSED: reported as `Released` when
            // the listener was removed, not as a loss now.
            let _ = reason;
            RelayHandled::Consumed
        }
        Libp2pSwarmEvent::ListenerClosed {
            listener_id,
            reason,
            ..
        } if state.listeners.contains_key(&listener_id) => {
            if let Some(relay) = state.listeners.remove(&listener_id) {
                state.by_relay.remove(&relay);
                let detail = reason.err().map(|e| e.to_string());
                closed(state, &relay, detail, now_ms, out);
                sync(state, swarm, now_ms, out);
            }
            RelayHandled::Consumed
        }
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::RelayClient(event)) => {
            match event {
                ClientEvent::ReservationReqAccepted { renewal, .. } => {
                    // The addresses arrive as the listener's; this is
                    // only the crate saying the exchange happened.
                    if renewal {
                        state.counters.renewals_seen += 1;
                    }
                }
                ClientEvent::OutboundCircuitEstablished { .. }
                | ClientEvent::InboundCircuitEstablished { .. } => {
                    // Relayed peer paths are step 7's; nothing here
                    // opens or accepts a circuit, and a circuit the
                    // relay hands over is admitted or refused by the
                    // pre-auth and connection gates like any inbound.
                    state.counters.circuits_seen += 1;
                }
            }
            RelayHandled::Consumed
        }
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Identify(
            identify::Event::Received {
                peer_id, ref info, ..
            },
        )) if state.settings.use_authorized_identify_relays => {
            learn(state, &peer_id, info, trust);
            RelayHandled::Passed(Box::new(event))
        }
        other => RelayHandled::Passed(Box::new(other)),
    }
}

fn authorized(class: ConnectionClass) -> bool {
    matches!(
        class,
        ConnectionClass::DataPlaneTrusted | ConnectionClass::ConnectivityInfrastructureOnly
    )
}

/// Offer an Identify-advertised relay to the manager (`RELAY.md` §3,
/// source 2): authorized, advertising the hop protocol, at each listen
/// address that is not itself a circuit. The manager bounds the set.
fn learn(
    state: &mut RelayState,
    peer_id: &PeerId,
    info: &identify::Info,
    trust: &ConnectionManager,
) {
    if !info.protocols.iter().any(|p| p.as_ref() == HOP_PROTOCOL) {
        return;
    }
    let Ok(relay) = TransportIdentity::parse(peer_id.to_base58()) else {
        return;
    };
    if !authorized(trust.classify(&relay)) {
        return;
    }
    for address in &info.listen_addrs {
        if address.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
            continue;
        }
        let _ = state.manager.learn(relay.clone(), &address.to_string());
    }
}

fn act(
    state: &mut RelayState,
    swarm: &mut GatedSwarm,
    actions: Vec<Action>,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) {
    for action in actions {
        match action {
            Action::Reserve { relay, addresses } => {
                listen(state, swarm, &relay, &addresses, now_ms, out)
            }
            Action::Release { relay, addresses } => release(state, swarm, &relay, addresses, out),
        }
    }
}

/// The address the crate listens on for a reservation: the relay's
/// address, its identity, the circuit marker. A configured address
/// already carrying `/p2p/<relay>` is not given it twice.
fn circuit_listen_address(relay: &PeerId, address: &Multiaddr) -> Multiaddr {
    let mut base: Multiaddr = address
        .iter()
        .filter(|p| !matches!(p, Protocol::P2p(id) if id == relay))
        .collect();
    base.push(Protocol::P2p(*relay));
    base.push(Protocol::P2pCircuit);
    base
}

fn listen(
    state: &mut RelayState,
    swarm: &mut GatedSwarm,
    relay: &TransportIdentity,
    addresses: &[String],
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) {
    let Ok(peer_id) = relay.as_str().parse::<PeerId>() else {
        failed_now(
            state,
            relay,
            "relay identity outside libp2p's grammar",
            now_ms,
            out,
        );
        return;
    };
    // EACH ADDRESS IN TURN across asks, so a relay with several is not
    // dialled forever at the one that fails.
    let slot = state.next_address.entry(relay.clone()).or_insert(0);
    let start = *slot;
    *slot = slot.wrapping_add(1);
    let chosen = (0..addresses.len())
        .map(|i| &addresses[(start + i) % addresses.len()])
        .find_map(|a| {
            let parsed: Multiaddr = a.parse().ok()?;
            (!parsed.iter().any(|p| matches!(p, Protocol::P2pCircuit))).then_some(parsed)
        });
    let Some(address) = chosen else {
        failed_now(
            state,
            relay,
            "no address of the relay is a direct one",
            now_ms,
            out,
        );
        return;
    };
    match swarm.listen_on(circuit_listen_address(&peer_id, &address)) {
        Ok(id) => {
            state.listeners.insert(id, relay.clone());
            state.by_relay.insert(relay.clone(), id);
        }
        Err(e) => failed_now(state, relay, &e.to_string(), now_ms, out),
    }
}

/// An ask that failed before any listener existed: the manager backs
/// the relay off exactly as for a listener that closed.
fn failed_now(
    state: &mut RelayState,
    relay: &TransportIdentity,
    detail: &str,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) {
    closed(state, relay, Some(detail.to_owned()), now_ms, out);
}

fn release(
    state: &mut RelayState,
    swarm: &mut GatedSwarm,
    relay: &TransportIdentity,
    addresses: Vec<String>,
    out: &mut Vec<SwarmEvent>,
) {
    if let Some(id) = state.by_relay.remove(relay) {
        state.listeners.remove(&id);
        if swarm.remove_listener(id) {
            state.released.insert(id);
        }
    }
    state.counters.released += 1;
    out.push(SwarmEvent::RelayReservationChanged {
        relay: relay.clone(),
        outcome: RelayReservationOutcome::Released,
        addresses,
        detail: None,
    });
}

/// A relay is gone from the manager: its listener with it, and its
/// addresses.
fn forget(
    state: &mut RelayState,
    swarm: &mut GatedSwarm,
    relay: &TransportIdentity,
    out: &mut Vec<SwarmEvent>,
) {
    let addresses = state.manager.forget(relay);
    state.next_address.remove(relay);
    if let Some(id) = state.by_relay.remove(relay) {
        state.listeners.remove(&id);
        if swarm.remove_listener(id) {
            state.released.insert(id);
        }
    }
    state.counters.released += 1;
    out.push(SwarmEvent::RelayReservationChanged {
        relay: relay.clone(),
        outcome: RelayReservationOutcome::Released,
        addresses,
        detail: Some("the relay is no longer authorized".to_owned()),
    });
}

fn accepted(
    state: &mut RelayState,
    relay: &TransportIdentity,
    address: &Multiaddr,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) {
    let text = address.to_string();
    match state.manager.record_accepted(relay, &text, now_ms) {
        Ok(true) => {
            state.counters.accepted += 1;
            out.push(SwarmEvent::RelayReservationChanged {
                relay: relay.clone(),
                outcome: RelayReservationOutcome::Accepted,
                addresses: vec![text],
                detail: None,
            });
        }
        Ok(false) => {
            state.counters.renewed += 1;
            out.push(SwarmEvent::RelayReservationChanged {
                relay: relay.clone(),
                outcome: RelayReservationOutcome::Renewed,
                addresses: vec![text],
                detail: None,
            });
        }
        Err(reason) => refused(state, relay, Some(text), reason, out),
    }
}

fn closed(
    state: &mut RelayState,
    relay: &TransportIdentity,
    detail: Option<String>,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) {
    let was_active = matches!(
        state.manager.state(relay),
        Some(ReservationState::Active { .. })
    );
    let jitter = state.jitter_ms();
    match state.manager.record_failed(relay, now_ms, jitter) {
        Ok(addresses) => {
            let outcome = if was_active {
                state.counters.lost += 1;
                RelayReservationOutcome::Lost
            } else {
                state.counters.failed += 1;
                RelayReservationOutcome::Failed
            };
            out.push(SwarmEvent::RelayReservationChanged {
                relay: relay.clone(),
                outcome,
                addresses,
                detail,
            });
        }
        Err(reason) => refused(state, relay, None, reason, out),
    }
}

fn refused(
    state: &mut RelayState,
    relay: &TransportIdentity,
    address: Option<String>,
    reason: RefusedRelayReport,
    out: &mut Vec<SwarmEvent>,
) {
    *state
        .counters
        .refused
        .entry(refusal_label(reason))
        .or_insert(0) += 1;
    out.push(SwarmEvent::RelayReportRefused {
        relay: relay.clone(),
        address,
        reason,
    });
}

/// §11's `refused_*` label for a refusal.
#[must_use]
pub const fn refusal_label(reason: RefusedRelayReport) -> &'static str {
    match reason {
        RefusedRelayReport::UnknownRelay => "refused_unknown_relay",
        RefusedRelayReport::UnrequestedAcceptance => "refused_unrequested_acceptance",
        RefusedRelayReport::UnrequestedFailure => "refused_unrequested_failure",
        RefusedRelayReport::EmptyAddress => "refused_empty_address",
        RefusedRelayReport::AddressesFull => "refused_addresses_full",
    }
}

/// Bring the Swarm's external addresses to the manager's advertised
/// set, and say where the target stands when that changed.
fn sync(state: &mut RelayState, swarm: &mut GatedSwarm, now_ms: u64, out: &mut Vec<SwarmEvent>) {
    let wanted: BTreeSet<String> = state.manager.advertised().into_iter().collect();
    for gone in state.advertised.difference(&wanted) {
        if let Ok(addr) = gone.parse::<Multiaddr>() {
            swarm.remove_external_address(&addr);
        }
    }
    for fresh in wanted.difference(&state.advertised) {
        if let Ok(addr) = fresh.parse::<Multiaddr>() {
            swarm.add_external_address(addr);
        }
    }
    state.advertised = wanted;
    let standing = (
        state.manager.standing(),
        state.manager.active(),
        state.manager.target(),
        state.manager.requested(),
        state.manager.askable_now(now_ms),
        state.manager.candidates(),
    );
    if state.last_standing != Some(standing) {
        state.last_standing = Some(standing);
        out.push(SwarmEvent::RelayStandingChanged {
            standing: standing.0,
            active: standing.1,
            target: standing.2,
            requested: standing.3,
            askable: standing.4,
            candidates: standing.5,
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    const R1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    #[test]
    fn the_hop_protocol_is_the_crates() {
        assert_eq!(HOP_PROTOCOL, libp2p::relay::HOP_PROTOCOL_NAME.as_ref());
    }

    #[test]
    fn from_profile_translates_the_block_and_refuses_what_the_manager_would() {
        let config = RelayClientConfig {
            static_relays: vec![format!("/ip4/192.0.2.1/tcp/4001/p2p/{R1}")],
            ..RelayClientConfig::default()
        };
        let settings = RelayClientSettings::from_profile(&config).expect("valid");
        assert_eq!(settings.static_relays.len(), 1);
        assert_eq!(settings.static_relays[0].peer.as_str(), R1);
        assert_eq!(settings.reservations.target_private_or_unknown, 2);
        assert_eq!(settings.reservations.retry_min_ms, 5_000);
        assert!(RelayState::new(&settings).is_ok());

        let rows: [(Vec<String>, &str); 4] = [
            (vec!["not an address".to_owned()], "not a multiaddr"),
            (vec!["/ip4/192.0.2.1/tcp/4001".to_owned()], "no /p2p/"),
            (
                vec![format!(
                    "/ip4/192.0.2.9/tcp/1/p2p/{R1}/p2p-circuit/p2p/{R1}"
                )],
                "through a circuit",
            ),
            (
                (0..17)
                    .map(|i| format!("/ip4/192.0.2.{i}/tcp/4001/p2p/{R1}"))
                    .collect(),
                "at most 16",
            ),
        ];
        for (relays, expected) in rows {
            let config = RelayClientConfig {
                static_relays: relays,
                ..RelayClientConfig::default()
            };
            let err = RelayClientSettings::from_profile(&config).expect_err("refused");
            assert!(err.contains(expected), "{err} should name {expected}");
        }
        // The manager's own rules, restated: a target above the
        // maximum.
        let settings = RelayClientSettings {
            reservations: ReservationConfig {
                target_private_or_unknown: 9,
                ..ReservationConfig::default()
            },
            ..RelayClientSettings::default()
        };
        assert!(settings.validate().is_err());
        // Nine addresses for one static relay: the manager refuses the
        // ninth and the state does not build.
        let settings = RelayClientSettings {
            static_relays: (0..9)
                .map(|i| StaticRelay {
                    peer: TransportIdentity::parse(R1).expect("id"),
                    address: format!("/ip4/192.0.2.1/tcp/{i}/p2p/{R1}"),
                })
                .collect(),
            ..RelayClientSettings::default()
        };
        assert!(RelayState::new(&settings).is_err());
    }

    #[test]
    fn the_listen_address_carries_the_relay_once_and_the_circuit_marker() {
        let relay: PeerId = R1.parse().expect("peer id");
        let bare: Multiaddr = "/ip4/192.0.2.1/tcp/4001".parse().expect("addr");
        let with_id: Multiaddr = format!("/ip4/192.0.2.1/tcp/4001/p2p/{R1}")
            .parse()
            .expect("addr");
        let expected: Multiaddr = format!("/ip4/192.0.2.1/tcp/4001/p2p/{R1}/p2p-circuit")
            .parse()
            .expect("addr");
        assert_eq!(circuit_listen_address(&relay, &bare), expected);
        assert_eq!(circuit_listen_address(&relay, &with_id), expected);
    }

    #[test]
    fn every_refusal_has_a_label_of_its_own() {
        let all = [
            RefusedRelayReport::UnknownRelay,
            RefusedRelayReport::UnrequestedAcceptance,
            RefusedRelayReport::UnrequestedFailure,
            RefusedRelayReport::EmptyAddress,
            RefusedRelayReport::AddressesFull,
        ];
        let labels: BTreeSet<&str> = all.iter().map(|r| refusal_label(*r)).collect();
        assert_eq!(labels.len(), all.len());
        assert!(labels.iter().all(|l| l.starts_with("refused_")));
    }
}
