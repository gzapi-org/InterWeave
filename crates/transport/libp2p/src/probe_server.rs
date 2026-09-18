// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `AUTONAT.md` §7 at the behaviour boundary: what the AutoNAT v2
//! server may dial back to, and how many probes it answers.
//!
//! # What the pinned server does not do
//!
//! SPIKE-004 F2: `handle_request_internal` (`v2/server/handler/
//! dial_request.rs`) pops the last address the client supplied and
//! dials it. There is no literal-IP requirement, no candidate-IP-equals-
//! observed-source-IP check and no address-class filter anywhere on
//! that path, and the only budget is a per-connection map of ten
//! requests with a ten-second timeout. §7 requires all four checks
//! before any dial is admitted and three budgets besides, so this
//! wrapper carries them.
//!
//! # Where each check runs, and why not the PENDING hook alone
//!
//! The dial-back target check runs in
//! [`ProbeServer::handle_pending_outbound_connection`] -- before any
//! socket, which is the entirety of what an SSRF check exists to
//! prevent; the established hook runs after the target was contacted
//! (F2, corrected on review). The wrapper cannot run it earlier: a
//! dial's address is `pub(crate)` inside `DialOpts`, so `poll` sees a
//! `ToSwarm::Dial` it can pair with a request but cannot read. One
//! consequence binds the composed behaviour's FIELD ORDER, and
//! `behaviour.rs` says so where the fields are declared: a pending
//! hook denial after the outbound gate's own hook has run would leave
//! the ticket the gate deposited with no settlement, because the Swarm
//! discards a behaviour dial's synchronous failure with no
//! `OutgoingConnectionError` (SPIKE-004: "surfaces as nothing at
//! all"). The server field therefore sits BEFORE the gate, so a
//! refused dial-back is refused before a ticket exists. Pinned over a
//! real Swarm by `a_refused_dial_back_leaves_no_ticket_and_no_note` in
//! `runtime/autonat_server_driver.rs`.
//!
//! The budgets run one step earlier, in
//! [`ProbeServer::on_connection_handler_event`], where the request
//! arrives as the handler's `DialBackCommand`. A request over budget is
//! not forwarded: the command's answer channel drops, the crate answers
//! `E_INTERNAL_ERROR`, and the client's crate maps that to `Error::Io`
//! -- a TRANSIENT outcome that returns the candidate to `Untested`
//! rather than counting against the address. A target refusal, by
//! contrast, fails the dial the crate already issued, which it answers
//! `E_DIAL_ERROR`, and the client reads as `AddressNotReachable`: the
//! address the client asked to be confirmed is one this server will
//! not confirm, which is what §7 means by "a probe failure". The two
//! refusals reach the client as two different classes on purpose.
//!
//! # Bounds
//!
//! Every collection here is bounded, and each bound is named on the
//! field. The one that is derived rather than declared: the per-client
//! rate map holds an entry only for a client with a probe start inside
//! the last minute, and the global window admits at most
//! `max_probes_global_per_minute` starts in that minute, so the map
//! never exceeds the global budget. Pinned by
//! `the_client_rate_map_is_bounded_by_the_global_budget`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::IpAddr;
use std::task::{Context, Poll};

use either::Either;
use libp2p::autonat::v2::server::{Behaviour as Server, Event as ServerEvent};
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
};

use interweave_transport_runtime::reachability::is_probeable_address;

/// One minute, the window every §7 rate is stated over.
pub const RATE_WINDOW_MS: u64 = 60_000;

/// How long a dial-back with no outcome stays counted as in flight.
///
/// The Swarm's connection timeout answers every dial within the
/// handshake timeout (10 s), so this fires only if an outcome is lost;
/// it keeps `max_concurrent_probes` from being spent by a dial the
/// Swarm forgot rather than by one it is making. A constant and not a
/// knob: the configured `timeout` step 3 found nothing for on the
/// client side describes nothing here either, and was removed.
pub const IN_FLIGHT_HORIZON_MS: u64 = 30_000;

/// Refusal events queued between polls, past which a refusal is still
/// COUNTED but not emitted as an event.
///
/// A flood of requests is the case this exists for: each is refused in
/// constant time, and the counter says how many; the events are for an
/// operator reading a log, who does not need ten thousand of them.
pub const MAX_PENDING_EVENTS: usize = 64;

/// §7's three budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeBudgets {
    /// Dial-backs in flight at once.
    pub max_concurrent: usize,
    /// Probe starts one client may make per [`RATE_WINDOW_MS`].
    pub per_client_per_minute: usize,
    /// Probe starts across all clients per [`RATE_WINDOW_MS`].
    pub global_per_minute: usize,
}

impl Default for ProbeBudgets {
    /// §7's defaults: 8, 2 and 60.
    fn default() -> Self {
        Self {
            max_concurrent: 8,
            per_client_per_minute: 2,
            global_per_minute: 60,
        }
    }
}

/// Why a probe was not served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProbeRefusal {
    /// `max_concurrent` dial-backs are already in flight.
    ConcurrentProbes,
    /// This client already started `per_client_per_minute` probes in
    /// the window.
    ClientRate,
    /// `global_per_minute` probes already started in the window.
    GlobalRate,
    /// The dial the crate issued carried no address.
    NoAddress,
    /// The candidate names no literal IP: a DNS name, a circuit, or
    /// nothing this server will resolve on a client's behalf.
    NotLiteralIp,
    /// The candidate's IP is not the observed source IP of the
    /// probing connection.
    SourceMismatch,
    /// The probing connection closed before the dial was decided, so
    /// there is no observed source to compare against.
    SourceUnknown,
    /// Loopback, unspecified, multicast, link-local, private, ULA or
    /// another special-use destination.
    NotGlobal,
    /// A dial the crate issued that no forwarded request accounts for.
    /// Fails closed: a future crate that dials for another reason is
    /// refused here rather than admitted unexamined.
    UnexpectedDial,
}

impl ProbeRefusal {
    /// Every variant, for the counter table and the tests that walk it.
    pub const ALL: [Self; 9] = [
        Self::ConcurrentProbes,
        Self::ClientRate,
        Self::GlobalRate,
        Self::NoAddress,
        Self::NotLiteralIp,
        Self::SourceMismatch,
        Self::SourceUnknown,
        Self::NotGlobal,
        Self::UnexpectedDial,
    ];

    /// The `outcome` label under `autonat_server_probes_total`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ConcurrentProbes => "refused_concurrent_probes",
            Self::ClientRate => "refused_client_rate",
            Self::GlobalRate => "refused_global_rate",
            Self::NoAddress => "refused_no_address",
            Self::NotLiteralIp => "refused_not_literal_ip",
            Self::SourceMismatch => "refused_source_mismatch",
            Self::SourceUnknown => "refused_source_unknown",
            Self::NotGlobal => "refused_not_global",
            Self::UnexpectedDial => "refused_unexpected_dial",
        }
    }
}

/// What a served probe came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServedOutcome {
    /// The dial-back reached the address and the nonce came back.
    Ok,
    /// The dial-back failed, or the exchange did.
    Failed,
}

/// What the wrapper reports upward, in place of the crate's event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeServerEvent {
    /// A probe the crate finished, however it came out.
    Served {
        /// The peer that asked.
        client: PeerId,
        /// The address the crate dialled back to.
        address: Multiaddr,
        /// How the exchange came out.
        outcome: ServedOutcome,
        /// Bytes the client sent as dial data before the dial-back.
        data_amount: usize,
    },
    /// A probe this wrapper refused. The address is known only for a
    /// target refusal; a budget refusal happens before the crate
    /// reveals it.
    Refused {
        /// The peer that asked.
        client: PeerId,
        /// The address, when the refusal came after the crate named it.
        address: Option<Multiaddr>,
        /// Which rule refused it.
        reason: ProbeRefusal,
    },
}

/// Per-variant refusal counts, and the served counts beside them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeCounters {
    /// Probes the crate finished with the nonce returned.
    pub served_ok: u64,
    /// Probes the crate finished any other way.
    pub served_failed: u64,
    refused: BTreeMap<ProbeRefusal, u64>,
    /// Refusals counted but not emitted, past [`MAX_PENDING_EVENTS`].
    pub events_dropped: u64,
}

impl ProbeCounters {
    /// How many times `reason` refused a probe.
    #[must_use]
    pub fn refused(&self, reason: ProbeRefusal) -> u64 {
        self.refused.get(&reason).copied().unwrap_or(0)
    }

    /// Every refusal, whatever the reason.
    #[must_use]
    pub fn refused_total(&self) -> u64 {
        self.refused.values().sum()
    }
}

/// A dial-back the crate issued and the Swarm has not yet answered.
#[derive(Debug, Clone, Copy)]
struct InFlight {
    client: PeerId,
    /// The connection the request arrived on, whose observed source
    /// the target must match.
    request: ConnectionId,
    started_ms: u64,
}

/// The vendored AutoNAT v2 server behind §7's target check and budgets.
pub struct ProbeServer {
    inner: Server,
    budgets: ProbeBudgets,
    /// The driver's clock, advanced by [`Self::tick`]. Every window and
    /// horizon here is read against it, so the wrapper needs no clock
    /// of its own and a test can move time.
    clock_ms: u64,
    /// The observed source of every live inbound connection, by id.
    ///
    /// BOUNDED by the Swarm's connection ceiling; an entry is written
    /// at the established inbound hook and removed on close.
    sources: HashMap<ConnectionId, (PeerId, IpAddr)>,
    /// Requests forwarded to the crate whose `ToSwarm::Dial` has not
    /// yet been seen, in arrival order. The crate pushes the dial in
    /// the same call that receives the command, so `poll` pairs each
    /// dial with the front of this queue and checks the peer matches.
    ///
    /// BOUNDED by `max_concurrent`: a request past that budget is not
    /// forwarded, and a forwarded one is counted against it from here.
    awaiting_dial: VecDeque<InFlight>,
    /// Dial-backs the Swarm is making, by the dial's connection id.
    ///
    /// BOUNDED by `max_concurrent` (with `awaiting_dial`, together),
    /// drained on `DialFailure`, on establishment, and by the horizon.
    in_flight: HashMap<ConnectionId, InFlight>,
    /// Probe starts per client inside the window, oldest first.
    ///
    /// BOUNDED: each deque holds at most `per_client_per_minute`
    /// starts, and the map holds at most `global_per_minute` clients
    /// (module note). Pruned on every start and every tick.
    per_client: BTreeMap<PeerId, VecDeque<u64>>,
    /// Probe starts across all clients inside the window.
    ///
    /// BOUNDED at `global_per_minute` entries.
    global: VecDeque<u64>,
    /// Events for the driver, drained one per `poll`.
    ///
    /// BOUNDED at [`MAX_PENDING_EVENTS`]; a refusal past that is
    /// counted in `counters.events_dropped` and not queued.
    pending: VecDeque<ProbeServerEvent>,
    counters: ProbeCounters,
}

impl ProbeServer {
    /// Wrap `inner` under `budgets`.
    #[must_use]
    pub fn new(inner: Server, budgets: ProbeBudgets) -> Self {
        Self {
            inner,
            budgets,
            clock_ms: 0,
            sources: HashMap::new(),
            awaiting_dial: VecDeque::new(),
            in_flight: HashMap::new(),
            per_client: BTreeMap::new(),
            global: VecDeque::new(),
            pending: VecDeque::new(),
            counters: ProbeCounters::default(),
        }
    }

    /// The wrapped behaviour, for the composed behaviour's own use.
    pub fn inner_mut(&mut self) -> &mut Server {
        &mut self.inner
    }

    /// The budgets in force.
    #[must_use]
    pub const fn budgets(&self) -> ProbeBudgets {
        self.budgets
    }

    /// The counters so far.
    #[must_use]
    pub const fn counters(&self) -> &ProbeCounters {
        &self.counters
    }

    /// Dial-backs currently counted against `max_concurrent`.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.awaiting_dial.len() + self.in_flight.len()
    }

    /// Clients with a probe start inside the window.
    #[must_use]
    pub fn rated_clients(&self) -> usize {
        self.per_client.len()
    }

    /// Advance the clock: prune the rate windows and release any
    /// dial-back past the horizon.
    pub fn tick(&mut self, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        self.prune_windows();
        let horizon = self.clock_ms.saturating_sub(IN_FLIGHT_HORIZON_MS);
        self.in_flight.retain(|_, f| f.started_ms >= horizon);
        self.awaiting_dial.retain(|f| f.started_ms >= horizon);
    }

    fn prune_windows(&mut self) {
        let floor = self.clock_ms.saturating_sub(RATE_WINDOW_MS);
        while self.global.front().is_some_and(|t| *t <= floor) {
            self.global.pop_front();
        }
        self.per_client.retain(|_, starts| {
            while starts.front().is_some_and(|t| *t <= floor) {
                starts.pop_front();
            }
            !starts.is_empty()
        });
    }

    fn refuse(&mut self, client: PeerId, address: Option<Multiaddr>, reason: ProbeRefusal) {
        *self.counters.refused.entry(reason).or_insert(0) += 1;
        if self.pending.len() >= MAX_PENDING_EVENTS {
            self.counters.events_dropped += 1;
            return;
        }
        self.pending.push_back(ProbeServerEvent::Refused {
            client,
            address,
            reason,
        });
    }

    /// Charge a probe start against all three budgets, or say which
    /// one refuses it. Charged only when every budget admits it, so a
    /// refusal spends nothing.
    fn admit_start(&mut self, client: PeerId) -> Result<(), ProbeRefusal> {
        self.prune_windows();
        if self.in_flight() >= self.budgets.max_concurrent {
            return Err(ProbeRefusal::ConcurrentProbes);
        }
        let mine = self.per_client.get(&client).map_or(0, VecDeque::len);
        if mine >= self.budgets.per_client_per_minute {
            return Err(ProbeRefusal::ClientRate);
        }
        if self.global.len() >= self.budgets.global_per_minute {
            return Err(ProbeRefusal::GlobalRate);
        }
        self.global.push_back(self.clock_ms);
        self.per_client
            .entry(client)
            .or_default()
            .push_back(self.clock_ms);
        Ok(())
    }

    /// §7's target rule for one dial-back, given the request it answers.
    fn check_target(&self, flight: &InFlight, addresses: &[Multiaddr]) -> Result<(), ProbeRefusal> {
        if addresses.is_empty() {
            return Err(ProbeRefusal::NoAddress);
        }
        let Some((_, source)) = self.sources.get(&flight.request) else {
            return Err(ProbeRefusal::SourceUnknown);
        };
        for address in addresses {
            let Some(ip) = literal_ip(address) else {
                return Err(ProbeRefusal::NotLiteralIp);
            };
            if ip != *source {
                return Err(ProbeRefusal::SourceMismatch);
            }
            if !is_probeable_address(&address.to_string()) {
                return Err(ProbeRefusal::NotGlobal);
            }
        }
        Ok(())
    }
}

/// The IP a multiaddr names literally in its first component, if any.
fn literal_ip(address: &Multiaddr) -> Option<IpAddr> {
    match address.iter().next()? {
        Protocol::Ip4(a) => Some(IpAddr::V4(a)),
        Protocol::Ip6(a) => Some(IpAddr::V6(a)),
        _ => None,
    }
}

impl NetworkBehaviour for ProbeServer {
    type ConnectionHandler = <Server as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = ProbeServerEvent;

    /// Record the observed source, then let the crate install its
    /// request handler.
    fn handle_established_inbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        if let Some(ip) = literal_ip(remote) {
            self.sources.insert(id, (peer, ip));
        }
        self.inner
            .handle_established_inbound_connection(id, peer, local, remote)
    }

    fn handle_established_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role: Endpoint,
        port: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // The dial-back reached its target: it leaves the in-flight
        // count here, and the handler the crate installs finishes the
        // exchange under the crate's own ten-second bound.
        self.in_flight.remove(&id);
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

    /// §7's dial-back target restriction, before any socket.
    ///
    /// Only a dial this wrapper paired with a request is judged; every
    /// other dial in the Swarm passes through to the crate untouched,
    /// which returns nothing for it.
    ///
    /// # Errors
    /// [`ConnectionDenied`] carrying the [`ProbeRefusal`], which the
    /// Swarm reports to the crate as `DialFailure` and the crate answers
    /// `E_DIAL_ERROR`.
    fn handle_pending_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: Option<PeerId>,
        addresses: &[Multiaddr],
        role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if let Some(flight) = self.in_flight.get(&id).copied() {
            if let Err(reason) = self.check_target(&flight, addresses) {
                self.in_flight.remove(&id);
                self.refuse(flight.client, addresses.first().cloned(), reason);
                return Err(ConnectionDenied::new(RefusedDialBack(reason)));
            }
        }
        self.inner
            .handle_pending_outbound_connection(id, peer, addresses, role)
    }

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        match &event {
            FromSwarm::ConnectionClosed(closed) => {
                self.sources.remove(&closed.connection_id);
            }
            FromSwarm::DialFailure(failure) => {
                self.in_flight.remove(&failure.connection_id);
            }
            _ => {}
        }
        self.inner.on_swarm_event(event);
    }

    /// The budgets, at the moment a request asks for a dial-back.
    ///
    /// A request over budget is dropped here and never reaches the
    /// crate: its answer channel closes, and the crate answers the
    /// client `E_INTERNAL_ERROR` (module note on why that class).
    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        if let Either::Right(Either::Left(_)) = &event {
            match self.admit_start(peer) {
                Ok(()) => self.awaiting_dial.push_back(InFlight {
                    client: peer,
                    request: id,
                    started_ms: self.clock_ms,
                }),
                Err(reason) => {
                    self.refuse(peer, None, reason);
                    return;
                }
            }
        }
        self.inner.on_connection_handler_event(peer, id, event);
    }

    /// Pair each dial the crate issues with the request it answers, and
    /// report this wrapper's events in place of the crate's.
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.pending.pop_front() {
            return Poll::Ready(ToSwarm::GenerateEvent(event));
        }
        loop {
            match self.inner.poll(cx) {
                Poll::Ready(ToSwarm::Dial { opts }) => {
                    let id = opts.connection_id();
                    let peer = opts.get_peer_id();
                    match self.awaiting_dial.pop_front() {
                        Some(flight) if Some(flight.client) == peer => {
                            self.in_flight.insert(id, flight);
                            return Poll::Ready(ToSwarm::Dial { opts });
                        }
                        front => {
                            // A dial for someone other than the request
                            // at the front, or with no request at all:
                            // the pairing is broken, so neither is
                            // trusted. The request's slot is released
                            // and the dial refused -- FED BACK to the
                            // crate as a failure, so the request it
                            // holds open is answered rather than left
                            // for the crate's timeout.
                            match peer.or(front.map(|f| f.client)) {
                                Some(client) => {
                                    self.refuse(client, None, ProbeRefusal::UnexpectedDial);
                                }
                                None => {
                                    *self
                                        .counters
                                        .refused
                                        .entry(ProbeRefusal::UnexpectedDial)
                                        .or_insert(0) += 1;
                                }
                            }
                            self.fail_dial(id, peer);
                        }
                    }
                }
                Poll::Ready(other) => {
                    return Poll::Ready(other.map_out(|served| self.served(served)));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl ProbeServer {
    /// Tell the crate a dial it issued has failed, without the Swarm
    /// ever having seen it.
    fn fail_dial(&mut self, id: ConnectionId, peer: Option<PeerId>) {
        let error = libp2p::swarm::DialError::Denied {
            cause: ConnectionDenied::new(RefusedDialBack(ProbeRefusal::UnexpectedDial)),
        };
        self.inner
            .on_swarm_event(FromSwarm::DialFailure(libp2p::swarm::DialFailure {
                peer_id: peer,
                error: &error,
                connection_id: id,
            }));
    }

    fn served(&mut self, event: ServerEvent) -> ProbeServerEvent {
        let outcome = if event.result.is_ok() {
            self.counters.served_ok += 1;
            ServedOutcome::Ok
        } else {
            self.counters.served_failed += 1;
            ServedOutcome::Failed
        };
        ProbeServerEvent::Served {
            client: event.client,
            address: event.tested_addr,
            outcome,
            data_amount: event.data_amount,
        }
    }
}

/// The cause a refused dial-back carries through `ConnectionDenied`.
#[derive(Debug)]
pub struct RefusedDialBack(pub ProbeRefusal);

impl std::fmt::Display for RefusedDialBack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "autonat dial-back refused: {}", self.0.label())
    }
}

impl std::error::Error for RefusedDialBack {}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    fn addr(s: &str) -> Multiaddr {
        s.parse().expect("a literal")
    }

    fn server(budgets: ProbeBudgets) -> ProbeServer {
        ProbeServer::new(Server::default(), budgets)
    }

    /// A server holding one request connection from `source`, with one
    /// dial-back in flight for it under `dial`.
    fn with_request(source: &str, dial: ConnectionId) -> (ProbeServer, PeerId) {
        let mut s = server(ProbeBudgets::default());
        let client = PeerId::random();
        let request = ConnectionId::new_unchecked(1);
        let _ = s.handle_established_inbound_connection(
            request,
            client,
            &addr("/ip4/0.0.0.0/tcp/1"),
            &addr(source),
        );
        s.in_flight.insert(
            dial,
            InFlight {
                client,
                request,
                started_ms: 0,
            },
        );
        (s, client)
    }

    fn judge(s: &mut ProbeServer, dial: ConnectionId, target: &[Multiaddr]) -> Option<ProbeRefusal> {
        let before = s.counters.refused_total();
        let outcome = s.handle_pending_outbound_connection(dial, None, target, Endpoint::Dialer);
        match outcome {
            Ok(_) => {
                assert_eq!(s.counters.refused_total(), before, "an admitted dial counts nothing");
                None
            }
            Err(denied) => {
                assert_eq!(s.counters.refused_total(), before + 1, "every refusal is counted");
                let reason = denied
                    .downcast::<RefusedDialBack>()
                    .expect("this wrapper's own cause")
                    .0;
                assert!(
                    matches!(s.pending.back(), Some(ProbeServerEvent::Refused { reason: r, .. }) if *r == reason),
                    "and reported with the same reason"
                );
                Some(reason)
            }
        }
    }

    #[test]
    fn the_target_rule_admits_the_source_itself_and_refuses_everything_else_by_name() {
        // §7, one clause per row: literal IP, equal to the observed
        // source, globally routable. The positive control is a public
        // source asking to be dialled back at itself.
        let dial = ConnectionId::new_unchecked(7);
        let (mut s, _) = with_request("/ip4/8.8.8.8/tcp/4001", dial);
        assert_eq!(judge(&mut s, dial, &[addr("/ip4/8.8.8.8/tcp/4001")]), None);
        // A refusal removes the flight, so each row gets a fresh one.
        let rows: [(&str, &str, ProbeRefusal); 6] = [
            ("/ip4/8.8.8.8/tcp/4001", "/dns4/example.invalid/tcp/4001", ProbeRefusal::NotLiteralIp),
            ("/ip4/8.8.8.8/tcp/4001", "/ip4/1.1.1.1/tcp/4001", ProbeRefusal::SourceMismatch),
            ("/ip4/8.8.8.8/tcp/4001", "/ip4/127.0.0.1/tcp/4001", ProbeRefusal::SourceMismatch),
            ("/ip4/127.0.0.1/tcp/4001", "/ip4/127.0.0.1/tcp/4001", ProbeRefusal::NotGlobal),
            ("/ip4/10.0.0.2/tcp/4001", "/ip4/10.0.0.2/tcp/9", ProbeRefusal::NotGlobal),
            (
                "/ip4/8.8.8.8/tcp/4001",
                "/ip4/8.8.8.8/tcp/4001/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN/p2p-circuit",
                ProbeRefusal::NotGlobal,
            ),
        ];
        for (source, target, expected) in rows {
            let (mut s, _) = with_request(source, dial);
            assert_eq!(judge(&mut s, dial, &[addr(target)]), Some(expected), "{source} -> {target}");
            assert!(s.in_flight.is_empty(), "a refused dial-back leaves the in-flight count");
        }
        // The port may differ from the request's: §7 lets "only the
        // candidate port/transport vary".
        let (mut s, _) = with_request("/ip4/8.8.8.8/tcp/4001", dial);
        assert_eq!(judge(&mut s, dial, &[addr("/ip4/8.8.8.8/tcp/9")]), None);
        // No address at all, and a request whose connection is gone.
        let (mut s, _) = with_request("/ip4/8.8.8.8/tcp/4001", dial);
        assert_eq!(judge(&mut s, dial, &[]), Some(ProbeRefusal::NoAddress));
        let (mut s, _) = with_request("/ip4/8.8.8.8/tcp/4001", dial);
        s.sources.clear();
        assert_eq!(
            judge(&mut s, dial, &[addr("/ip4/8.8.8.8/tcp/4001")]),
            Some(ProbeRefusal::SourceUnknown)
        );
    }

    #[test]
    fn a_dial_this_wrapper_did_not_pair_passes_through_unjudged() {
        // Every dial in the Swarm reaches every behaviour's pending
        // hook. One that is not a dial-back -- a Kademlia query, a
        // manual dial to a private address -- is none of this rule's
        // business.
        let (mut s, _) = with_request("/ip4/8.8.8.8/tcp/4001", ConnectionId::new_unchecked(7));
        let other = ConnectionId::new_unchecked(8);
        assert_eq!(judge(&mut s, other, &[addr("/ip4/10.0.0.1/tcp/1")]), None);
        assert_eq!(s.in_flight.len(), 1, "and the paired one is untouched");
    }

    #[test]
    fn each_budget_refuses_by_name_and_a_refusal_spends_nothing() {
        let mut s = server(ProbeBudgets {
            max_concurrent: 2,
            per_client_per_minute: 2,
            global_per_minute: 3,
        });
        let a = PeerId::random();
        let b = PeerId::random();
        s.tick(1_000);
        assert_eq!(s.admit_start(a), Ok(()));
        assert_eq!(s.admit_start(a), Ok(()));
        assert_eq!(s.admit_start(a), Err(ProbeRefusal::ClientRate));
        assert_eq!(s.global.len(), 2, "the refused start was not charged globally");
        assert_eq!(s.admit_start(b), Ok(()));
        assert_eq!(s.admit_start(b), Err(ProbeRefusal::GlobalRate));
        assert_eq!(
            s.per_client.get(&b).map(VecDeque::len),
            Some(1),
            "nor against the client"
        );
        // Concurrency is checked first: with two dial-backs pending,
        // even a client with rate to spare is refused.
        s.awaiting_dial.push_back(InFlight {
            client: a,
            request: ConnectionId::new_unchecked(1),
            started_ms: 1_000,
        });
        s.in_flight.insert(
            ConnectionId::new_unchecked(2),
            InFlight {
                client: a,
                request: ConnectionId::new_unchecked(1),
                started_ms: 1_000,
            },
        );
        let c = PeerId::random();
        assert_eq!(s.admit_start(c), Err(ProbeRefusal::ConcurrentProbes));
        // The window slides: one minute on, both rates are free again.
        s.awaiting_dial.clear();
        s.in_flight.clear();
        s.tick(1_000 + RATE_WINDOW_MS);
        assert_eq!(s.admit_start(a), Ok(()));
        assert_eq!(s.admit_start(b), Ok(()));
    }

    #[test]
    fn the_client_rate_map_is_bounded_by_the_global_budget() {
        // The module note's derived bound: an entry exists only for a
        // client with a start inside the window, and the window admits
        // at most the global budget of starts.
        let mut s = server(ProbeBudgets {
            max_concurrent: 64,
            per_client_per_minute: 1,
            global_per_minute: 3,
        });
        s.tick(10);
        for _ in 0..3 {
            assert_eq!(s.admit_start(PeerId::random()), Ok(()));
        }
        assert_eq!(s.admit_start(PeerId::random()), Err(ProbeRefusal::GlobalRate));
        assert_eq!(s.rated_clients(), 3);
        s.tick(10 + RATE_WINDOW_MS);
        assert_eq!(s.rated_clients(), 0, "pruned on the tick, not only on the next start");
    }

    #[test]
    fn a_dial_back_with_no_outcome_leaves_the_count_at_the_horizon() {
        let mut s = server(ProbeBudgets::default());
        s.tick(5_000);
        s.in_flight.insert(
            ConnectionId::new_unchecked(2),
            InFlight {
                client: PeerId::random(),
                request: ConnectionId::new_unchecked(1),
                started_ms: 5_000,
            },
        );
        s.tick(5_000 + IN_FLIGHT_HORIZON_MS);
        assert_eq!(s.in_flight(), 1, "held up to the horizon");
        s.tick(5_001 + IN_FLIGHT_HORIZON_MS);
        assert_eq!(s.in_flight(), 0, "and released past it");
    }

    #[test]
    fn refusals_past_the_event_bound_are_counted_and_not_queued() {
        let mut s = server(ProbeBudgets::default());
        let client = PeerId::random();
        for _ in 0..=MAX_PENDING_EVENTS {
            s.refuse(client, None, ProbeRefusal::GlobalRate);
        }
        assert_eq!(s.pending.len(), MAX_PENDING_EVENTS);
        assert_eq!(s.counters.refused(ProbeRefusal::GlobalRate), MAX_PENDING_EVENTS as u64 + 1);
        assert_eq!(s.counters.events_dropped, 1);
    }

    #[test]
    fn every_refusal_has_its_own_label() {
        let labels: std::collections::BTreeSet<&str> =
            ProbeRefusal::ALL.iter().map(|r| r.label()).collect();
        assert_eq!(labels.len(), ProbeRefusal::ALL.len());
        assert!(labels.iter().all(|l| l.starts_with("refused_")));
    }

    #[test]
    fn a_served_probe_is_counted_by_its_outcome() {
        let mut s = server(ProbeBudgets::default());
        let client = PeerId::random();
        let ok = s.served(ServerEvent {
            all_addrs: vec![],
            tested_addr: addr("/ip4/8.8.8.8/tcp/1"),
            client,
            data_amount: 3,
            result: Ok(()),
        });
        let failed = s.served(ServerEvent {
            all_addrs: vec![],
            tested_addr: addr("/ip4/8.8.8.8/tcp/1"),
            client,
            data_amount: 0,
            result: Err(std::io::Error::other("dial back failed")),
        });
        assert!(matches!(ok, ProbeServerEvent::Served { outcome: ServedOutcome::Ok, data_amount: 3, .. }));
        assert!(matches!(failed, ProbeServerEvent::Served { outcome: ServedOutcome::Failed, .. }));
        assert_eq!((s.counters.served_ok, s.counters.served_failed), (1, 1));
    }
}
