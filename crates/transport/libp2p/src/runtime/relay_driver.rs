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
//! # Every ask ends in a listener event, or in the horizon
//!
//! A dial the gate refuses, a dial that fails, a relay that refuses the
//! reservation, a reservation request that times out (the handler's
//! own bound) and a relay whose connection closes all end in
//! `ListenerClosed`; an acceptance ends in `NewListenAddr`, one per
//! address the relay reported, and a reservation the relay accepted
//! with NO address closes the listener with an error (SPIKE-004 note
//! 10). One path ends in nothing: a relay that loses its authorization
//! between the dial and its establishment. `ClassGated` then denies the
//! connection at the established hook and hides it -- and its close --
//! from the client, which keeps the listener's channel open for the
//! process's life. So `Requested` has a horizon here,
//! [`REQUEST_HORIZON_MS`], past which the ask is abandoned and recorded
//! as failed; and a trust change that makes a relay `Unauthorized`
//! abandons its ask at once, static relay or learned. The one close
//! that is NOT a failure is the one this driver caused: a listener it
//! removed closes too, and its id is remembered until that close
//! arrives -- every event of it in between consumed -- so nothing of
//! it is reported back as a loss or, worse, as an ordinary listener;
//! the manager refuses such a report by name regardless.
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
//! forgotten on the trust change that revoked it, and on any tick, its
//! addresses withdrawn; a static one stays configured, its ask
//! abandoned the same way, and the gate refuses its next dial.

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
use rand::Rng as _;

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

/// How long an ask may stay `Requested` before it is abandoned: the
/// largest handshake timeout the profile allows, plus the pinned
/// client's own bound on a reservation request (`libp2p-relay` 0.22.0
/// `priv_client/handler.rs`, `STREAM_TIMEOUT`, sixty seconds), plus a
/// margin for the tick. Every other end of an ask arrives as a listener
/// event well inside it; this is for the one that never comes (the
/// module note).
pub const REQUEST_HORIZON_MS: u64 = 120_000;

/// The pinned client's bound on a reservation request, restated so the
/// horizon's margin is checked against it.
pub const CRATE_RESERVE_TIMEOUT_MS: u64 = 60_000;

const _: () = assert!(
    REQUEST_HORIZON_MS >= crate::probe_server::MAX_PROFILE_HANDSHAKE_MS + CRATE_RESERVE_TIMEOUT_MS,
    "the request horizon must outlast a handshake and a reservation request"
);

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
    /// How long a `DialPeer`'s direct candidates get before a circuit
    /// route in the book is dialled beside them (`transport/libp2p/
    /// CONNECTIVITY.md` §12, step 9): the profile's
    /// `relay.client.direct_head_start`, 750 ms by default, zero legal
    /// (no head-start), SPIKE-004-tunable and not a wire invariant. It
    /// lives here because only a profile with the relay transport can
    /// dial a circuit at all.
    pub direct_head_start_ms: u64,
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
            direct_head_start_ms: u64::from(config.direct_head_start_ms),
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
            if relay_of(&relay.address)? != relay.peer {
                return Err("relay static_relays: the /p2p/ component does not name the relay");
            }
        }
        Ok(())
    }
}

impl Default for RelayClientSettings {
    /// No static relay, no learning, the manager's defaults, section
    /// 12's head-start.
    fn default() -> Self {
        Self {
            static_relays: Vec::new(),
            use_authorized_identify_relays: false,
            reservations: ReservationConfig::default(),
            direct_head_start_ms: 750,
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

/// One open reservation listener: the relay it reserves on and the
/// address it listens through, so a close can say which address the
/// ask went to.
#[derive(Debug, Clone)]
struct Listening {
    relay: TransportIdentity,
    through: Multiaddr,
}

/// What the driver holds beside the manager: the listener each ask
/// opened, the ones it closed itself, and what the Swarm advertises.
#[derive(Debug)]
pub struct RelayState {
    settings: RelayClientSettings,
    manager: ReservationManager,
    /// The relay each open listener reserves on. One per relay that is
    /// `Requested` or `Active`, so bounded by the manager's candidates;
    /// an entry leaves on the listener's close or at the request
    /// horizon (`a_request_past_the_horizon_is_abandoned_and_failed`).
    listeners: HashMap<ListenerId, Listening>,
    by_relay: BTreeMap<TransportIdentity, ListenerId>,
    /// Listeners this driver removed and whose close has not yet been
    /// reported; each entry leaves on that close, which
    /// `remove_listener` queues unconditionally. Bounded by
    /// `listeners`, from which every entry came.
    released: HashSet<ListenerId>,
    /// Which of a relay's addresses the next ask listens through, so a
    /// relay with several is tried at each in turn. Pruned with the
    /// relay.
    next_address: BTreeMap<TransportIdentity, usize>,
    /// The circuit addresses the Swarm currently advertises on this
    /// driver's account -- the manager's set as of the last sync.
    advertised: BTreeSet<String>,
    /// What came in by the operator's door (ADR-0052 rule 9).
    operator: crate::operator_set::OperatorSet,
    /// This node's own listeners, for rule 3; refreshed before each
    /// dispatch.
    own_listeners: Vec<String>,
    /// Where the learned-relay hook files what it admitted and refused.
    stores: crate::store_refusals::StoreRefusals,
    last_standing: Option<(Standing, usize, usize, usize, usize, usize)>,
}

impl RelayState {
    /// Share the runtime's operator set and store counts with the
    /// learned-relay hook (ADR-0052 rules 8 and 9).
    pub(crate) fn set_boundary(
        &mut self,
        operator: crate::operator_set::OperatorSet,
        stores: crate::store_refusals::StoreRefusals,
    ) {
        self.operator = operator;
        self.stores = stores;
    }

    /// This node's current listeners, for rule 3 at the learned-relay
    /// hook.
    pub(crate) fn set_own_listeners(&mut self, listeners: impl IntoIterator<Item = String>) {
        self.own_listeners.clear();
        self.own_listeners.extend(listeners);
    }

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
            operator: crate::operator_set::OperatorSet::new(),
            own_listeners: Vec::new(),
            stores: crate::store_refusals::StoreRefusals::new(),
            last_standing: None,
        })
    }

    fn jitter_ms(&self) -> u64 {
        let span = self.settings.reservations.retry_min_ms.saturating_add(1);
        // `rand::rng()` since the 0.57 bump took rand to 0.10, where
        // `OsRng` is a fallible `TryRngCore` rather than an infallible
        // `RngCore`. This is retry jitter, not a nonce: the thread RNG
        // is a ChaCha CSPRNG seeded from the OS, and spreading a
        // reconnection herd needs nothing stronger.
        rand::rng().next_u64() % span
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
    abandon_past_horizon(state, swarm, now_ms, out);
    let actions = state.manager.tick(now_ms);
    act(state, swarm, actions, now_ms, out);
    sync(state, swarm, now_ms, out);
}

/// Abandon every ask older than [`REQUEST_HORIZON_MS`]: its listener
/// removed, the relay recorded as failed. The module note says which
/// path needs it.
fn abandon_past_horizon(
    state: &mut RelayState,
    swarm: &mut GatedSwarm,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) {
    let overdue: Vec<TransportIdentity> = state
        .manager
        .relays()
        .filter(|(relay, _)| {
            matches!(
                state.manager.state(relay),
                Some(ReservationState::Requested { since_ms, .. })
                    if now_ms.saturating_sub(*since_ms) >= REQUEST_HORIZON_MS
            )
        })
        .map(|(relay, _)| relay.clone())
        .collect();
    for relay in overdue {
        abandon_listener(state, swarm, &relay);
        closed(
            state,
            &relay,
            Some("no answer within the request horizon".to_owned()),
            now_ms,
            out,
        );
    }
}

/// Remove the listener held for `relay`, if any, remembering its id so
/// its close is consumed rather than read as a loss.
fn abandon_listener(state: &mut RelayState, swarm: &mut GatedSwarm, relay: &TransportIdentity) {
    if let Some(id) = state.by_relay.remove(relay) {
        state.listeners.remove(&id);
        if swarm.remove_listener(id) {
            state.released.insert(id);
        }
    }
}

/// Every relay that is no longer authorized loses its ask: a learned
/// one is forgotten, with its listener and its addresses; a static one
/// stays configured -- the gate refuses its next dial -- but the
/// listener it holds is abandoned and the relay recorded as failed,
/// because a relay de-authorized between its dial and its
/// establishment would otherwise stay `Requested` for the process's
/// life (the module note). Called on a trust change, so the withdrawal
/// precedes the revoked connection's close, and on every tick, so a
/// class that changed by any other route is caught too.
pub(super) fn forget_deauthorized(
    state: &mut RelayState,
    swarm: &mut GatedSwarm,
    trust: &ConnectionManager,
    now_ms: u64,
    out: &mut Vec<SwarmEvent>,
) {
    let stale: Vec<(TransportIdentity, RelaySource)> = state
        .manager
        .relays()
        .filter(|(relay, _)| !authorized(trust.classify(relay)))
        .map(|(relay, source)| (relay.clone(), source))
        .collect();
    if stale.is_empty() {
        return;
    }
    for (relay, source) in stale {
        match source {
            RelaySource::Learned => forget(state, swarm, &relay, out),
            RelaySource::Static => {
                if state.by_relay.contains_key(&relay) {
                    abandon_listener(state, swarm, &relay);
                    closed(
                        state,
                        &relay,
                        Some("the relay is no longer authorized".to_owned()),
                        now_ms,
                        out,
                    );
                }
            }
        }
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
        // EVERYTHING OF A LISTENER THIS DRIVER REMOVED IS ITS OWN: an
        // address the relay reported that was still queued behind the
        // removal -- a relay with several addresses drains one per
        // poll -- would otherwise reach the consumer as an ordinary
        // `Listening` and sit in the runtime's listener table forever.
        // The close, queued behind them, ends the entry.
        Libp2pSwarmEvent::NewListenAddr { listener_id, .. }
        | Libp2pSwarmEvent::ExpiredListenAddr { listener_id, .. }
        | Libp2pSwarmEvent::ListenerError { listener_id, .. }
            if state.released.contains(&listener_id) =>
        {
            RelayHandled::Consumed
        }
        Libp2pSwarmEvent::ListenerClosed { listener_id, .. }
            if state.released.remove(&listener_id) =>
        {
            RelayHandled::Consumed
        }
        Libp2pSwarmEvent::NewListenAddr {
            listener_id,
            address,
        } if state.listeners.contains_key(&listener_id) => {
            let relay = state.listeners[&listener_id].relay.clone();
            accepted(state, &relay, &address, now_ms, out);
            sync(state, swarm, now_ms, out);
            RelayHandled::Consumed
        }
        Libp2pSwarmEvent::ListenerClosed {
            listener_id,
            reason,
            ..
        } if state.listeners.contains_key(&listener_id) => {
            if let Some(Listening { relay, through }) = state.listeners.remove(&listener_id) {
                state.by_relay.remove(&relay);
                let detail = Some(match reason {
                    Ok(()) => format!("the listener closed (asked through {through})"),
                    Err(e) => format!("{e} (asked through {through})"),
                });
                closed(state, &relay, detail, now_ms, out);
                sync(state, swarm, now_ms, out);
            }
            RelayHandled::Consumed
        }
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::RelayClient(event)) => {
            match event {
                ClientEvent::ReservationReqAccepted { .. } => {
                    // The addresses arrive as the listener's; this is
                    // only the crate saying the exchange happened.
                }
                ClientEvent::OutboundCircuitEstablished { .. }
                | ClientEvent::InboundCircuitEstablished { .. } => {
                    // The crate saying a circuit's hop completed. The
                    // circuit is a CONNECTION, and the path it gives the
                    // peer is announced from the open set by
                    // `dialing::path_events` when it establishes -- an
                    // inbound one admitted or refused by the pre-auth and
                    // connection gates like any inbound, under
                    // `RelayCircuit` (step 7); nothing here opens one.
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
        // THE ENFORCEMENT FOR THIS STORE, not hygiene (ADR-0052 rule 8,
        // A 2026-09-25). A learned relay address becomes the EXPLICIT
        // address of the relay client's reservation dial
        // (`libp2p-relay 0.22.0` `priv_client.rs:373-376` and `:419-422`,
        // `.addresses(vec![relay_addr])`), and the root funnel passes an
        // explicit address untouched -- it prunes only what the same
        // dial's `extend_addresses_through_behaviour` adds. So this is the only place a
        // trusted peer's advertised loopback or `/dns4` name is stopped
        // under `use_authorized_identify_relays`.
        if !state.stores.judge(
            crate::store_refusals::store::RELAY_RESERVATIONS,
            &state.operator,
            address,
            state.own_listeners.iter().map(String::as_str),
        ) {
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
    // dialled forever at the one that fails
    // (`the_ask_rotates_through_the_relays_direct_addresses`).
    let slot = state.next_address.entry(relay.clone()).or_insert(0);
    let start = *slot;
    *slot = slot.wrapping_add(1);
    let Some(address) = pick_address(start, addresses) else {
        failed_now(
            state,
            relay,
            "no address of the relay is a direct one",
            now_ms,
            out,
        );
        return;
    };
    let through = circuit_listen_address(&peer_id, &address);
    match swarm.listen_on(through.clone()) {
        Ok(id) => {
            state.listeners.insert(
                id,
                Listening {
                    relay: relay.clone(),
                    through,
                },
            );
            state.by_relay.insert(relay.clone(), id);
        }
        Err(e) => failed_now(state, relay, &e.to_string(), now_ms, out),
    }
}

/// The address the `start`th ask of a relay listens through: its
/// addresses in turn from `start`, skipping any that is itself a
/// circuit or does not parse; `None` when none is direct.
fn pick_address(start: usize, addresses: &[String]) -> Option<Multiaddr> {
    (0..addresses.len())
        .map(|i| &addresses[(start.wrapping_add(i)) % addresses.len()])
        .find_map(|a| {
            let parsed: Multiaddr = a.parse().ok()?;
            (!parsed.iter().any(|p| matches!(p, Protocol::P2pCircuit))).then_some(parsed)
        })
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
    abandon_listener(state, swarm, relay);
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
    abandon_listener(state, swarm, relay);
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
            out.push(SwarmEvent::RelayReservationChanged {
                relay: relay.clone(),
                outcome: RelayReservationOutcome::Accepted,
                addresses: vec![text],
                detail: None,
            });
        }
        Ok(false) => {
            out.push(SwarmEvent::RelayReservationChanged {
                relay: relay.clone(),
                outcome: RelayReservationOutcome::Renewed,
                addresses: vec![text],
                detail: None,
            });
        }
        Err(reason) => refused(relay, Some(text), reason, out),
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
                RelayReservationOutcome::Lost
            } else {
                RelayReservationOutcome::Failed
            };
            out.push(SwarmEvent::RelayReservationChanged {
                relay: relay.clone(),
                outcome,
                addresses,
                detail,
            });
        }
        Err(reason) => refused(relay, None, reason, out),
    }
}

fn refused(
    relay: &TransportIdentity,
    address: Option<String>,
    reason: RefusedRelayReport,
    out: &mut Vec<SwarmEvent>,
) {
    out.push(SwarmEvent::RelayReportRefused {
        relay: relay.clone(),
        address,
        reason,
    });
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
    const R2: &str = "12D3KooWHyNGMf9HTd3Zj6dStdkcc5ycsubW1rEgQSp6k6yfZBoy";

    /// ADR-0052 rule 8 (A 2026-09-25): this store's learn site IS the
    /// enforcement. A learned relay address becomes the EXPLICIT address
    /// of the reservation dial (`libp2p-relay 0.22.0` `priv_client.rs:373-376`
    /// and `:419-422`),
    /// which the root funnel passes untouched, so a trusted relay whose
    /// advertised addresses are a loopback, a metadata-service address
    /// and a `/dns4` name must not become a reservation candidate at all
    /// -- and the same relay advertising a global address must, or the
    /// hook refuses every relay.
    ///
    /// Read from whether the manager HOLDS a candidate, not from the
    /// counts alone: a hook that counted the refusal and learned the
    /// address anyway would pass a test of the counts. (`forget` would
    /// not do: it returns an ACTIVE reservation's addresses, and an idle
    /// candidate's are nowhere public -- the first version of this test
    /// asserted on it and its control caught that it read nothing.)
    #[test]
    fn a_relay_that_advertises_only_refused_addresses_is_never_a_candidate() {
        let keys = libp2p::identity::Keypair::generate_ed25519();
        let peer_id = keys.public().to_peer_id();
        let relay = TransportIdentity::parse(peer_id.to_base58()).expect("canonical");
        let mut manager =
            ConnectionManager::new(interweave_transport_runtime::ConnectionPolicy::default(), 8);
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([relay.clone()]).expect("one peer"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );
        let settings = RelayClientSettings {
            static_relays: Vec::new(),
            use_authorized_identify_relays: true,
            reservations: interweave_transport_runtime::relay::ReservationConfig::default(),
            direct_head_start_ms: 750,
        };
        let info = |addresses: &[&str]| identify::Info {
            public_key: keys.public(),
            protocol_version: "/interweave/id/1.0.0".to_owned(),
            agent_version: "test".to_owned(),
            listen_addrs: addresses
                .iter()
                .map(|a| a.parse().expect("valid"))
                .collect(),
            protocols: vec![libp2p::StreamProtocol::new(HOP_PROTOCOL)],
            observed_addr: "/ip4/8.8.8.8/tcp/1".parse().expect("valid"),
            signed_peer_record: None,
        };

        // ONLY REFUSED ADDRESSES: no candidate.
        let mut state = RelayState::new(&settings).expect("builds");
        let stores = crate::store_refusals::StoreRefusals::new();
        state.set_boundary(crate::operator_set::OperatorSet::new(), stores.clone());
        state.set_own_listeners(Vec::new());
        learn(
            &mut state,
            &peer_id,
            &info(&[
                "/ip4/127.0.0.1/tcp/4001",
                "/ip4/169.254.169.254/tcp/80",
                "/dns4/a-name-the-relay-chose.invalid/tcp/4001",
            ]),
            &manager,
        );
        assert_eq!(
            state.manager.relays().count(),
            0,
            "a relay reachable only at refused addresses must not become a candidate: its \
             reservation dial would carry them EXPLICITLY, which the root funnel does not touch"
        );
        let counts = stores.get(crate::store_refusals::store::RELAY_RESERVATIONS);
        assert_eq!(counts.admitted, 0);
        assert_eq!(counts.refused.get("special_use").copied(), Some(2));
        assert_eq!(counts.refused.get("not_literal").copied(), Some(1));

        // THE CONTROL: the same relay at a global address is a candidate.
        let mut state = RelayState::new(&settings).expect("builds");
        state.set_own_listeners(Vec::new());
        learn(
            &mut state,
            &peer_id,
            &info(&["/ip4/8.8.4.4/tcp/4001"]),
            &manager,
        );
        assert_eq!(
            state.manager.relays().count(),
            1,
            "the control: an admissible address makes the relay a candidate, so the zero \
             above is the hook's doing and not a learn path that never runs"
        );
    }

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
        assert_eq!(
            settings.direct_head_start_ms, 750,
            "section 12's head-start, from the block"
        );
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
        // A hand-built pair whose /p2p/ component names another peer.
        let settings = RelayClientSettings {
            static_relays: vec![StaticRelay {
                peer: TransportIdentity::parse(R1).expect("id"),
                address: format!("/ip4/192.0.2.1/tcp/1/p2p/{R2}"),
            }],
            ..RelayClientSettings::default()
        };
        assert!(
            settings
                .validate()
                .is_err_and(|e| e.contains("does not name the relay"))
        );
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

    /// A `GatedSwarm` with the relay client and its transport composed,
    /// as the runtime builds one, for the seam tests: nothing is
    /// polled, so a listener stays where the driver put it.
    fn swarm_with_relay_client(manager: &ConnectionManager) -> GatedSwarm {
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let attribution = DialAttribution::default();
        let outbound = crate::outbound_gate::OutboundAdmission::new(
            manager.handle(),
            crate::outbound_gate::InFlightTickets::default(),
            attribution.clone(),
            tokio::time::Instant::now(),
        );
        let class_policy = manager.handle();
        let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .expect("tcp")
            .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)
            .expect("relay client")
            .with_behaviour(|key, client| {
                crate::behaviour::SubstrateBehaviour::new(
                    key,
                    interweave_transport_runtime::preauth::PreAuthLimits::default(),
                    outbound,
                    crate::behaviour::Configured {
                        relay_client: build_behaviour(client, attribution, class_policy.clone()),
                        ..crate::behaviour::Configured::default()
                    },
                    class_policy,
                )
                // As production builds it: the root funnel around the
                // whole composite (ADR-0052 A 2026-09-25 D1).
                .map(|b| {
                    crate::root_funnel::RootFunnel::new(b, crate::operator_set::OperatorSet::new())
                })
                .map_err(Box::<dyn std::error::Error + Send + Sync>::from)
            })
            .expect("behaviour")
            .build();
        GatedSwarm::new(swarm)
    }

    fn ident(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("a canonical identity")
    }

    /// Trust that holds `relay` as infrastructure only.
    fn trusting(relay: &TransportIdentity) -> ConnectionManager {
        let mut m =
            ConnectionManager::new(interweave_transport_runtime::ConnectionPolicy::default(), 8);
        let _ = m.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new(std::iter::empty()).expect("empty"),
                interweave_trust_api::InfrastructureSet::new([relay.clone()]).expect("one"),
            ),
            &[],
        );
        m
    }

    fn nobody() -> ConnectionManager {
        ConnectionManager::new(interweave_transport_runtime::ConnectionPolicy::default(), 8)
    }

    fn one_static(relay: &str, address: &str) -> RelayClientSettings {
        RelayClientSettings {
            static_relays: vec![StaticRelay {
                peer: ident(relay),
                address: address.to_owned(),
            }],
            ..RelayClientSettings::default()
        }
    }

    fn outcomes(events: &[SwarmEvent]) -> Vec<(RelayReservationOutcome, Option<String>)> {
        events
            .iter()
            .filter_map(|e| match e {
                SwarmEvent::RelayReservationChanged {
                    outcome, detail, ..
                } => Some((*outcome, detail.clone())),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_request_past_the_horizon_is_abandoned_and_failed() {
        // The one end of an ask that no listener event reports (the
        // module note): the ask is Requested, nothing answers, and at
        // the horizon the driver abandons the listener and backs the
        // relay off. Before the horizon, nothing moves -- the control.
        let relay = ident(R1);
        let trust = trusting(&relay);
        let mut swarm = swarm_with_relay_client(&trust);
        let mut state = RelayState::new(&one_static(R1, &format!("/ip4/127.0.0.1/tcp/1/p2p/{R1}")))
            .expect("valid");
        let mut out = Vec::new();
        reconcile(&mut state, &mut swarm, &trust, 0, &mut out);
        assert!(matches!(
            state.manager.state(&relay),
            Some(ReservationState::Requested { since_ms: 0, .. })
        ));
        assert_eq!(state.listeners.len(), 1, "a listener was opened");
        assert!(outcomes(&out).is_empty());

        reconcile(
            &mut state,
            &mut swarm,
            &trust,
            REQUEST_HORIZON_MS - 1,
            &mut out,
        );
        assert!(outcomes(&out).is_empty(), "nothing before the horizon");
        assert_eq!(state.listeners.len(), 1);

        reconcile(&mut state, &mut swarm, &trust, REQUEST_HORIZON_MS, &mut out);
        assert_eq!(
            outcomes(&out),
            vec![(
                RelayReservationOutcome::Failed,
                Some("no answer within the request horizon".to_owned())
            )]
        );
        assert!(matches!(
            state.manager.state(&relay),
            Some(ReservationState::Backoff { attempts: 1, .. })
        ));
        assert!(state.listeners.is_empty(), "the listener is gone");
        assert_eq!(
            state.released.len(),
            1,
            "and its close, when it arrives, is the driver's to consume"
        );
    }

    #[tokio::test]
    async fn a_static_relay_that_lost_its_authorization_has_its_ask_abandoned_and_stays_configured()
    {
        let relay = ident(R1);
        let trust = trusting(&relay);
        let mut swarm = swarm_with_relay_client(&trust);
        let mut state = RelayState::new(&one_static(R1, &format!("/ip4/127.0.0.1/tcp/1/p2p/{R1}")))
            .expect("valid");
        let mut out = Vec::new();
        reconcile(&mut state, &mut swarm, &trust, 0, &mut out);
        assert_eq!(state.listeners.len(), 1);
        // Still authorized: nothing happens on the sweep -- the control.
        forget_deauthorized(&mut state, &mut swarm, &trust, 1, &mut out);
        assert!(outcomes(&out).is_empty());
        assert_eq!(state.listeners.len(), 1);
        // De-authorized: the ask is abandoned, the relay backs off, and
        // it is still a candidate -- static relays are the operator's.
        forget_deauthorized(&mut state, &mut swarm, &nobody(), 2, &mut out);
        assert_eq!(
            outcomes(&out),
            vec![(
                RelayReservationOutcome::Failed,
                Some("the relay is no longer authorized".to_owned())
            )]
        );
        assert!(state.listeners.is_empty());
        assert!(matches!(
            state.manager.state(&relay),
            Some(ReservationState::Backoff { .. })
        ));
        assert_eq!(state.manager.candidates(), 1, "still configured");
        assert_eq!(state.manager.source(&relay), Some(RelaySource::Static));
    }

    #[tokio::test]
    async fn every_event_of_a_released_listener_is_consumed_until_its_close() {
        // A relay with several addresses drains one NewListenAddr per
        // poll; a release between two of them must not let the second
        // reach the consumer as an ordinary listener, nor the runtime's
        // listener table.
        let relay = ident(R1);
        let trust = trusting(&relay);
        let mut swarm = swarm_with_relay_client(&trust);
        let mut state = RelayState::new(&one_static(R1, &format!("/ip4/127.0.0.1/tcp/1/p2p/{R1}")))
            .expect("valid");
        let mut out = Vec::new();
        reconcile(&mut state, &mut swarm, &trust, 0, &mut out);
        let id = *state.listeners.keys().next().expect("a listener");
        let circuit: Multiaddr = format!("/ip4/127.0.0.1/tcp/1/p2p/{R1}/p2p-circuit/p2p/{R1}")
            .parse()
            .expect("addr");
        // The first address lands: accepted and advertised.
        let handled = handle_relay(
            Libp2pSwarmEvent::NewListenAddr {
                listener_id: id,
                address: circuit.clone(),
            },
            &mut swarm,
            &mut state,
            &trust,
            1,
            &mut out,
        );
        assert!(matches!(handled, RelayHandled::Consumed));
        assert_eq!(state.manager.advertised(), vec![circuit.to_string()]);
        // The driver releases the listener (a forget of a de-authorized
        // learned relay would do the same through abandon_listener).
        abandon_listener(&mut state, &mut swarm, &relay);
        assert!(state.released.contains(&id));
        // The second address, queued behind the removal, arrives.
        let handled = handle_relay(
            Libp2pSwarmEvent::NewListenAddr {
                listener_id: id,
                address: format!("/ip6/::1/tcp/1/p2p/{R1}/p2p-circuit/p2p/{R1}")
                    .parse()
                    .expect("addr"),
            },
            &mut swarm,
            &mut state,
            &trust,
            2,
            &mut out,
        );
        assert!(
            matches!(handled, RelayHandled::Consumed),
            "consumed, not passed"
        );
        assert_eq!(
            state.manager.advertised(),
            vec![circuit.to_string()],
            "and not folded into the manager either"
        );
        // Then the close, which ends the entry.
        let handled = handle_relay(
            Libp2pSwarmEvent::ListenerClosed {
                listener_id: id,
                addresses: vec![],
                reason: Ok(()),
            },
            &mut swarm,
            &mut state,
            &trust,
            3,
            &mut out,
        );
        assert!(matches!(handled, RelayHandled::Consumed));
        assert!(state.released.is_empty(), "the entry left on the close");
        // THE CONTROL: a listener the driver never held passes through.
        let stranger = libp2p::core::transport::ListenerId::next();
        let handled = handle_relay(
            Libp2pSwarmEvent::NewListenAddr {
                listener_id: stranger,
                address: "/ip4/127.0.0.1/tcp/9".parse().expect("addr"),
            },
            &mut swarm,
            &mut state,
            &trust,
            4,
            &mut out,
        );
        assert!(matches!(handled, RelayHandled::Passed(_)));
    }

    #[tokio::test]
    async fn the_swarms_external_set_follows_the_managers_advertised_set() {
        // RELAY.md section 5: added on the acceptance, gone on the loss,
        // in the same turn -- read from the Swarm itself, since a peer's
        // Identify also carries the listener's own addresses and cannot
        // tell the two apart.
        let relay = ident(R1);
        let trust = trusting(&relay);
        let mut swarm = swarm_with_relay_client(&trust);
        let mut state = RelayState::new(&one_static(R1, &format!("/ip4/127.0.0.1/tcp/1/p2p/{R1}")))
            .expect("valid");
        let mut out = Vec::new();
        reconcile(&mut state, &mut swarm, &trust, 0, &mut out);
        let id = *state.listeners.keys().next().expect("a listener");
        let circuit: Multiaddr = format!("/ip4/127.0.0.1/tcp/1/p2p/{R1}/p2p-circuit/p2p/{R1}")
            .parse()
            .expect("addr");
        assert_eq!(swarm.external_addresses().count(), 0);
        let _ = handle_relay(
            Libp2pSwarmEvent::NewListenAddr {
                listener_id: id,
                address: circuit.clone(),
            },
            &mut swarm,
            &mut state,
            &trust,
            1,
            &mut out,
        );
        assert_eq!(
            swarm.external_addresses().cloned().collect::<Vec<_>>(),
            vec![circuit.clone()],
            "advertised on the acceptance"
        );
        let _ = handle_relay(
            Libp2pSwarmEvent::ListenerClosed {
                listener_id: id,
                addresses: vec![circuit],
                reason: Err(std::io::Error::other("the relay went away")),
            },
            &mut swarm,
            &mut state,
            &trust,
            2,
            &mut out,
        );
        assert_eq!(
            swarm.external_addresses().count(),
            0,
            "withdrawn on the loss, in the same call"
        );
        assert!(matches!(
            outcomes(&out).last(),
            Some((RelayReservationOutcome::Lost, Some(d))) if d.contains("went away")
        ));
    }

    #[tokio::test]
    async fn a_second_ask_listens_through_the_relays_next_address() {
        // `pick_address` rotates from a start the driver advances; this
        // pins the advance. Two asks of a two-address relay open two
        // listeners whose addresses differ, and the third ask wraps.
        let relay = ident(R1);
        let trust = trusting(&relay);
        let mut swarm = swarm_with_relay_client(&trust);
        let settings = RelayClientSettings {
            static_relays: vec![
                StaticRelay {
                    peer: relay.clone(),
                    address: format!("/ip4/127.0.0.1/tcp/1/p2p/{R1}"),
                },
                StaticRelay {
                    peer: relay.clone(),
                    address: format!("/ip4/127.0.0.1/tcp/2/p2p/{R1}"),
                },
            ],
            reservations: ReservationConfig {
                retry_min_ms: 1,
                retry_max_ms: 1,
                ..ReservationConfig::default()
            },
            ..RelayClientSettings::default()
        };
        let mut state = RelayState::new(&settings).expect("valid");
        let mut out = Vec::new();
        let mut listened = Vec::new();
        for now in [0_u64, 10, 20] {
            reconcile(&mut state, &mut swarm, &trust, now, &mut out);
            let (id, listening) = state.listeners.iter().next().expect("a listener");
            listened.push(listening.through.clone());
            // The ask fails; the relay backs off for 1 ms and is asked
            // again on the next reconcile.
            let id = *id;
            let _ = handle_relay(
                Libp2pSwarmEvent::ListenerClosed {
                    listener_id: id,
                    addresses: vec![],
                    reason: Ok(()),
                },
                &mut swarm,
                &mut state,
                &trust,
                now + 1,
                &mut out,
            );
        }
        assert_eq!(
            state.next_address[&relay], 3,
            "the slot advanced once per ask"
        );
        assert_ne!(
            listened[0], listened[1],
            "the second ask used the other address"
        );
        assert_eq!(listened[0], listened[2], "and the third wrapped");
    }

    #[test]
    fn the_ask_rotates_through_the_relays_direct_addresses() {
        let addresses = vec![
            "/ip4/192.0.2.1/tcp/1".to_owned(),
            format!("/ip4/192.0.2.9/tcp/1/p2p/{R1}/p2p-circuit"),
            "not an address".to_owned(),
            "/ip4/192.0.2.2/tcp/1".to_owned(),
        ];
        let a1: Multiaddr = "/ip4/192.0.2.1/tcp/1".parse().expect("addr");
        let a2: Multiaddr = "/ip4/192.0.2.2/tcp/1".parse().expect("addr");
        assert_eq!(pick_address(0, &addresses), Some(a1.clone()));
        assert_eq!(
            pick_address(1, &addresses),
            Some(a2.clone()),
            "the circuit and the unparsable are skipped"
        );
        assert_eq!(pick_address(2, &addresses), Some(a2.clone()));
        assert_eq!(pick_address(3, &addresses), Some(a2.clone()));
        assert_eq!(pick_address(4, &addresses), Some(a1), "and it wraps");
        assert_eq!(
            pick_address(usize::MAX, &addresses),
            Some(a2),
            "and a counter that wrapped still picks: usize::MAX is the last slot"
        );
        assert_eq!(
            pick_address(0, &[format!("/ip4/192.0.2.9/tcp/1/p2p/{R1}/p2p-circuit")]),
            None,
            "a relay reachable only through a circuit is not asked"
        );
        assert_eq!(pick_address(0, &[]), None);
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
}
