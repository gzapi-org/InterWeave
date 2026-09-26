// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The keepalive on relay control connections
//! (`transport/libp2p/CONNECTIVITY.md` §14 item 5, the rule since
//! 2026-09-26; the owner, "keepalive yes").
//!
//! # Why a relay control connection needs one
//!
//! A path can die with no event at all -- a NAT rebinding, a carrier
//! drop, an interface gone without the runtime seeing the listener
//! change -- and TCP says nothing for as long as nothing is sent. A
//! relay client then goes on believing itself reachable through a relay
//! it cannot reach: SPIKE-004 phase B's `ifchange` row measured a client
//! still counting both relays two minutes after its path to them was
//! gone. So the client PINGS its relay and a missed ping CLOSES the
//! connection, which takes the reservation ladder exactly as a removal
//! does.
//!
//! # Only on control connections, and the two ends differ
//!
//! The pinging is the crate's own (`libp2p-ping` 0.48.0's handler at its
//! default: every 15 s, 20 s to answer, the first failure forgiven), and
//! it runs only on a connection to a peer this profile holds an ACTIVE
//! reservation on -- switched on and off by the relay driver as the
//! reservation manager's state moves ([`RelayKeepalive::set_active`]).
//! No other connection is pinged.
//!
//! A relay must ANSWER from the first moment, and must not ping: the
//! crate's handler gives up for good the first time the other end
//! refuses the protocol (`handler.rs:201-205`, `State::Inactive`), so a
//! relay that offered ping only once it knew a reservation stood would
//! lose the client's first ping to a race it could not win -- and the
//! crate's handler always pings too, so running it on the relay would
//! ping every authorized peer merely connected to it. A relay therefore
//! runs an answer-only handler on every connection its class gate admits:
//! it echoes what the protocol echoes and never opens a stream.
//!
//! Under [`crate::class_gate::ClassGated`] for the infrastructure
//! service, so a peer in no trust set is offered nothing.
//!
//! # How long a dead path takes to close
//!
//! The crate forgives the first failure and reports the second
//! (`handler.rs:273-288`), and after a failure it opens a NEW stream,
//! whose negotiation over a dead path fails only at the Swarm's upgrade
//! timeout (ten seconds) whatever the ping interval. So at the default a
//! silent path closes roughly 15 + 20 + 10 seconds after it went silent
//! -- measured at a fast interval by `tests/relay_keepalive.rs`, whose
//! window allows for that last term.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::task::{Context, Poll};

use either::Either;
use futures::future::BoxFuture;
use futures::{AsyncReadExt as _, AsyncWriteExt as _, FutureExt as _};
use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::core::upgrade::{DeniedUpgrade, ReadyUpgrade};
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::handler::{
    ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
};
use libp2p::swarm::{
    CloseConnection, ConnectionClosed, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour,
    NotifyHandler, Stream, StreamProtocol, SubstreamProtocol, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId, ping};

use crate::class_gate::{ClassGated, Service};
use interweave_transport_runtime::SnapshotHandle;

/// The keepalive field's type in the composed behaviour.
pub type KeepaliveField = Toggle<ClassGated<RelayKeepalive>>;

/// The keepalive field: present when this profile is a relay client or
/// a relay server, answering pings when it is a server, and offered only
/// to the two authorized classes.
#[must_use]
pub fn build_field(client: bool, server: bool, policy: SnapshotHandle) -> KeepaliveField {
    Toggle::from((client || server).then(|| {
        ClassGated::for_service(
            RelayKeepalive::new(server),
            policy,
            Service::ConnectivityInfrastructure,
        )
    }))
}

/// The crate's pinging handler; the type is not exported by name.
type PingHandler = <ping::Behaviour as NetworkBehaviour>::ConnectionHandler;

/// The ping payload size (`libp2p-ping` 0.48.0 `protocol.rs`, the
/// specification's 32 bytes).
const PING_SIZE: usize = 32;

/// What the keepalive counted: how an operator tells a dead path from
/// a relay that does not speak ping.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct KeepaliveCounters {
    /// Pings this profile sent that were answered.
    pub answered: u64,
    /// Connections closed for a missed ping.
    pub missed: u64,
    /// Control connections whose relay does not speak ping: kept, and
    /// not pinged again.
    pub unsupported: u64,
    /// Pings this profile answered for others.
    pub echoed: u64,
}

/// What the behaviour tells its handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// Start pinging: the peer holds this profile's reservation.
    On,
    /// Stop pinging.
    Off,
}

/// What a handler reports.
#[derive(Debug)]
pub enum Report {
    /// The crate's handler's result.
    Ping(Result<std::time::Duration, ping::Failure>),
    /// One ping answered for the peer.
    Echoed,
}

/// The keepalive, for a relay client, a relay server, or both.
pub struct RelayKeepalive {
    config: ping::Config,
    /// Whether this profile answers pings: it serves relays.
    answers: bool,
    /// The peers this profile holds an active reservation on.
    active: HashSet<PeerId>,
    connections: HashMap<PeerId, HashSet<ConnectionId>>,
    pending: VecDeque<ToSwarm<(), Switch>>,
    counters: KeepaliveCounters,
}

impl RelayKeepalive {
    /// A keepalive pinging at the crate's default (`ping::Config::new`)
    /// and, when `answers`, answering every admitted connection.
    #[must_use]
    pub fn new(answers: bool) -> Self {
        Self::with_config(answers, ping::Config::new())
    }

    /// The same with `config`'s interval and timeout -- for a test that
    /// cannot wait a minute for a missed ping; the runtime uses [`Self::new`].
    #[must_use]
    pub fn with_config(answers: bool, config: ping::Config) -> Self {
        Self {
            config,
            answers,
            active: HashSet::new(),
            connections: HashMap::new(),
            pending: VecDeque::new(),
            counters: KeepaliveCounters::default(),
        }
    }

    /// Ping exactly the connections to `relays`: those joining the set
    /// are switched on, those leaving it off.
    pub fn set_active(&mut self, relays: HashSet<PeerId>) {
        for (peer, switch) in relays
            .difference(&self.active)
            .map(|p| (*p, Switch::On))
            .chain(self.active.difference(&relays).map(|p| (*p, Switch::Off)))
            .collect::<Vec<_>>()
        {
            for id in self.connections.get(&peer).into_iter().flatten() {
                self.pending.push_back(ToSwarm::NotifyHandler {
                    peer_id: peer,
                    handler: NotifyHandler::One(*id),
                    event: switch,
                });
            }
        }
        self.active = relays;
    }

    /// What was counted so far.
    #[must_use]
    pub const fn counters(&self) -> KeepaliveCounters {
        self.counters
    }

    fn handler(&mut self, id: ConnectionId, peer: PeerId) -> KeepaliveHandler {
        self.connections.entry(peer).or_default().insert(id);
        KeepaliveHandler {
            config: self.config.clone(),
            answers: self.answers,
            pinging: self
                .active
                .contains(&peer)
                .then(|| PingHandler::new(self.config.clone())),
            echo: None,
        }
    }
}

impl NetworkBehaviour for RelayKeepalive {
    type ConnectionHandler = KeepaliveHandler;
    type ToSwarm = ();

    fn handle_established_inbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(self.handler(id, peer))
    }

    fn handle_established_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(self.handler(id, peer))
    }

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        if let FromSwarm::ConnectionClosed(ConnectionClosed {
            peer_id,
            connection_id,
            ..
        }) = event
            && let Some(ids) = self.connections.get_mut(&peer_id)
        {
            ids.remove(&connection_id);
            if ids.is_empty() {
                self.connections.remove(&peer_id);
            }
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {
            Report::Echoed => self.counters.echoed += 1,
            Report::Ping(Ok(_)) => self.counters.answered += 1,
            // Kept: a relay that does not speak ping is not a dead
            // path, and the crate's handler has stopped pinging it.
            Report::Ping(Err(ping::Failure::Unsupported)) => self.counters.unsupported += 1,
            // A MISSED LIVENESS: the crate reports the second failure in
            // a row, the first being forgiven (`handler.rs:273-288`).
            Report::Ping(Err(_)) => {
                self.counters.missed += 1;
                self.pending.push_back(ToSwarm::CloseConnection {
                    peer_id: peer,
                    connection: CloseConnection::One(id),
                });
            }
        }
    }

    fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        self.pending.pop_front().map_or(Poll::Pending, Poll::Ready)
    }
}

/// One connection's keepalive: the crate's pinging handler while its
/// peer holds a reservation of this profile's, an answer-only echo on a
/// relay, and nothing otherwise.
pub struct KeepaliveHandler {
    config: ping::Config,
    answers: bool,
    pinging: Option<PingHandler>,
    /// The echo of the latest inbound ping stream; a new one replaces
    /// it, as the crate's handler does, so one connection holds one.
    echo: Option<BoxFuture<'static, io::Result<Stream>>>,
}

/// Answer one ping on `stream`: what the protocol's responder does,
/// and nothing else. The stream comes back for the next.
async fn echo(mut stream: Stream) -> io::Result<Stream> {
    let mut payload = [0u8; PING_SIZE];
    stream.read_exact(&mut payload).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(stream)
}

impl ConnectionHandler for KeepaliveHandler {
    type FromBehaviour = Switch;
    type ToBehaviour = Report;
    type InboundProtocol = Either<ReadyUpgrade<StreamProtocol>, DeniedUpgrade>;
    type OutboundProtocol = ReadyUpgrade<StreamProtocol>;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol> {
        if self.pinging.is_some() || self.answers {
            SubstreamProtocol::new(Either::Left(ReadyUpgrade::new(ping::PROTOCOL_NAME)), ())
        } else {
            SubstreamProtocol::new(Either::Right(DeniedUpgrade), ())
        }
    }

    fn on_behaviour_event(&mut self, switch: Switch) {
        match switch {
            Switch::On => {
                if self.pinging.is_none() {
                    self.pinging = Some(PingHandler::new(self.config.clone()));
                }
            }
            Switch::Off => self.pinging = None,
        }
    }

    fn connection_keep_alive(&self) -> bool {
        // Liveness, not retention: whether the connection is kept is
        // the relay client's question, which it answers itself.
        false
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ConnectionHandlerEvent<Self::OutboundProtocol, (), Self::ToBehaviour>> {
        if let Some(fut) = self.echo.as_mut()
            && let Poll::Ready(answered) = fut.poll_unpin(cx)
        {
            self.echo = None;
            if let Ok(stream) = answered {
                self.echo = Some(echo(stream).boxed());
                return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(Report::Echoed));
            }
        }
        if let Some(pinging) = self.pinging.as_mut() {
            return pinging.poll(cx).map(|event| event.map_custom(Report::Ping));
        }
        Poll::Pending
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            '_,
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: futures::future::Either::Left(stream),
                info,
            }) => {
                if let Some(pinging) = self.pinging.as_mut() {
                    pinging.on_connection_event(ConnectionEvent::FullyNegotiatedInbound(
                        FullyNegotiatedInbound {
                            protocol: stream,
                            info,
                        },
                    ));
                } else if self.answers {
                    self.echo = Some(echo(stream).boxed());
                }
            }
            ConnectionEvent::FullyNegotiatedOutbound(outbound) => {
                if let Some(pinging) = self.pinging.as_mut() {
                    pinging.on_connection_event(ConnectionEvent::FullyNegotiatedOutbound(outbound));
                }
            }
            ConnectionEvent::DialUpgradeError(err) => {
                if let Some(pinging) = self.pinging.as_mut() {
                    pinging.on_connection_event(ConnectionEvent::DialUpgradeError(err));
                }
            }
            // An echo the peer ended, a denied upgrade, an address or
            // protocol change: nothing the keepalive acts on.
            _ => {}
        }
    }
}
