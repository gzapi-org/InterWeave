// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Swarm ownership, bounded channels, and deterministic shutdown.
//!
//! # One task owns the Swarm
//!
//! A `Swarm` is not `Sync` and must be polled from one place. It is
//! therefore moved into a dedicated task, and everything else talks to
//! it through channels. That is not merely an implementation detail: it
//! is what makes "the policy snapshot is read without blocking the
//! Swarm poll" (ADR-0011) achievable later, because nothing outside the
//! task can hold the Swarm at all.
//!
//! # Both channels are BOUNDED
//!
//! CLAUDE.md §6 requires it, and the reason is visible here. An
//! unbounded event channel would let a slow consumer turn a burst of
//! remote activity into unbounded local memory — a remote peer choosing
//! how much memory this process uses. Bounded means the Swarm task
//! applies backpressure to itself instead, which is the correct place
//! for the cost to land.
//!
//! # Shutdown is deterministic, and proved so
//!
//! [`SwarmRuntime::shutdown`] sends a command, then **awaits the task's
//! join handle**. It does not drop a handle and hope. The exit gate says
//! "shut down without leaked tasks", and the only way to know a task
//! ended is to have waited for it.
//!
//! Bounded channels and deterministic shutdown interact, and the first
//! version of this module got the interaction wrong. Awaiting the event
//! send inline parks the whole task inside the event branch: with a full
//! channel and a consumer that has stopped draining, the command branch
//! is never polled again, so `shutdown` enqueues its command and waits
//! forever for a reply from a task that is waiting for the consumer it
//! is blocking. The loop therefore holds a translated event and selects
//! between delivering it and taking a command, so shutdown always wins.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::preauth::PreAuthLimits;
use interweave_transport_runtime::{
    ConnectionManager, ConnectionPolicy, ConnectionSlot, DialDenial, DialOrigin, DialRequest,
    DialTicket, TrustSources,
};
use libp2p::core::transport::ListenerId;
use libp2p::swarm::{DialError, SwarmEvent as Libp2pSwarmEvent};
use libp2p::{Multiaddr, PeerId, identify, noise, tcp, yamux};
use tokio::sync::{mpsc, oneshot};

use crate::behaviour::{SubstrateBehaviour, SubstrateBehaviourEvent};
use crate::gated_swarm::{AdmittedDial, GatedSwarm};

/// Default depth of the command channel.
pub const DEFAULT_COMMAND_CAPACITY: usize = 64;

/// Default depth of the event channel.
///
/// Deeper than commands because events arrive from the network and
/// commands come from this process. It is still bounded: a burst of
/// remote activity must cost the Swarm task backpressure, never
/// unbounded local memory.
pub const DEFAULT_EVENT_CAPACITY: usize = 256;

/// Upper bound on every channel depth and table size below.
///
/// Not a tuning value — a ceiling. Without one a configuration can ask
/// for an allocation large enough to be the denial of service it was
/// meant to prevent, and the request looks like ordinary tuning.
pub const MAX_CONFIGURED_CAPACITY: usize = 65_536;

/// How the substrate is built.
#[derive(Debug, Clone, Copy)]
pub struct SubstrateConfig {
    /// Depth of the command channel.
    pub command_capacity: usize,
    /// Depth of the event channel.
    pub event_capacity: usize,
    /// Maximum concurrent pending dials.
    pub max_pending_dials: usize,
    /// Maximum established connections.
    pub max_connections: usize,
    /// Idle connection timeout.
    pub idle_timeout: Duration,
    /// Bounds on work done for a peer that has not authenticated.
    ///
    /// A `PreAuthLimits` value is proof its numbers were checked --
    /// `PreAuthLimitsBuilder::build` is the only way to make one -- so
    /// there is nothing for `validate` to re-check here.
    pub preauth: PreAuthLimits,
    /// Maximum listeners with a caller still awaiting their address.
    ///
    /// The command channel bounds how many `Listen` commands can be
    /// QUEUED, not how many can be accepted: the task drains commands
    /// continuously, so pending replies and OS listeners accumulate past
    /// any instantaneous queue depth. This is the bound on the table
    /// itself.
    pub max_pending_listens: usize,
}

impl Default for SubstrateConfig {
    fn default() -> Self {
        Self {
            command_capacity: DEFAULT_COMMAND_CAPACITY,
            event_capacity: DEFAULT_EVENT_CAPACITY,
            max_pending_dials: 32,
            max_connections: 256,
            idle_timeout: Duration::from_secs(60),
            preauth: PreAuthLimits::default(),
            max_pending_listens: 64,
        }
    }
}

impl SubstrateConfig {
    /// Check every limit before anything is built.
    ///
    /// # Errors
    /// Returns [`SubstrateError::InvalidConfig`] naming the first field
    /// outside `1..=`[`MAX_CONFIGURED_CAPACITY`].
    pub fn validate(&self) -> Result<(), SubstrateError> {
        // CHANNEL DEPTHS need at least one slot: `mpsc::channel(0)`
        // panics, so zero here is not a strict policy but an abort.
        let depths = [
            ("command_capacity", self.command_capacity, 1),
            ("event_capacity", self.event_capacity, 1),
            // CAPS may be zero, and zero is not a mistake: a policy
            // admitting no dial, holding no connection, or accepting no
            // listen is a coherent thing to configure and is how the
            // refusal paths are exercised. Rejecting it would turn a
            // panic guard into a policy opinion.
            ("max_pending_dials", self.max_pending_dials, 0),
            ("max_connections", self.max_connections, 0),
            ("max_pending_listens", self.max_pending_listens, 0),
        ];
        for (field, got, min) in depths {
            if got < min || got > MAX_CONFIGURED_CAPACITY {
                return Err(SubstrateError::InvalidConfig {
                    field,
                    got,
                    allowed: (min, MAX_CONFIGURED_CAPACITY),
                });
            }
        }
        Ok(())
    }
}

/// What the substrate can be asked to do.
#[derive(Debug)]
pub enum SwarmCommand {
    /// Start listening on an address.
    Listen {
        /// The address to listen on.
        address: Multiaddr,
        /// Answered with the listener's assigned address, once the OS
        /// has assigned it.
        ///
        /// Held until `NewListenAddr` arrives rather than answered
        /// immediately: `listen_on` returns only a `ListenerId`, so an
        /// immediate answer could carry nothing a caller could advertise
        /// or dial.
        reply: oneshot::Sender<Result<Multiaddr, String>>,
    },
    /// Dial a peer at an address.
    ///
    /// Carries the EXPECTED PeerId, and it is bound into the dial rather
    /// than used only for admission. Dialling a bare address tells libp2p
    /// nothing about who should be there, so a server at that address can
    /// complete a Noise handshake with any key and the connection is
    /// accepted — dialling an address is not the same as reaching the
    /// peer that was supposed to be there.
    Dial {
        /// The peer this address is believed to belong to.
        peer: TransportIdentity,
        /// Where to dial.
        address: Multiaddr,
        /// Answered when the dial is admitted or refused locally.
        reply: oneshot::Sender<Result<(), DialRefusal>>,
    },
    /// Replace the trust sources, evicting what they no longer permit.
    SetTrust {
        /// Who this profile trusts, and for what.
        trust: Box<TrustSources>,
        /// Answered with the number of connections closed by the change.
        reply: oneshot::Sender<usize>,
    },
    /// Stop, closing listeners and connections.
    Shutdown {
        /// Answered once the Swarm has been dropped.
        reply: oneshot::Sender<()>,
    },
}

/// Why a dial did not proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialRefusal {
    /// The local admission policy refused it.
    ///
    /// Refused BEFORE a socket is opened. That ordering is the whole
    /// value of the gate: a quarantined address costs nothing.
    Policy(DialDenial),
    /// libp2p refused the dial itself.
    Backend(String),
}

/// What the substrate reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwarmEvent {
    /// A listener is up.
    Listening {
        /// The address it bound to.
        address: Multiaddr,
    },
    /// A connection was established and Noise authenticated the peer.
    Connected {
        /// The authenticated remote identity.
        peer: TransportIdentity,
    },
    /// A connection closed.
    Disconnected {
        /// The remote identity.
        peer: TransportIdentity,
    },
    /// Identify completed for a peer.
    Identified {
        /// The remote identity.
        peer: TransportIdentity,
        /// The protocol string it advertised.
        protocol_version: String,
        /// The addresses it claims to listen on. ADVISORY: peer-asserted
        /// and never authorization.
        listen_addresses: Vec<Multiaddr>,
    },
    /// An outbound dial failed after being admitted.
    DialFailed {
        /// The peer that was being dialed, when known.
        peer: Option<TransportIdentity>,
        /// What went wrong.
        detail: String,
    },
}

/// What can go wrong building or driving the substrate.
#[derive(Debug)]
pub enum SubstrateError {
    /// The transport could not be constructed.
    Transport(String),
    /// The Swarm task is gone.
    ///
    /// Every command path returns this rather than panicking: the task
    /// ending is a normal outcome of shutdown, and a caller racing it
    /// should get an error, not an abort.
    Stopped,
    /// A stored or observed PeerId is not one the neutral contract accepts.
    Identity(String),
    /// A [`SubstrateConfig`] value outside its permitted range.
    ///
    /// Returned rather than panicked. `mpsc::channel(0)` aborts the
    /// process, and this is a transport daemon whose lint policy treats a
    /// reachable panic as a defect — a configuration mistake must not be
    /// the thing that takes it down.
    InvalidConfig {
        /// Which field.
        field: &'static str,
        /// The value supplied.
        got: usize,
        /// The permitted range, inclusive.
        allowed: (usize, usize),
    },
}

impl core::fmt::Display for SubstrateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Transport(d) => write!(f, "transport: {d}"),
            Self::Stopped => write!(f, "the swarm task has stopped"),
            Self::Identity(d) => write!(f, "identity: {d}"),
            Self::InvalidConfig {
                field,
                got,
                allowed: (min, max),
            } => write!(f, "{field} is {got}; it must be {min}..={max}"),
        }
    }
}

impl core::error::Error for SubstrateError {}

fn to_transport_identity(peer: &PeerId) -> Result<TransportIdentity, SubstrateError> {
    TransportIdentity::parse(peer.to_base58()).map_err(|e| SubstrateError::Identity(e.to_string()))
}

/// A running substrate.
///
/// Dropping this does NOT stop the Swarm task deterministically; call
/// [`SwarmRuntime::shutdown`] and await it. The drop path is a safety
/// net, not the intended exit.
#[derive(Debug)]
pub struct SwarmRuntime {
    commands: mpsc::Sender<SwarmCommand>,
    events: mpsc::Receiver<SwarmEvent>,
    task: Option<tokio::task::JoinHandle<()>>,
    local_peer: TransportIdentity,
}

impl SwarmRuntime {
    /// Build the substrate and start its task.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Transport`] if the transport cannot be
    /// constructed, or [`SubstrateError::Identity`] if libp2p produces a
    /// PeerId the neutral grammar rejects.
    pub fn start(
        identity: &ProfileIdentity,
        config: SubstrateConfig,
        trust: TrustSources,
    ) -> Result<Self, SubstrateError> {
        // BEFORE anything is built. `mpsc::channel(0)` panics, and a
        // half-constructed Swarm would still have opened sockets.
        config.validate()?;

        let keypair = identity.swarm_keypair();
        let local_peer = to_transport_identity(&PeerId::from_public_key(&keypair.public()))?;

        let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| SubstrateError::Transport(e.to_string()))?
            .with_behaviour(|key| SubstrateBehaviour::new(key.public(), config.preauth))
            .map_err(|e| SubstrateError::Transport(e.to_string()))?
            .with_swarm_config(|c| c.with_idle_connection_timeout(config.idle_timeout))
            // THE HANDSHAKE TIMEOUT, taken from the same limits the
            // pre-auth gate enforces rather than left to libp2p's
            // default. The two happen to agree at ten seconds today,
            // and a configuration that narrowed one without the other
            // would produce a listener whose accounting and whose
            // transport disagreed about when a handshake is over --
            // slots reclaimed while the socket was still negotiating,
            // or the reverse.
            .with_connection_timeout(Duration::from_millis(config.preauth.handshake_timeout_ms()))
            .build();
        let mut swarm = GatedSwarm::new(swarm);

        // The Stage 2 policy, driving a real dial path from the first
        // line of substrate code rather than being wired in later.
        let policy = ConnectionPolicy::new(config.max_pending_dials, config.max_connections);
        let mut manager = ConnectionManager::new(policy, config.max_pending_dials);
        // TRUST IS A CONSTRUCTOR ARGUMENT, not a later call. A runtime
        // that could be started without saying who it trusts would have
        // a window in which it trusted nobody -- or, in the version this
        // replaces, everybody -- and nothing in the type system to say
        // which.
        let _ = manager.set_trust(trust, &[]);

        // A MONOTONIC CLOCK, because the policy is a state machine over
        // time and it had been given a literal `0` on every call. Every
        // backoff window, every quarantine, and every retry deadline was
        // therefore evaluated at the same instant forever: an address
        // quarantined for thirty minutes was quarantined until restart,
        // and a peer in backoff never left it. `Instant` rather than
        // wall time so a clock adjustment cannot move a deadline.
        let started = std::time::Instant::now();

        // Tickets for dials the Swarm has accepted and not yet reported
        // on. Keyed by the connection id the dial was built with, which
        // is knowable before dialling and is what the outcome event
        // carries back.
        let mut in_flight: HashMap<libp2p::swarm::ConnectionId, DialTicket> = HashMap::new();

        // Every connection this process holds open, each holding the
        // slot it occupies under `max_connections`. Bounded by that
        // ceiling rather than by the peer set, because the entry is
        // created by a remote party connecting.
        //
        // The peer is kept alongside the slot because a trust change
        // has to find the connections it revokes, and "which peer is on
        // this connection" is not a question the Swarm will answer
        // after the fact.
        let mut open: HashMap<libp2p::swarm::ConnectionId, OpenConnection> = HashMap::new();

        // Listen replies wait for the address the OS actually assigned.
        // `listen_on` returns a ListenerId and nothing else; the bound
        // address arrives later as `NewListenAddr`. A reply sent before
        // then can only be a placeholder, and a caller cannot advertise
        // or dial a placeholder.
        let mut listens: PendingListens = HashMap::new();

        let (command_tx, mut command_rx) = mpsc::channel(config.command_capacity);
        let (event_tx, event_rx) = mpsc::channel(config.event_capacity);

        let task = tokio::spawn(async move {
            // Events translated but not yet handed over.
            //
            // THIS IS WHY SHUTDOWN CANNOT DEADLOCK. Awaiting the send
            // inline would park the whole task inside the event branch:
            // with a full channel and a consumer that has stopped
            // draining, the command branch is never polled again, so
            // `shutdown` enqueues its command and waits forever for a
            // reply from a task that is waiting for the consumer it is
            // blocking. Holding events here instead lets the loop keep
            // selecting, and a Shutdown command wins over delivering one.
            //
            // AND THIS IS WHY `listen` CANNOT HANG. An earlier version
            // held a single event and, while it was held, selected only
            // between channel capacity and more commands — so the Swarm
            // was not polled at all. `translate` is what answers a
            // pending `listen`, and it only runs on a polled event, so a
            // `Listen` issued in that state waited for a `NewListenAddr`
            // that could never be observed. The caller could not drain
            // its way out either: `listen` borrows `&self` and
            // `next_event` borrows `&mut self`, so no one holding the
            // former can call the latter. Keeping the Swarm in the same
            // select is what closes that cycle.
            let mut outbox: VecDeque<SwarmEvent> = VecDeque::new();

            loop {
                // BOUNDED, per the resource rules: a consumer that stops
                // draining must not let a remote peer choose this
                // process's memory. The cap is the channel's own
                // capacity, so a stalled consumer costs at most twice
                // what it already agreed to buffer.
                //
                // The slack is one slot per OUTSTANDING LISTEN, and it is
                // safe for the reason the cap exists: `listens` grows
                // only when a local caller issues `Listen` and shrinks
                // when that caller is answered, so its size is chosen by
                // this process and never by the network. Without the
                // slack a full outbox would stop the polling that
                // resolves those very callers, which is the deadlock
                // above wearing a bound.
                let room = outbox.len() < config.event_capacity.saturating_add(listens.len());

                tokio::select! {
                    // `reserve` waits for capacity WITHOUT consuming an
                    // event, so nothing is lost when another branch wins.
                    permit = event_tx.reserve(), if !outbox.is_empty() => {
                        match permit {
                            Ok(permit) => {
                                if let Some(event) = outbox.pop_front() {
                                    permit.send(event);
                                }
                            }
                            // The consumer is gone; nothing can be
                            // delivered again. Stop rather than
                            // accumulate.
                            Err(_) => break,
                        }
                    }
                    command = command_rx.recv() => {
                        match command {
                            // The channel closed: every sender is gone, so
                            // no further work can arrive. Ending here is
                            // what makes a dropped runtime stop rather than
                            // spin forever.
                            None => break,
                            Some(SwarmCommand::Shutdown { reply }) => {
                                let _ = reply.send(());
                                break;
                            }
                            Some(command) => {
                                let mut refuse = Vec::new();
                                handle_command(
                                    &mut swarm,
                                    &mut manager,
                                    &open,
                                    &mut refuse,
                                    &mut listens,
                                    &mut in_flight,
                                    config.max_pending_listens,
                                    now_ms(started),
                                    command,
                                );
                                // A revocation names connections; this
                                // is what closes them. Deferred out of
                                // `handle_command` so that function
                                // borrows the table it reads rather
                                // than the Swarm it would mutate.
                                for id in refuse {
                                    swarm.close_connection(id);
                                }
                            }
                        }
                    }
                    event = swarm.select_next_some(), if room => {
                        // SETTLE THE ADMISSION FIRST. Every dial holds a
                        // pending slot until its outcome arrives, so an
                        // outcome that did not release one is a slot
                        // leaked for the life of the process -- the
                        // ceiling decaying by one per dial until nothing
                        // can connect. Done here rather than inside
                        // `translate`, which is a pure shape conversion
                        // and must not also own resource accounting.
                        let mut refuse = Vec::new();
                        settle_outcome(
                            &event,
                            &mut manager,
                            &mut in_flight,
                            &mut open,
                            &mut refuse,
                            now_ms(started),
                        );
                        // An inbound connection the ceiling cannot
                        // account for is closed rather than kept.
                        // Deferred out of `settle_outcome` so that
                        // function stays free of the Swarm.
                        for id in refuse {
                            swarm.close_connection(id);
                        }

                        let mut abandoned = Vec::new();
                        if let Some(event) = translate(event, &mut listens, &mut abandoned) {
                            outbox.push_back(event);
                        }
                        // A caller that stopped awaiting `listen` — a
                        // dropped future, a cancelled task, a timeout —
                        // leaves an OS listener that nobody holds a
                        // handle to and nobody can close. The bound above
                        // limits how many can accumulate; this is what
                        // makes them go away rather than merely be
                        // capped.
                        abandoned.extend(
                            listens
                                .iter()
                                .filter(|(_, reply)| reply.is_closed())
                                .map(|(id, _)| *id),
                        );
                        for id in abandoned {
                            listens.remove(&id);
                            let _ = swarm.remove_listener(id);
                        }
                    }
                }
            }
            // `swarm` drops here, closing listeners and connections. The
            // join handle completing is what proves it happened.
        });

        Ok(Self {
            commands: command_tx,
            events: event_rx,
            task: Some(task),
            local_peer,
        })
    }

    /// This profile's PeerId.
    #[must_use]
    pub const fn local_peer(&self) -> &TransportIdentity {
        &self.local_peer
    }

    /// Start listening, returning the address that was actually bound.
    ///
    /// With port 0 the assigned port is only knowable from this answer,
    /// so it waits for the listener to report it.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone, or
    /// [`SubstrateError::Transport`] if the listener could not bind.
    pub async fn listen(&self, address: Multiaddr) -> Result<Multiaddr, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::Listen { address, reply })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer
            .await
            .map_err(|_| SubstrateError::Stopped)?
            .map_err(SubstrateError::Transport)
    }

    /// Dial `peer` at `address`, subject to the admission policy.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone. A refusal
    /// by policy or by the backend is `Ok(Err(..))`: the command was
    /// delivered and answered, and the answer was no.
    pub async fn dial(
        &self,
        peer: TransportIdentity,
        address: Multiaddr,
    ) -> Result<Result<(), DialRefusal>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::Dial {
                peer,
                address,
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Replace the trust sources, evicting connections they no longer
    /// permit.
    ///
    /// Returns how many connections the change closed. ADR-0012 makes
    /// that the observable part: a revocation whose only effect was on
    /// the next dial would leave the revoked peer connected.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone.
    pub async fn set_trust(&self, trust: TrustSources) -> Result<usize, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::SetTrust {
                trust: Box::new(trust),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// The next event, or `None` once the substrate has stopped.
    pub async fn next_event(&mut self) -> Option<SwarmEvent> {
        self.events.recv().await
    }

    /// Stop the substrate and WAIT for its task to finish.
    ///
    /// The waiting is the point. "Shut down without leaked tasks" is only
    /// checkable if something observed the task ending, and a dropped
    /// handle observes nothing.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task had already
    /// ended — which is not a failure, only a race a caller may want to
    /// know about.
    pub async fn shutdown(mut self) -> Result<(), SubstrateError> {
        let (reply, answer) = oneshot::channel();
        // Best-effort: if the task already ended, the send fails and the
        // join below still confirms it.
        if self
            .commands
            .send(SwarmCommand::Shutdown { reply })
            .await
            .is_ok()
        {
            let _ = answer.await;
        }
        match self.task.take() {
            Some(handle) => handle
                .await
                .map_err(|e| SubstrateError::Transport(e.to_string())),
            None => Err(SubstrateError::Stopped),
        }
    }
}

impl Drop for SwarmRuntime {
    /// Aborts the task if `shutdown` was not called.
    ///
    /// A safety net so a forgotten runtime cannot outlive its owner, NOT
    /// the intended exit: an abort gives the Swarm no chance to close
    /// connections, and nothing waits to see that it happened.
    fn drop(&mut self) {
        if let Some(handle) = self.task.take() {
            handle.abort();
        }
    }
}

/// Release the admission a connection outcome belongs to.
///
/// The two events that end an outbound attempt are the established
/// connection and the outgoing error. Both carry the `ConnectionId` the
/// dial was built with, which is why the ticket is filed under it: no
/// matching by address, no guessing from a peer that may appear twice.
///
/// An event for a connection this runtime did not dial -- anything
/// inbound -- finds no ticket and does nothing, which is correct rather
/// than merely harmless: inbound connections were never admitted
/// through the dial gate and have no slot to return.
fn settle_outcome(
    event: &Libp2pSwarmEvent<SubstrateBehaviourEvent>,
    manager: &mut ConnectionManager,
    in_flight: &mut HashMap<libp2p::swarm::ConnectionId, DialTicket>,
    open: &mut HashMap<libp2p::swarm::ConnectionId, OpenConnection>,
    refuse: &mut Vec<libp2p::swarm::ConnectionId>,
    now_ms: u64,
) {
    match event {
        Libp2pSwarmEvent::ConnectionEstablished {
            connection_id,
            peer_id,
            ..
        } => {
            // The peer is AUTHENTICATED by this point -- Noise has run
            // -- which is what makes classifying it here meaningful and
            // classifying it any earlier impossible.
            let Ok(peer) = to_transport_identity(peer_id) else {
                // A PeerId the neutral grammar rejects cannot be
                // classified, recorded, or revoked later. Refusing is
                // the only answer that does not leave an unaccountable
                // connection open.
                refuse.push(*connection_id);
                return;
            };
            match in_flight.remove(connection_id) {
                // Outbound: the slot was reserved when the dial was
                // admitted, and the connection takes it over.
                Some(ticket) => {
                    let slot = manager.record_success(ticket, now_ms);
                    open.insert(*connection_id, OpenConnection { peer, slot });
                }
                // INBOUND HAS NO ADMISSION. ADR-0011: the same current
                // authorization that governs outbound applies before an
                // inbound data-plane connection is retained -- arriving
                // is not an authorization. The ceiling is the second
                // question, because a connection this profile will not
                // keep should not spend a slot to find that out.
                None => {
                    let class = manager.classify(&peer);
                    if !manager.retain_inbound(class) {
                        refuse.push(*connection_id);
                        return;
                    }
                    match manager.admit_inbound() {
                        Some(slot) => {
                            open.insert(*connection_id, OpenConnection { peer, slot });
                        }
                        None => {
                            refuse.push(*connection_id);
                        }
                    }
                }
            }
        }
        Libp2pSwarmEvent::ConnectionClosed { connection_id, .. } => {
            // The other half of the pair, and only for a connection
            // that was actually counted: a refused inbound reports a
            // close too, and releasing a slot it never held would let
            // the ceiling drift upward one refusal at a time.
            if let Some(connection) = open.remove(connection_id) {
                manager.record_connection_closed(connection.slot);
            }
        }
        Libp2pSwarmEvent::OutgoingConnectionError {
            connection_id,
            error,
            ..
        } => {
            if let Some(ticket) = in_flight.remove(connection_id) {
                // NOT EVERY FAILURE IS THE ADDRESS'S FAULT. A peer that
                // answered with a different key is not an unreachable
                // route to be retried on backoff -- it is an address
                // that is serving somebody else, and ADR-0011 puts that
                // into quarantine rather than into the retry schedule.
                // Passing it to `record_failure` like any timeout made
                // `record_identity_mismatch` unreachable, so the
                // quarantine existed only as a method nobody called.
                if matches!(error, DialError::WrongPeerId { .. }) {
                    let _ = manager.record_identity_mismatch(ticket, now_ms);
                } else {
                    // ADDRESS-SCOPED, not peer-scoped. ADR-0011: a
                    // failure against one address must not advance a
                    // trusted peer into punitive backoff while a
                    // known-good route remains, and `record_failure` is
                    // the path that keeps that distinction.
                    manager.record_failure(ticket, now_ms);
                }
            }
        }
        _ => {}
    }
}

/// Milliseconds since the runtime task started.
///
/// Monotonic and relative. The policy is a state machine over elapsed
/// time, so an origin of zero is as good as any epoch and immune to a
/// wall-clock adjustment moving a quarantine deadline.
fn now_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A connection this process holds open.
///
/// The slot is the accounting; the peer is what makes a revocation
/// actionable. Kept together because releasing one without the other is
/// exactly the drift that turns a ceiling into a leak.
#[derive(Debug)]
struct OpenConnection {
    peer: TransportIdentity,
    slot: ConnectionSlot,
}

/// Listen commands whose bound address has not arrived yet.
type PendingListens = HashMap<ListenerId, oneshot::Sender<Result<Multiaddr, String>>>;

#[allow(clippy::too_many_arguments)]
fn handle_command(
    swarm: &mut GatedSwarm,
    manager: &mut ConnectionManager,
    open: &HashMap<libp2p::swarm::ConnectionId, OpenConnection>,
    refuse: &mut Vec<libp2p::swarm::ConnectionId>,
    listens: &mut PendingListens,
    in_flight: &mut HashMap<libp2p::swarm::ConnectionId, DialTicket>,
    max_pending_listens: usize,
    now_ms: u64,
    command: SwarmCommand,
) {
    match command {
        SwarmCommand::Listen { address, reply } => {
            // REFUSED BEFORE THE SOCKET, not after. The command channel
            // bounds how many Listen commands may be queued, and the task
            // drains it continuously — so listeners and their pending
            // replies accumulate far past any instantaneous queue depth
            // unless the table itself is bounded. Binding first and then
            // declining to remember the reply would leave the OS listener
            // open with nobody able to close it.
            if listens.len() >= max_pending_listens {
                let _ = reply.send(Err(format!(
                    "at most {max_pending_listens} listeners may be awaiting an address"
                )));
                return;
            }
            match swarm.listen_on(address) {
                // Held until `NewListenAddr` names the assigned address.
                // Answering now could only mean answering with a
                // placeholder, and `listen` documents its result as the
                // bound address.
                Ok(id) => {
                    listens.insert(id, reply);
                }
                Err(e) => {
                    let _ = reply.send(Err(e.to_string()));
                }
            }
        }
        SwarmCommand::Dial {
            peer,
            address,
            reply,
        } => {
            let request = DialRequest {
                peer: Some(peer.clone()),
                address: address.to_string(),
                // A COMMAND IS A PERSON OR AN ADMIN API. Reporting it as
                // the scheduler's own dial made a denial unable to say
                // which of the two it refused, and those are the two an
                // operator most needs told apart.
                origin: DialOrigin::Manual,
            };
            // ADMITTED BEFORE A SOCKET IS OPENED. A quarantined address
            // costs nothing, which is the whole point of checking here
            // rather than after the connection fails.
            //
            // THE CLASS IS NOT THIS SITE'S TO ASSERT. It used to be a
            // hardcoded `DataPlaneTrusted` on every dial, which is the
            // ADR-0036 separation stated in the policy and discarded by
            // its only caller: an infrastructure-only peer was dialable
            // for application traffic, and an empty allowlist -- the
            // default that admits nobody -- admitted everyone. The gate
            // classifies from the trust sources it publishes, and there
            // is no longer an argument through which a call site could
            // say otherwise.
            let ticket = match manager.handle().admit(&request, now_ms) {
                Ok(t) => t,
                Err(denial) => {
                    let _ = reply.send(Err(DialRefusal::Policy(denial)));
                    return;
                }
            };
            // DERIVED FROM THE ADMISSION, not paired with it. The
            // destination is read back out of the ticket rather than
            // rebuilt from the command's own `peer`/`address`, so there
            // is no second copy of the destination that could disagree
            // with the one the gate admitted.
            let admitted = match AdmittedDial::from_ticket(ticket) {
                Ok(a) => a,
                Err(boxed) => {
                    let undialable = *boxed;
                    // The refusal is still an admission that reserved a
                    // slot, so it is settled here rather than dropped
                    // on the floor.
                    manager.record_failure(undialable.ticket, now_ms);
                    let _ = reply.send(Err(DialRefusal::Backend(undialable.reason)));
                    return;
                }
            };
            let id = admitted.connection_id();
            let answer = match swarm.dial(admitted) {
                Ok(ticket) => {
                    // Held until the outcome event settles it. Dropping
                    // it here would release the pending slot the instant
                    // the dial began, and the ceiling would bound
                    // nothing but the rate of the loop.
                    in_flight.insert(id, ticket);
                    Ok(())
                }
                Err(boxed) => {
                    let (e, ticket) = *boxed;
                    // A synchronous refusal produces no event, so the
                    // admission is settled here or never.
                    manager.record_failure(ticket, now_ms);
                    Err(DialRefusal::Backend(e.to_string()))
                }
            };
            let _ = reply.send(answer);
        }
        SwarmCommand::SetTrust { trust, reply } => {
            // EVICTION IS THE POINT. ADR-0012: removing a peer must
            // take away the connectivity it already has, not merely
            // what it would be granted next time. A trust change that
            // only affected future dials would leave a revoked peer
            // with a live session for as long as it kept talking.
            let live: Vec<TransportIdentity> = open.values().map(|c| c.peer.clone()).collect();
            let revoked = manager.set_trust(*trust, &live);

            let mut closed = 0_usize;
            for entry in &revoked {
                for (id, connection) in open.iter() {
                    if connection.peer == entry.peer {
                        refuse.push(*id);
                        closed = closed.saturating_add(1);
                    }
                }
            }
            let _ = reply.send(closed);
        }
        SwarmCommand::Shutdown { reply } => {
            let _ = reply.send(());
        }
    }
}

/// Translate a libp2p event into this crate's vocabulary.
///
/// Deliberately does NOT feed outcomes back into the `ConnectionPolicy`.
/// Recording a success or an address failure is the ConnectionManager's
/// job, and that arrives with Stage 5 along with the retry scheduler that
/// gives backoff something to act on. Recording here without a scheduler
/// would populate state nothing reads, and a half-wired feedback loop is
/// harder to reason about than an absent one.
fn translate(
    event: Libp2pSwarmEvent<SubstrateBehaviourEvent>,
    listens: &mut PendingListens,
    abandoned: &mut Vec<ListenerId>,
) -> Option<SwarmEvent> {
    match event {
        Libp2pSwarmEvent::NewListenAddr {
            listener_id,
            address,
        } => {
            // Answer the waiting `listen` with the address the OS
            // assigned. A listener may report several; the first answers
            // and the rest are ordinary events.
            if let Some(reply) = listens.remove(&listener_id)
                && reply.send(Ok(address.clone())).is_err()
            {
                // The caller is gone and this listener now belongs to
                // nobody. Close it rather than leave it bound.
                abandoned.push(listener_id);
            }
            Some(SwarmEvent::Listening { address })
        }
        // A listener that dies before binding must not leave `listen`
        // waiting for an address that will never arrive.
        Libp2pSwarmEvent::ListenerClosed { listener_id, .. } => {
            if let Some(reply) = listens.remove(&listener_id) {
                let _ = reply.send(Err("the listener closed before binding".to_owned()));
            }
            None
        }
        Libp2pSwarmEvent::ListenerError { listener_id, error } => {
            if let Some(reply) = listens.remove(&listener_id) {
                let _ = reply.send(Err(error.to_string()));
            }
            None
        }
        Libp2pSwarmEvent::ConnectionEstablished { peer_id, .. } => to_transport_identity(&peer_id)
            .ok()
            .map(|peer| SwarmEvent::Connected { peer }),
        Libp2pSwarmEvent::ConnectionClosed { peer_id, .. } => to_transport_identity(&peer_id)
            .ok()
            .map(|peer| SwarmEvent::Disconnected { peer }),
        Libp2pSwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            Some(SwarmEvent::DialFailed {
                peer: peer_id.as_ref().and_then(|p| to_transport_identity(p).ok()),
                detail: error.to_string(),
            })
        }
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Identify(
            identify::Event::Received { peer_id, info, .. },
        )) => to_transport_identity(&peer_id).ok().map(|peer| {
            SwarmEvent::Identified {
                peer,
                protocol_version: info.protocol_version,
                // ADVISORY. These are addresses the peer asserted about
                // itself; they are never authorization and never proof of
                // reachability.
                listen_addresses: info.listen_addrs,
            }
        }),
        _ => None,
    }
}
