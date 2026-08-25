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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::preauth::PreAuthLimits;
use interweave_transport_runtime::{
    ConnectionClass, ConnectionManager, ConnectionPolicy, ConnectionSlot, DialDenial, DialOrigin,
    DialRequest, DialTicket, Revoked, TrustSources,
};
use libp2p::core::transport::{ListenerId, TransportError};
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
    /// How often the reconnect scheduler looks for due retries.
    ///
    /// A period, not a deadline: a retry becomes due when the policy
    /// says so, and this is how long it may wait to be noticed. Short
    /// enough that the backoff is what determines the delay, long
    /// enough that an idle profile is not walking a table every
    /// moment.
    pub retry_tick: Duration,
    /// Most peers the scheduler will dial in one tick.
    ///
    /// The retry table is bounded, so the whole of it could come due at
    /// once -- and dialing all of it in one pass is a burst this
    /// profile inflicts on itself. The rest stay due and are taken next
    /// tick.
    pub max_retries_per_tick: usize,
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
            retry_tick: Duration::from_secs(1),
            max_retries_per_tick: 4,
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
            // A tick that dialed the whole table would be a burst; zero
            // is a scheduler that never dials, which is the state this
            // stage exists to leave.
            ("max_retries_per_tick", self.max_retries_per_tick, 1),
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
    /// Remember an address as a candidate for a peer.
    AddAddress {
        /// The peer the address belongs to.
        peer: TransportIdentity,
        /// The candidate address.
        address: Multiaddr,
        /// Answered with whether it was remembered.
        reply: oneshot::Sender<bool>,
    },
    /// Dial a peer at the best address already known for it.
    DialPeer {
        /// The peer to reach.
        peer: TransportIdentity,
        /// Answered when a dial is admitted, or with why none was.
        reply: oneshot::Sender<Result<(), DialRefusal>>,
    },
    /// Replace the trust sources, evicting what they no longer permit.
    SetTrust {
        /// Who this profile trusts, and for what.
        trust: Box<TrustSources>,
        /// Answered with the number of connections closed by the change.
        reply: oneshot::Sender<usize>,
    },
    /// Refuse new connectivity while keeping what is already up.
    Drain {
        /// Answered once the manager is draining.
        reply: oneshot::Sender<()>,
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
    /// Nothing is known about where to reach this peer.
    ///
    /// Distinct from a policy refusal on purpose: "I have no address"
    /// and "I have one and will not use it" are different problems and
    /// an operator fixes them differently.
    NoKnownAddress,
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
        // THE LOCAL IDENTITY FIRST, from the keypair rather than from
        // anything a caller supplied. A configuration that lists this
        // profile's own PeerId -- a copied allowlist, a template filled
        // in wrong -- would otherwise be classified as an ordinary
        // trusted remote and reach admission, retries and the address
        // book for a peer that cannot be dialed.
        manager.bind_local_peer(local_peer.clone());
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
        let started = tokio::time::Instant::now();

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

        // The scheduler's heartbeat. `Delay` rather than `Burst` so a
        // task that was busy does not then fire a backlog of ticks it
        // slept through, each one walking the retry table again.
        let mut retries = tokio::time::interval(config.retry_tick);
        retries.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
                    // THE RECONNECT SCHEDULER. `due_retries` used to
                    // be read-only: every call returned the SAME due
                    // entries until something else cleared them, which a
                    // scheduler tick never did. A slow dial still
                    // pending when the next tick fired got started
                    // again, unbounded, because nothing recorded that
                    // an attempt was already under way. `take_due_retries`
                    // CLAIMS what it returns, so a peer does not surface
                    // here a second time until its attempt settles.
                    //
                    // A tick rather than a timer per peer, because a
                    // timer per peer is a structure a remote party
                    // grows by failing to connect. The retry table is
                    // already bounded; this walks it.
                    _ = retries.tick() => {
                        let now = now_ms(started);
                        let due = manager.take_due_retries(now, config.max_retries_per_tick);
                        for peer in due {
                            let candidates = manager.dial_candidates(&peer, now);
                            if candidates.is_empty() {
                                // NOTHING TO TRY. Reconsidering this
                                // peer a moment later would not produce
                                // a different answer, and leaving the
                                // claim in place would mean it is never
                                // reconsidered at all -- both a stuck
                                // claim and an immediate re-offer are
                                // wrong; clearing it is the only answer
                                // that is not a starvation risk in
                                // either direction.
                                manager.clear_retry_claim(&peer);
                                continue;
                            }

                            // ADMITTED LIKE ANY OTHER DIAL, and
                            // attributed to the scheduler rather than
                            // to whoever asked first: a denial an
                            // operator sees must say which of the two
                            // it refused.
                            let mut last: Option<DialRefusal> = None;
                            let mut ticketed = false;
                            for address in candidates {
                                match attempt_dial(
                                    &mut swarm,
                                    &mut manager,
                                    &mut in_flight,
                                    &peer,
                                    &address,
                                    DialOrigin::ConnectionManager,
                                    now,
                                ) {
                                    Ok(()) => {
                                        // A ticket now owns the claim:
                                        // record_success/record_failure/
                                        // record_permanent_failure will
                                        // settle it when the outcome
                                        // arrives.
                                        //
                                        // RECLAIMED, because an earlier
                                        // candidate in this same loop
                                        // may have failed synchronously
                                        // and released the claim on the
                                        // way past. Leaving it released
                                        // with a dial in flight lets the
                                        // next tick start a second one
                                        // for the same peer.
                                        manager.reclaim_retry(&peer);
                                        ticketed = true;
                                        last = None;
                                        break;
                                    }
                                    // A POLICY DENIAL produced no ticket
                                    // at all, so nothing downstream will
                                    // ever settle this claim. "A denied
                                    // dial must not reset retry state"
                                    // applies here exactly as it does
                                    // for every other origin: the claim
                                    // is released, not rescheduled, so
                                    // the peer is offered again without
                                    // waiting out a fresh backoff it did
                                    // not earn.
                                    //
                                    // Authorization that no longer holds
                                    // is the one exception: retrying an
                                    // unauthorized or draining peer on
                                    // the very next tick would not
                                    // become true by waiting a second,
                                    // so that claim is cleared instead.
                                    Err(DialRefusal::Backend(reason)) => {
                                        last = Some(DialRefusal::Backend(reason));
                                    }
                                    Err(refusal @ DialRefusal::Policy(
                                        DialDenial::Unauthorized
                                        | DialDenial::NotAuthorizedForDataPlane
                                        | DialDenial::ShuttingDown,
                                    )) => {
                                        last = Some(refusal);
                                        break;
                                    }
                                    Err(refusal) => {
                                        last = Some(refusal);
                                    }
                                }
                            }
                            if !ticketed {
                                match &last {
                                    Some(DialRefusal::Policy(
                                        DialDenial::Unauthorized
                                        | DialDenial::NotAuthorizedForDataPlane
                                        | DialDenial::ShuttingDown,
                                    )) => manager.clear_retry_claim(&peer),
                                    _ => manager.release_retry_claim(&peer),
                                }
                            }
                            // REPORTED, because nobody asked for this
                            // dial and so nobody is holding a reply
                            // channel for it. A scheduled retry that
                            // failed silently would leave an operator
                            // watching a peer that never reconnects
                            // with nothing at all to look at.
                            //
                            // BOUNDED HERE TOO, and freshly checked
                            // rather than trusting the `room` computed
                            // once at the top of the loop: this branch
                            // is not gated by `if room` the way the
                            // Swarm-event branch is, and one tick can
                            // push up to `max_retries_per_tick` events
                            // in a single pass over `due` -- the outbox
                            // capacity that one stale bool described
                            // could be exhausted several pushes into the
                            // same loop. A stalled consumer combined
                            // with a peer that fails every tick would
                            // otherwise grow the outbox by one entry per
                            // tick forever, which is exactly the
                            // unbounded memory this channel exists to
                            // rule out. Dropped, not queued: the
                            // diagnostic is informational only, nothing
                            // downstream is waiting on it, and the
                            // outcome itself was already settled above
                            // regardless of whether this line runs.
                            if let Some(refusal) = last {
                                let has_room = outbox.len()
                                    < config.event_capacity.saturating_add(listens.len());
                                if has_room {
                                    outbox.push_back(SwarmEvent::DialFailed {
                                        peer: Some(peer.clone()),
                                        detail: format!("scheduled retry: {refusal:?}"),
                                    });
                                }
                            }
                        }
                    }
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
                        let announce = settle_outcome(
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
                        // TRANSLATED ONLY IF IT HAPPENED. `translate` is
                        // a shape conversion and knows nothing about
                        // admission, so without this a refused
                        // connection still reaches the consumer as
                        // `Connected` -- a peer announced as available
                        // immediately after the revocation that refused
                        // it.
                        let translated = match announce {
                            Announce::Yes => translate(event, &mut listens, &mut abandoned),
                            Announce::Suppress => {
                                // `translate` also answers pending
                                // `listen` calls, and a suppressed event
                                // is always a connection event, never a
                                // listener one -- so nothing is owed an
                                // answer here.
                                None
                            }
                        };
                        if let Some(event) = translated {
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

    /// Remember an address as a candidate for `peer`.
    ///
    /// Returns whether it was remembered: an unclassified peer gets no
    /// book entry, and a peer whose eight slots are all dialable keeps
    /// them.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone.
    pub async fn add_address(
        &self,
        peer: TransportIdentity,
        address: Multiaddr,
    ) -> Result<bool, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::AddAddress {
                peer,
                address,
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Dial `peer` at the best address known for it.
    ///
    /// Known-good routes are tried first, and each candidate is
    /// admitted on its own: the ordering is a preference, and a
    /// quarantined address is refused by the gate rather than skipped
    /// by the sort.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone. A
    /// refusal is `Ok(Err(..))`, including
    /// [`DialRefusal::NoKnownAddress`] when the book holds nothing for
    /// this peer.
    pub async fn dial_peer(
        &self,
        peer: TransportIdentity,
    ) -> Result<Result<(), DialRefusal>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::DialPeer { peer, reply })
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

    /// Refuse new connectivity, keeping the connections already up.
    ///
    /// After this, outbound admission answers
    /// [`DialDenial::ShuttingDown`] and no inbound connection is
    /// retained. Existing connections are untouched: a node leaving
    /// service stops taking new work before it drops the work it has.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone.
    pub async fn drain(&self) -> Result<(), SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::Drain { reply })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
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

/// Admit one dial, bind it to its ticket, and hand it to the Swarm.
///
/// The single place a dial happens, whoever asked: the command path,
/// the address-book path, and the retry scheduler all arrive here. A
/// second copy of this sequence is how one of them would end up
/// skipping the ticket, the binding, or the settlement.
fn attempt_dial(
    swarm: &mut GatedSwarm,
    manager: &mut ConnectionManager,
    in_flight: &mut HashMap<libp2p::swarm::ConnectionId, DialTicket>,
    peer: &TransportIdentity,
    address: &str,
    origin: DialOrigin,
    now_ms: u64,
) -> Result<(), DialRefusal> {
    let request = DialRequest {
        peer: Some(peer.clone()),
        address: address.to_owned(),
        origin,
    };
    // ADMITTED BEFORE A SOCKET IS OPENED. A quarantined address costs
    // nothing, which is the whole point of checking here rather than
    // after the connection fails.
    //
    // THE CLASS IS NOT THIS SITE'S TO ASSERT. It used to be a hardcoded
    // `DataPlaneTrusted` on every dial, which is the ADR-0036
    // separation stated in the policy and discarded by its only caller.
    // The gate classifies from the trust sources it publishes, and
    // there is no longer an argument through which a call site could
    // say otherwise.
    let ticket = manager
        .handle()
        .admit(&request, now_ms)
        .map_err(DialRefusal::Policy)?;

    // DERIVED FROM THE ADMISSION, not paired with it. The destination
    // is read back out of the ticket rather than rebuilt from the
    // caller's own peer and address, so there is no second copy of the
    // destination that could disagree with the one the gate admitted.
    let admitted = match AdmittedDial::from_ticket(ticket) {
        Ok(a) => a,
        Err(boxed) => {
            let undialable = *boxed;
            // The refusal is still an admission that reserved a slot,
            // so it is settled here rather than dropped on the floor.
            manager.record_failure(undialable.ticket, now_ms);
            return Err(DialRefusal::Backend(undialable.reason));
        }
    };
    let id = admitted.connection_id();
    match swarm.dial(admitted) {
        Ok(ticket) => {
            // Held until the outcome event settles it. Dropping it here
            // would release the pending slot the instant the dial
            // began, and the ceiling would bound nothing but the rate
            // of the loop.
            in_flight.insert(id, ticket);
            Ok(())
        }
        Err(boxed) => {
            let (e, ticket) = *boxed;
            // A synchronous refusal produces no event, so the admission
            // is settled here or never.
            if is_permanent_dial_error(&e) {
                manager.record_permanent_failure(ticket, now_ms);
            } else {
                manager.record_failure(ticket, now_ms);
            }
            Err(DialRefusal::Backend(e.to_string()))
        }
    }
}

/// Whether `error` describes THIS PROCESS's transport stack rather than
/// the remote end's availability.
///
/// `MultiaddrNotSupported` is libp2p's own name for "no configured
/// transport understands this address" -- a UDP address handed to a
/// TCP-only Swarm, for instance. It is not a fact about the network:
/// the same address fails the same way every time, on every attempt,
/// whatever the remote end does. Retrying it is not a smaller version
/// of retrying a timed-out connection; it is retrying a question this
/// process has already answered.
///
/// `DialError::Transport` carries one entry per address the dial
/// considered, so ALL of them must be the structural kind for the whole
/// attempt to be structural -- a mix means at least one address reached
/// the network and failed there, which is the ordinary case
/// `record_failure` exists for.
fn is_permanent_dial_error(error: &DialError) -> bool {
    match error {
        DialError::NoAddresses | DialError::LocalPeerId { .. } => true,
        DialError::Transport(attempts) => {
            !attempts.is_empty()
                && attempts
                    .iter()
                    .all(|(_, e)| matches!(e, TransportError::MultiaddrNotSupported(_)))
        }
        _ => false,
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
) -> Announce {
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
                return Announce::Suppress;
            };
            match in_flight.remove(connection_id) {
                // Outbound: the slot was reserved when the dial was
                // admitted, and the connection takes it over.
                Some(ticket) => {
                    // REVALIDATED, not merely recorded. Admission
                    // happened when the dial was ADMITTED; the
                    // handshake that just finished could have taken
                    // long enough for a trust revocation or a drain to
                    // land in between. Retaining the connection because
                    // it was admitted once would hold it open under
                    // authority that no longer exists -- the exact
                    // outbound counterpart of the check inbound already
                    // gets below.
                    // THE ORIGIN IS PART OF THE QUESTION. An
                    // infrastructure-only peer is authorized for
                    // reachability and refused for the data plane, so
                    // asking only what the peer is authorized FOR --
                    // the inbound predicate, which has no origin to
                    // consult -- closed relay reservations, relay
                    // circuits, AutoNAT probes and DCUtR hole punches
                    // that admission had correctly permitted.
                    let class = manager.classify(&peer);
                    if !manager.authorizes_for(class, ticket.origin()) {
                        manager.record_authorization_withdrawn(ticket, now_ms);
                        refuse.push(*connection_id);
                        return Announce::Suppress;
                    }
                    // THE ADDRESS THAT WORKED. Learned from the ticket
                    // rather than from anything the peer said, so a
                    // route this profile has actually authenticated is
                    // in the book even if the peer never advertises it.
                    let address = ticket.address().to_owned();
                    let origin = ticket.origin();
                    let slot = manager.record_success(ticket, now_ms);
                    let _ = manager.learn_address(&peer, &address, now_ms);
                    open.insert(
                        *connection_id,
                        OpenConnection {
                            peer,
                            slot,
                            origin: Some(origin),
                        },
                    );
                }
                // INBOUND HAS NO ADMISSION. ADR-0011: the same current
                // authorization that governs outbound applies before an
                // inbound data-plane connection is retained -- arriving
                // is not an authorization. The ceiling is the second
                // question, because a connection this profile will not
                // keep should not spend a slot to find that out.
                None => {
                    let class = manager.classify(&peer);
                    if !manager.authorizes(class) {
                        refuse.push(*connection_id);
                        return Announce::Suppress;
                    }
                    match manager.admit_inbound() {
                        Some(slot) => {
                            open.insert(
                                *connection_id,
                                OpenConnection {
                                    peer,
                                    slot,
                                    origin: None,
                                },
                            );
                        }
                        None => {
                            refuse.push(*connection_id);
                            return Announce::Suppress;
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
            //
            // The SAME condition decides whether to announce it. A
            // connection this runtime refused was never announced as
            // `Connected`, so announcing its close would hand a
            // consumer a `Disconnected` for a peer it was never told
            // about -- which reads as a peer going away rather than as
            // one that was never admitted.
            let Some(connection) = open.remove(connection_id) else {
                return Announce::Suppress;
            };
            manager.record_connection_closed(connection.slot);
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
                } else if is_permanent_dial_error(error) {
                    // STRUCTURAL, not transient. The same address fails
                    // the same way every time this process asks, so
                    // treating it as an ordinary network failure --
                    // punitive backoff, a rescheduled retry -- retries a
                    // thing retrying cannot fix. The paused-time
                    // scheduler test caught this: a UDP address on a
                    // TCP-only Swarm was retried forever.
                    manager.record_permanent_failure(ticket, now_ms);
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
        // ADVISORY, and bounded. These are addresses the peer asserted
        // about itself: not authorization, not proof of reachability,
        // and not permission to dial -- every dial still passes
        // admission. Remembered only for a peer the trust sources
        // classify, and at most eight of them, because the list is
        // written by the party being described.
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Identify(
            identify::Event::Received { peer_id, info, .. },
        )) => {
            if let Ok(peer) = to_transport_identity(peer_id) {
                for address in &info.listen_addrs {
                    let _ = manager.learn_address(&peer, &address.to_string(), now_ms);
                }
            }
        }
        _ => {}
    }
    Announce::Yes
}

/// Whether the event this runtime just settled should be reported to
/// the consumer.
///
/// A connection REFUSED at establishment -- authorization withdrawn
/// mid-handshake, an inbound peer this profile will not retain, a
/// ceiling with no room, a PeerId the neutral grammar rejects -- was
/// settled and queued for closing, but `translate` is a pure shape
/// conversion and would happily emit `Connected` for it anyway. A
/// consumer would then see a peer become available and start work
/// against it, moments before the close it was never told was coming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Announce {
    /// Report it: the ordinary case.
    Yes,
    /// Say nothing. This connection is not one the consumer was told
    /// about, and telling it now would describe a state that never
    /// existed.
    Suppress,
}

/// Milliseconds since the runtime task started.
///
/// Monotonic and relative. The policy is a state machine over elapsed
/// time, so an origin of zero is as good as any epoch and immune to a
/// wall-clock adjustment moving a quarantine deadline.
fn now_ms(started: tokio::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Which open connections a trust change actually withdraws.
///
/// THE NEW CLASS, PER CONNECTION, AGAINST ITS OWN ORIGIN. ADR-0036's
/// separation is an origin/class PAIR, so this cannot be decided from
/// the class alone. A peer in both trust sets that loses only its
/// data-plane trust is still infrastructure: `set_trust` reports it
/// revoked, while `authorizes_for` goes on permitting its relay
/// reservation, relay circuit and AutoNAT probes. Closing every
/// connection to a reported peer dropped exactly those -- the
/// reachability that peer is still trusted for.
///
/// Inbound carries no origin because arriving is not a dial. It was
/// admitted by the origin-less `authorizes` and is re-asked the same
/// question, so a revocation that reaches the data plane still closes
/// it.
fn connections_to_close<'a>(
    manager: &ConnectionManager,
    revoked: &[Revoked],
    open: impl Iterator<
        Item = (
            libp2p::swarm::ConnectionId,
            &'a TransportIdentity,
            Option<DialOrigin>,
        ),
    >,
) -> BTreeSet<libp2p::swarm::ConnectionId> {
    let revoked_class: BTreeMap<&TransportIdentity, ConnectionClass> = revoked
        .iter()
        .map(|entry| (&entry.peer, entry.now))
        .collect();
    let mut closing = BTreeSet::new();
    for (id, peer, origin) in open {
        let Some(now) = revoked_class.get(peer) else {
            continue;
        };
        let still_authorized = match origin {
            Some(origin) => manager.authorizes_for(*now, origin),
            None => manager.authorizes(*now),
        };
        if !still_authorized {
            closing.insert(id);
        }
    }
    closing
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
    /// Why this connection was opened, or `None` for one that arrived.
    ///
    /// ADR-0036's separation is an origin/class PAIR, so a trust change
    /// cannot be re-evaluated from the class alone. Without this a peer
    /// that lost only its data-plane trust -- still infrastructure --
    /// had every connection to it closed, including relay reservations
    /// and AutoNAT probes that `authorizes_for` would still permit.
    ///
    /// Inbound is `None` because arriving is not a dial: it was admitted
    /// with the origin-less `authorizes`, and it is re-evaluated the
    /// same way.
    origin: Option<DialOrigin>,
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
            // A COMMAND IS A PERSON OR AN ADMIN API. Reporting it as
            // the scheduler's own dial made a denial unable to say
            // which of the two it refused, and those are the two an
            // operator most needs told apart.
            let answer = attempt_dial(
                swarm,
                manager,
                in_flight,
                &peer,
                &address.to_string(),
                DialOrigin::Manual,
                now_ms,
            );
            let _ = reply.send(answer);
        }
        SwarmCommand::AddAddress {
            peer,
            address,
            reply,
        } => {
            let _ = reply.send(manager.learn_address(&peer, &address.to_string(), now_ms));
        }
        SwarmCommand::DialPeer { peer, reply } => {
            // KNOWN-GOOD FIRST, and every candidate still admitted
            // individually: the ordering is a preference, and a
            // quarantined address that sorts last is refused by the
            // gate rather than by the sort.
            let candidates = manager.dial_candidates(&peer, now_ms);
            if candidates.is_empty() {
                let _ = reply.send(Err(DialRefusal::NoKnownAddress));
                return;
            }
            let mut answer = Err(DialRefusal::NoKnownAddress);
            for address in &candidates {
                answer = attempt_dial(
                    swarm,
                    manager,
                    in_flight,
                    &peer,
                    address,
                    DialOrigin::Manual,
                    now_ms,
                );
                if answer.is_ok() {
                    break;
                }
            }
            let _ = reply.send(answer);
        }
        SwarmCommand::SetTrust { trust, reply } => {
            // EVICTION IS THE POINT. ADR-0012: removing a peer must
            // take away the connectivity it already has, not merely
            // what it would be granted next time. A trust change that
            // only affected future dials would leave a revoked peer
            // with a live session for as long as it kept talking.
            // UNIQUE PEERS for the classification. `open` is keyed by
            // CONNECTION, and one peer can hold several -- collecting
            // its values produced a duplicate entry per connection, so
            // `set_trust` reported the same revocation more than once
            // and the nested scan below then closed each connection
            // once per duplicate. Two connections to one revoked peer
            // reported four closures against two real connections.
            let live: BTreeSet<TransportIdentity> = open.values().map(|c| c.peer.clone()).collect();
            let live: Vec<TransportIdentity> = live.into_iter().collect();
            let revoked = manager.set_trust(*trust, &live);

            // UNIQUE CONNECTIONS for the closing and the count. Every
            // connection is named at most once however many revoked
            // entries match it, so the number reported is the number of
            // connections actually closed.
            let closing = connections_to_close(
                manager,
                &revoked,
                open.iter().map(|(id, c)| (*id, &c.peer, c.origin)),
            );
            let closed = closing.len();
            refuse.extend(closing);
            let _ = reply.send(closed);
        }
        SwarmCommand::Drain { reply } => {
            // DRAINING IS NOT STOPPING. Existing connections stay up --
            // a node going out of service should stop taking on new
            // work before it drops the work it has. `begin_shutdown`
            // publishes the flag every snapshot reads live, so a holder
            // that took its snapshot a moment ago is draining too.
            manager.begin_shutdown();
            let _ = reply.send(());
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

#[cfg(test)]
mod tests {
    use super::{connections_to_close, is_permanent_dial_error};
    use interweave_transport_api::TransportIdentity;
    use interweave_transport_runtime::{
        ConnectionManager, ConnectionPolicy, DialOrigin, TrustSources,
    };
    use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
    use libp2p::Multiaddr;
    use libp2p::core::transport::TransportError;
    use libp2p::swarm::{ConnectionId, DialError};

    const RELAY: &str = "12D3KooWCLxLXFHqvfsHVLDcNsSpZBQq1M1KMRgQRLLLnHTv7oQD";

    fn ident(text: &str) -> TransportIdentity {
        TransportIdentity::parse(text).expect("a valid peer id")
    }

    fn manager(data_plane: &[&str], infrastructure: &[&str]) -> ConnectionManager {
        let mut m = ConnectionManager::new(ConnectionPolicy::default(), 8);
        m.set_trust(trust(data_plane, infrastructure), &[]);
        m
    }

    fn trust(data_plane: &[&str], infrastructure: &[&str]) -> TrustSources {
        TrustSources::new(
            PeerTrustPolicy::new(data_plane.iter().map(|p| ident(p))).expect("small"),
            InfrastructureSet::new(infrastructure.iter().map(|p| ident(p))).expect("small"),
        )
    }

    /// A peer trusted BOTH ways loses only its data-plane trust.
    ///
    /// ADR-0036 keeps the two authorizations separate, so this peer is
    /// still infrastructure and its relay reservation is still
    /// authorized. Deciding from the class alone -- which is what
    /// closing every connection to a reported peer does -- drops the
    /// reachability the peer is still trusted for.
    #[test]
    fn partial_revocation_keeps_the_reachability_it_still_authorizes() {
        let mut m = manager(&[RELAY], &[RELAY]);
        let peer = ident(RELAY);
        let revoked = m.set_trust(trust(&[], &[RELAY]), &[peer.clone()]);
        assert_eq!(revoked.len(), 1, "the data-plane loss IS a revocation");

        let reservation = ConnectionId::new_unchecked(1);
        let closing = connections_to_close(
            &m,
            &revoked,
            [(reservation, &peer, Some(DialOrigin::RelayReservation))].into_iter(),
        );
        assert!(
            closing.is_empty(),
            "an infrastructure peer keeps the connection it is still authorized for"
        );
    }

    /// The other half: the same revocation MUST close the data plane.
    ///
    /// Without this, "keep reachability" is satisfied by keeping
    /// everything, which is the bug in the opposite direction.
    #[test]
    fn partial_revocation_still_closes_the_data_plane() {
        let mut m = manager(&[RELAY], &[RELAY]);
        let peer = ident(RELAY);
        let revoked = m.set_trust(trust(&[], &[RELAY]), &[peer.clone()]);

        let data = ConnectionId::new_unchecked(2);
        let closing = connections_to_close(
            &m,
            &revoked,
            [(data, &peer, Some(DialOrigin::ConnectionManager))].into_iter(),
        );
        assert!(
            closing.contains(&data),
            "the data-plane connection is exactly what was withdrawn"
        );
    }

    /// Inbound carries no origin, and is re-asked the question it was
    /// admitted with rather than being kept by default.
    #[test]
    fn an_inbound_connection_is_reevaluated_without_an_origin() {
        let mut m = manager(&[RELAY], &[RELAY]);
        let peer = ident(RELAY);
        let revoked = m.set_trust(trust(&[], &[RELAY]), &[peer.clone()]);

        let inbound = ConnectionId::new_unchecked(3);
        let closing = connections_to_close(&m, &revoked, [(inbound, &peer, None)].into_iter());
        assert!(
            closing.contains(&inbound),
            "arriving is not an authorization: the data-plane loss closes it"
        );
    }

    /// A peer that was not revoked at all is untouched, whatever its
    /// origin.
    #[test]
    fn a_peer_that_kept_its_trust_keeps_every_connection() {
        let mut m = manager(&[RELAY], &[RELAY]);
        let peer = ident(RELAY);
        let revoked = m.set_trust(trust(&[RELAY], &[RELAY]), &[peer.clone()]);
        assert!(revoked.is_empty(), "nothing changed, nothing revoked");

        let closing = connections_to_close(
            &m,
            &revoked,
            [(ConnectionId::new_unchecked(4), &peer, None)].into_iter(),
        );
        assert!(closing.is_empty());
    }

    fn addr() -> Multiaddr {
        "/ip4/127.0.0.1/tcp/1".parse().expect("valid")
    }

    fn unsupported() -> (Multiaddr, TransportError<std::io::Error>) {
        (addr(), TransportError::MultiaddrNotSupported(addr()))
    }

    fn network(kind: std::io::ErrorKind) -> (Multiaddr, TransportError<std::io::Error>) {
        (addr(), TransportError::Other(std::io::Error::from(kind)))
    }

    #[test]
    fn a_single_unsupported_address_is_permanent() {
        assert!(is_permanent_dial_error(&DialError::Transport(vec![
            unsupported()
        ])));
    }

    #[test]
    fn a_single_network_failure_is_not_permanent() {
        assert!(!is_permanent_dial_error(&DialError::Transport(vec![
            network(std::io::ErrorKind::ConnectionRefused)
        ])));
    }

    #[test]
    fn one_network_failure_among_several_unsupported_addresses_is_not_permanent() {
        // THE quantifier this classification rests on. A dial that
        // tried several addresses and reached the network on even one
        // of them is not a structural failure -- `.all()`, not `.any()`,
        // is what a mix has to fall through to `record_failure` rather
        // than being cleared as unfixable.
        assert!(!is_permanent_dial_error(&DialError::Transport(vec![
            unsupported(),
            network(std::io::ErrorKind::TimedOut),
        ])));
    }

    #[test]
    fn every_address_unsupported_is_permanent_even_with_several() {
        assert!(is_permanent_dial_error(&DialError::Transport(vec![
            unsupported(),
            unsupported(),
        ])));
    }

    #[test]
    fn no_addresses_is_permanent() {
        assert!(is_permanent_dial_error(&DialError::NoAddresses));
    }

    #[test]
    fn dialing_the_local_peer_is_permanent() {
        assert!(is_permanent_dial_error(&DialError::LocalPeerId {
            address: addr()
        }));
    }

    #[test]
    fn a_timeout_is_not_permanent() {
        assert!(!is_permanent_dial_error(&DialError::Aborted));
    }
}
