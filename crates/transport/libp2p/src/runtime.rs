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

use futures::StreamExt as _;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{
    ConnectionClass, ConnectionPolicy, DialDenial, DialOrigin, DialRequest,
};
use libp2p::core::transport::ListenerId;
use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::{Multiaddr, PeerId, Swarm, identify, noise, tcp, yamux};
use tokio::sync::{mpsc, oneshot};

use crate::behaviour::{SubstrateBehaviour, SubstrateBehaviourEvent};

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
    ) -> Result<Self, SubstrateError> {
        // BEFORE anything is built. `mpsc::channel(0)` panics, and a
        // half-constructed Swarm would still have opened sockets.
        config.validate()?;

        let keypair = identity.swarm_keypair();
        let local_peer = to_transport_identity(&PeerId::from_public_key(&keypair.public()))?;

        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| SubstrateError::Transport(e.to_string()))?
            .with_behaviour(|key| SubstrateBehaviour::new(key.public()))
            .map_err(|e| SubstrateError::Transport(e.to_string()))?
            .with_swarm_config(|c| c.with_idle_connection_timeout(config.idle_timeout))
            .build();

        // The Stage 2 policy, driving a real dial path from the first
        // line of substrate code rather than being wired in later.
        let mut policy = ConnectionPolicy::new(config.max_pending_dials, config.max_connections);

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
                                handle_command(
                                    &mut swarm,
                                    &mut policy,
                                    &mut listens,
                                    config.max_pending_listens,
                                    command,
                                );
                            }
                        }
                    }
                    event = swarm.select_next_some(), if room => {
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

/// Listen commands whose bound address has not arrived yet.
type PendingListens = HashMap<ListenerId, oneshot::Sender<Result<Multiaddr, String>>>;

fn handle_command(
    swarm: &mut Swarm<SubstrateBehaviour>,
    policy: &mut ConnectionPolicy,
    listens: &mut PendingListens,
    max_pending_listens: usize,
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
                origin: DialOrigin::ConnectionManager,
            };
            // ADMITTED BEFORE A SOCKET IS OPENED. A quarantined address
            // costs nothing, which is the whole point of checking here
            // rather than after the connection fails.
            if let Err(denial) = policy.admit(&request, ConnectionClass::DataPlaneTrusted, 0) {
                let _ = reply.send(Err(DialRefusal::Policy(denial)));
                return;
            }
            // BOUND TO THE EXPECTED IDENTITY. Dialling a bare address
            // tells libp2p nothing about who should be there, so a server
            // at that address can complete a Noise handshake with any key
            // and the connection is accepted. Building the dial with the
            // PeerId makes the mismatch libp2p's problem: it refuses the
            // connection instead of reporting `Connected` for a peer
            // nobody asked for.
            //
            // This is what the command's documentation always claimed and
            // the code did not do.
            let Ok(expected) = peer.as_str().parse::<PeerId>() else {
                let _ = reply.send(Err(DialRefusal::Backend(format!(
                    "expected peer {} is not a libp2p PeerId",
                    peer.as_str()
                ))));
                return;
            };
            let opts = DialOpts::peer_id(expected)
                .addresses(vec![address])
                .condition(PeerCondition::Always)
                .build();
            let answer = match swarm.dial(opts) {
                Ok(()) => Ok(()),
                Err(e) => Err(DialRefusal::Backend(e.to_string())),
            };
            let _ = reply.send(answer);
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
