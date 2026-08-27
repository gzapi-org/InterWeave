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

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportError as DirectError;
use interweave_transport_api::{
    DirectMessageV2, DirectRejectReason, EndpointId, TransportIdentity,
};
use interweave_transport_runtime::direct_inbound::{
    AdmissionContext, Outcome as AdmissionOutcome, admit_prefix, admit_structured,
};
use interweave_transport_runtime::endpoint_queue::EndpointQueues;
use interweave_transport_runtime::endpoint_registry::{EndpointRegistry, RegisteredEndpoint};
use interweave_transport_runtime::{
    ConnectionClass, ConnectionManager, ConnectionPolicy, DialDenial, DialOrigin, DialTicket,
    TrustSources,
};
use libp2p::core::transport::ListenerId;
use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;
use libp2p::{Multiaddr, PeerId, identify, noise, tcp, yamux};
use tokio::sync::{mpsc, oneshot};

use crate::behaviour::{SubstrateBehaviour, SubstrateBehaviourEvent};
use crate::gated_swarm::GatedSwarm;
use crate::gated_swarm::NotConnected;

mod config;
mod dialing;
mod messages;

// Re-exported so `lib.rs` and every call site keep the paths they had:
// this split moved code, not the public surface.
use dialing::{
    Announce, OpenConnection, PendingListens, attempt_dial, connections_to_close, now_ms,
    settle_outcome, wall_ms,
};

pub use messages::{DialRefusal, SwarmCommand, SwarmEvent};

pub use config::{
    DEFAULT_COMMAND_CAPACITY, DEFAULT_EVENT_CAPACITY, MAX_CONFIGURED_CAPACITY, SubstrateConfig,
    SubstrateError,
};

/// The reverse conversion.
///
/// Fallible for the same reason the forward one is: the neutral grammar
/// is deliberately looser than libp2p's multihash parse — it checks
/// prefix, alphabet and length — so a value this crate accepts is not
/// automatically one libp2p can turn back into a PeerId.
fn to_peer_id(peer: &TransportIdentity) -> Result<PeerId, ()> {
    peer.as_str().parse::<PeerId>().map_err(|_| ())
}

fn to_transport_identity(peer: &PeerId) -> Result<TransportIdentity, SubstrateError> {
    TransportIdentity::parse(peer.to_base58()).map_err(|e| SubstrateError::Identity(e.to_string()))
}

/// One outbound direct exchange awaiting its answer.
struct PendingDirect {
    /// The id this exchange sent, which the answer must echo.
    message_id: interweave_transport_api::MessageId,
    /// The destination asked for, when one was named.
    ///
    /// `None` is an omitted destination, where the remote's resolved
    /// endpoint is the ANSWER rather than something to check — it is how
    /// the caller learns the default. An explicit one is a question with
    /// exactly one correct reply.
    requested: Option<EndpointId>,
    /// The caller waiting for the outcome.
    reply: oneshot::Sender<Result<EndpointId, DirectError>>,
    /// Who the exchange is with, so the per-peer bound can be counted.
    peer: TransportIdentity,
}

/// Whether another outbound direct exchange may start.
///
/// Takes the in-flight peers as an ITERATOR rather than the pending
/// table, so the decision can be tested against a population this test
/// builds instead of one only the swarm can produce —
/// `OutboundRequestId` has no public constructor, and a rule that can
/// only be exercised through the network is a rule tested by luck.
/// SPIKE-002 finding 1 settled that class: filling 128 in-flight
/// exchanges over loopback means racing the responder.
///
/// One pass, counting both bounds together, because the iterator is
/// consumed and two passes would need it cloned or collected.
fn admit_outbound<'a>(
    in_flight: impl Iterator<Item = &'a TransportIdentity>,
    peer: &TransportIdentity,
) -> Result<(), DirectError> {
    let mut total: usize = 0;
    let mut for_peer: usize = 0;
    for held in in_flight {
        total = total.saturating_add(1);
        if held == peer {
            for_peer = for_peer.saturating_add(1);
        }
    }
    if total >= MAX_OUTBOUND_DIRECT {
        return Err(DirectError::Overloaded);
    }
    if for_peer >= MAX_OUTBOUND_DIRECT_PER_PEER {
        return Err(DirectError::Overloaded);
    }
    Ok(())
}

/// How long `shutdown` lets already-dispatched exchanges finish.
///
/// `DIRECT.md`: "allow existing exchanges a short bounded grace, then
/// close." Shorter than the ten-second direct deadline on purpose — this
/// is a grace for exchanges already in flight, not a second deadline for
/// them, and a caller that asked to stop should not wait out the full
/// protocol timeout to find out that it has.
///
/// The contract's daemon-facing `shutdown(grace)` takes this as a
/// parameter. Stage 6 has no daemon, so it is a constant here and
/// becomes an argument when the API that needs it exists.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether a shutdown that is waiting out its grace may finish now.
///
/// Two ways out, and the caller is answered on either: everything in
/// flight has settled, or the deadline passed.
///
/// BOTH DIRECTIONS COUNT. `pending_direct` tracks outbound exchanges
/// only, so a verdict reading it alone would break the loop while an
/// admitted request's answer was still queued — dropping the accepted
/// queue and the unsent response, and leaving the sender to retry into
/// a node that had already accepted it.
///
/// A function because the window it protects cannot be reached over a
/// real socket: a response small enough to fit the kernel's send buffer
/// completes even against a peer that has stopped reading, so
/// `ResponseSent` arrives before any shutdown could race it. The
/// arithmetic is testable even where the race is not.
const fn shutdown_settled(
    pending_outbound: usize,
    answering_inbound: usize,
    past_deadline: bool,
) -> bool {
    past_deadline || (pending_outbound == 0 && answering_inbound == 0)
}

/// Whether the Swarm may be polled.
///
/// The outbox is bounded so a stalled consumer cannot make this process
/// buffer without limit — but polling is also what SETTLES the callers
/// waiting on in-flight work, so the bound has to leave room for them or
/// it stops the very progress that would drain it.
///
/// Three kinds of caller earn that room: listeners awaiting an address,
/// outbound exchanges awaiting a response, and inbound requests whose
/// answer is queued but not yet written. All three are reachable only
/// through admission, which is itself rate limited, and the first two
/// are separately bounded (`max_pending_listens`, and `admit_outbound`
/// at 128).
///
/// THIS PREDICATE HAS BEEN WRONG THREE TIMES, which is why it is a
/// function with tests rather than an expression inside a `select!` arm.
/// It counted listeners and not outbound exchanges, and `send_direct`
/// froze past its deadline. Its slack was then shared with delivery
/// events, so a peer could refill it and reach the same freeze. And it
/// omitted inbound answers on the argument that their count is loosely
/// bounded — which traded a liveness failure for a tidier bound: an
/// admitted message's response could never be written, and the remote
/// timed out and retried until an unrelated local consumer drained.
const fn polling_room(
    buffered: usize,
    event_capacity: usize,
    pending_listens: usize,
    pending_exchanges: usize,
    answering_inbound: usize,
) -> bool {
    buffered
        < event_capacity
            .saturating_add(pending_listens)
            .saturating_add(pending_exchanges)
            .saturating_add(answering_inbound)
}

/// Whether an accepted delivery may be buffered for the consumer.
///
/// The BASE capacity only: every slot [`polling_room`] adds beyond it is
/// reserved for progress and never spent on notifications. A delivery
/// buffered into that slack is a peer taking a slot this process needs
/// to settle its own work.
///
/// THAT INCLUDES THE LISTENER SLACK, which this used to share. The
/// argument for sharing was that "a pending listener is waiting on a
/// command reply rather than on a response the Swarm must carry" — and
/// it is wrong. A listener waits for `NewListenAddr`, which is a Swarm
/// event and needs a slot in this same outbox. Letting a delivery take
/// it means `listen()` waits for an address that arrives only once some
/// unrelated consumer drains.
///
/// Refusing here drops a NOTIFICATION, not a message. The event is
/// already in the endpoint's bounded queue — the admission `AcceptedV2`
/// promised (ADR-0018) — and `drain_endpoint` still returns it. What is
/// lost under sustained backpressure is a wake-up, and only for a
/// consumer that by construction is not reading.
const fn may_buffer_delivery(buffered: usize, event_capacity: usize) -> bool {
    buffered < event_capacity
}

/// Outbound direct exchanges allowed at once, in total.
///
/// The `direct inflight total` row of `resource-limits.md` (128, ceiling
/// 512). It is a DIFFERENT row from the dedup reservation limits, which
/// happen to carry the same numbers and bound inbound work instead —
/// naming them separately is what stops one being "fixed" to match the
/// other.
const MAX_OUTBOUND_DIRECT: usize = 128;

/// Outbound direct exchanges allowed at once with any one peer.
///
/// The `direct inflight/peer` row (8, ceiling 32).
const MAX_OUTBOUND_DIRECT_PER_PEER: usize = 8;

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
        let _ = manager.set_trust(trust.clone(), &[]);

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

        // Outbound direct exchanges awaiting an answer.
        //
        // Bounded by the request-response behaviour's own concurrency
        // and by the command channel that feeds it — every entry was put
        // here by a LOCAL caller, never by a remote party, so this is not
        // a structure a peer can grow.
        // Inbound direct admission. Empty of endpoints until a local
        // session claims a lease, which is the correct posture for a
        // daemon that has just started (testing.md scenario 27).
        let mut direct_state = DirectState::new(now_ms(started));
        // FROM THE SAME SOURCES the manager just took. Two copies of
        // "who may talk to us" is two answers that will eventually
        // differ, and the one a directed message meets must be the one
        // that admitted its connection.
        direct_state.adopt_trust(&trust);

        // A shutdown that is waiting for in-flight exchanges: the
        // deadline it must not pass, and the caller to answer when it
        // finishes or expires.
        let mut stopping: Option<(tokio::time::Instant, oneshot::Sender<()>)> = None;
        let mut pending_direct: HashMap<
            libp2p::request_response::OutboundRequestId,
            PendingDirect,
        > = HashMap::new();

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
                // PENDING DIRECT EXCHANGES BUY SLACK TOO, for exactly
                // the reason pending listeners do — the comment above
                // names the deadlock and then counts only one of the two
                // callers it applies to. A dispatched direct request is
                // answered only by Swarm progress, so a full outbox
                // stopping that progress leaves `send_direct` waiting
                // past its own deadline with nothing able to settle it.
                // A remote peer can drive the outbox full on its own:
                // every accepted delivery appends a `DirectDelivered`.
                //
                // The slack stays bounded because `pending_direct` is
                // bounded — `admit_outbound` caps it at 128 — so this
                // cannot become the unbounded queue the capacity exists
                // to rule out.
                // A GRACE THAT IS OVER ENDS THE LOOP. Checked before the
                // select rather than inside it, so both ways out — the
                // last exchange settling, and the deadline passing —
                // leave through one place and answer the caller once.
                if let Some((deadline, _)) = &stopping
                    && shutdown_settled(
                        pending_direct.len(),
                        direct_state.answering(),
                        tokio::time::Instant::now() >= *deadline,
                    )
                {
                    if let Some((_, reply)) = stopping.take() {
                        let _ = reply.send(());
                    }
                    break;
                }

                // The deadline, when a shutdown is waiting out its
                // grace. `None` the rest of the time, and the select
                // branch below is inert then.
                let grace_deadline = stopping.as_ref().map(|(deadline, _)| *deadline);

                let room = polling_room(
                    outbox.len(),
                    config.event_capacity,
                    listens.len(),
                    pending_direct.len(),
                    direct_state.answering(),
                );

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
                                // NOTHING IN FLIGHT IS THE COMMON CASE,
                                // and it still stops immediately.
                                if shutdown_settled(
                                    pending_direct.len(),
                                    direct_state.answering(),
                                    false,
                                ) || stopping.is_some()
                                {
                                    let _ = reply.send(());
                                    break;
                                }
                                // EXCHANGES ALREADY DISPATCHED GET A
                                // BOUNDED GRACE. Breaking here drops
                                // every `PendingDirect`, so each caller
                                // that had already reached the wire is
                                // answered `Stopped` — a shutdown
                                // cancelling work it had accepted.
                                // `DIRECT.md` asks for the opposite:
                                // stop taking new work, let existing
                                // exchanges finish briefly, then close.
                                //
                                // `begin_shutdown` is what stops the new
                                // work, and it is the same flag the
                                // drain path already reads on both
                                // directions — so no second notion of
                                // "closing" is introduced here.
                                manager.begin_shutdown();
                                stopping = Some((
                                    tokio::time::Instant::now() + SHUTDOWN_GRACE,
                                    reply,
                                ));
                            }
                            Some(command) => {
                                let mut refuse = Vec::new();
                                handle_command(
                                    &mut swarm,
                                    &mut manager,
                                    &open,
                                    &mut refuse,
                                    &mut listens,
                                    &mut pending_direct,
                                    &mut direct_state,
                                    &mut in_flight,
                                    config.max_pending_listens,
                                    config.max_payload_bytes,
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
                    // WAKE AT THE DEADLINE even if nothing else arrives.
                    // Without this the grace would only end when some
                    // other branch happened to fire, which for a peer
                    // that has gone silent is never.
                    () = async {
                        match grace_deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending::<()>().await,
                        }
                    } => {}
                    event = swarm.select_next_some(), if room => {
                        // SETTLE THE ADMISSION FIRST. Every dial holds a
                        // pending slot until its outcome arrives, so an
                        // outcome that did not release one is a slot
                        // leaked for the life of the process -- the
                        // ceiling decaying by one per dial until nothing
                        // can connect. Done here rather than inside
                        // `translate`, which is a pure shape conversion
                        // and must not also own resource accounting.
                        // DIRECT V2 FIRST, and it CONSUMES the event.
                        // An inbound request carries a `ResponseChannel`
                        // that cannot be borrowed out of a shared
                        // reference, so this cannot live in
                        // `settle_outcome` — which takes the event by
                        // reference precisely so it can run before the
                        // shape conversion.
                        // COMPUTED BEFORE THE MUTABLE BORROW, and from
                        // the BASE capacity: the pending-exchange slack
                        // belongs to progress alone.
                        let may_buffer =
                            may_buffer_delivery(outbox.len(), config.event_capacity);
                        let event = match handle_direct(
                            event,
                            &mut swarm,
                            &mut direct_state,
                            &mut pending_direct,
                            &mut outbox,
                            DirectTick {
                                now_ms: now_ms(started),
                                max_payload_bytes: config.max_payload_bytes,
                                wall_ms: wall_ms(),
                                draining: manager.is_draining(),
                                may_buffer_delivery: may_buffer,
                            },
                        ) {
                            DirectHandled::Consumed => continue,
                            DirectHandled::Passed(event) => *event,
                        };

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
                        // NOTIFICATIONS SEE THE BASE CAPACITY, like
                        // deliveries. `polling_room` adds slack so the
                        // callers waiting on in-flight work can be
                        // settled; an `Identify::Received` buffered into
                        // that slack takes the slot a direct response or
                        // its timeout needed, `room` goes false on the
                        // next iteration, and `send_direct` waits on an
                        // answer nothing will ever poll.
                        //
                        // Dropped rather than queued when there is no
                        // base room: these are informational, nothing
                        // downstream blocks on them, and the alternative
                        // is an outbox that grows with whatever the
                        // network sends.
                        if let Some(event) = translated
                            && may_buffer_delivery(outbox.len(), config.event_capacity)
                        {
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

    /// Send one directed message to an already-connected peer.
    ///
    /// Returns the endpoint the remote resolved it to, which for an
    /// omitted destination is how the caller learns the remote's
    /// default. A `Rejected` answer becomes the local error its coarse
    /// reason maps to — remote `no_route` is
    /// [`RemoteEndpointUnavailable`](DirectError::RemoteEndpointUnavailable),
    /// because that is all the peer disclosed.
    ///
    /// The peer must already be connected; see
    /// [`GatedSwarm::send_direct`](crate::gated_swarm::GatedSwarm::send_direct)
    /// for why an implicit dial is refused rather than attempted.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone; otherwise the
    /// exchange's own outcome.
    pub async fn send_direct(
        &self,
        peer: TransportIdentity,
        frame: DirectMessageV2,
    ) -> Result<Result<EndpointId, DirectError>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::SendDirect {
                peer,
                frame: Box::new(frame),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Install endpoint configuration for directed messaging.
    ///
    /// Replaces whatever was there and discards every open queue: this
    /// is the leases changing hands, and a new holder must not inherit
    /// the previous one's undelivered messages.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone.
    pub async fn configure_direct(&self, config: DirectEndpoints) -> Result<(), SubstrateError> {
        // NO VALIDATION HERE. `DirectEndpoints` can only be built by
        // `from_profile`, which runs the canonical `ProfileConfig`
        // validator — duplicate ids, a default naming an absent or
        // disabled endpoint, the endpoint-count ceiling and the queue
        // depth are all decided there. A second copy of those rules on
        // this path is how the two drift.
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::ConfigureDirect {
                config: Box::new(config),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)?
    }

    /// End one endpoint's lease and close its queue with it.
    ///
    /// Returns how many undelivered events were discarded. An offline
    /// endpoint holds no daemon-side backlog, so they are dropped rather
    /// than kept for whoever leases it next.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone.
    pub async fn revoke_endpoint(&self, endpoint: EndpointId) -> Result<usize, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::RevokeEndpoint { endpoint, reply })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Take everything waiting on one endpoint's queue, oldest first.
    ///
    /// The in-process stand-in for what Stage 8's IPC session does.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone.
    pub async fn drain_endpoint(
        &self,
        endpoint: EndpointId,
    ) -> Result<Vec<interweave_transport_runtime::DirectEvent>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::DrainEndpoint { endpoint, reply })
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

#[allow(clippy::too_many_arguments)]
fn handle_command(
    swarm: &mut GatedSwarm,
    manager: &mut ConnectionManager,
    open: &HashMap<libp2p::swarm::ConnectionId, OpenConnection>,
    refuse: &mut Vec<libp2p::swarm::ConnectionId>,
    listens: &mut PendingListens,
    pending_direct: &mut HashMap<libp2p::request_response::OutboundRequestId, PendingDirect>,
    direct_state: &mut DirectState,
    in_flight: &mut HashMap<libp2p::swarm::ConnectionId, DialTicket>,
    max_pending_listens: usize,
    effective_payload: usize,
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
            // DIRECT ADMISSION MOVES WITH IT. Revocation must take away
            // the connectivity a peer already has (ADR-0012), and a
            // directed message travels over a connection — so a trust
            // change that closed connections while leaving direct
            // admission on the old policy would refuse the peer's
            // connection and accept its message.
            direct_state.adopt_trust(&trust);
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
        SwarmCommand::SendDirect { peer, frame, reply } => {
            // THE SOURCE ENDPOINT MUST BE ONE THIS NODE HOLDS A LEASE
            // FOR. It used to be taken from the frame and sent as given,
            // which made it arbitrary caller input: a handle holder could
            // name any endpoint at all, and the receiver would key its
            // dedup entry on that label and surface it on the delivered
            // event. CLAUDE.md §5 is the other way round — source
            // EndpointId is derived from the local lease, never trusted
            // from the caller.
            //
            // The registry is the whole check. `configure_direct` claims
            // a lease per configured endpoint, so holding one means this
            // runtime really serves that endpoint; a name it never
            // configured, or one whose lease was revoked, has none.
            //
            // Stage 8 tightens this rather than replacing it: an IPC
            // session gets ONE exclusive lease and may name only that
            // one, where this layer accepts any lease the node holds.
            // Which lease belongs to which caller is a question this
            // stage has no sessions to ask.
            if direct_state
                .registry
                .lease(&frame.source_endpoint)
                .is_none()
            {
                let _ = reply.send(Err(DirectError::EndpointNotRegistered));
                return;
            }
            // TRUST IS RE-READ HERE, not inherited from the connection.
            // `set_trust` revokes a peer and closes its connections, but
            // the close is asynchronous: until the event arrives the
            // connection is still open and `is_connected` still says yes.
            // A send queued in that window would cross a connection that
            // has already lost data-plane authorization, which is the one
            // thing the revocation was for.
            //
            // Infrastructure-only trust is not data-plane trust either
            // (ADR-0036), so the test names the class it needs rather
            // than testing for "not Unauthorized".
            // DRAINING STOPS OUTBOUND WORK TOO. Inbound already refuses
            // after `drain()`; starting a NEW local exchange in the same
            // window contradicts the same contract from the other side —
            // the node has said it is going out of service and then
            // dispatched fresh work whose answer it may not be around to
            // receive. `ShuttingDown` is the local error, not the remote
            // one: nothing crossed a network boundary.
            // THE LIMIT BINDS BOTH DIRECTIONS. A profile that refuses a
            // payload on the way in has not configured anything if it
            // sends the same payload out — and the remote's own limit is
            // its business, so this is about honouring the local
            // configuration rather than predicting the peer's.
            if frame.payload.bytes().len() > effective_payload {
                let _ = reply.send(Err(DirectError::PayloadTooLarge));
                return;
            }
            if manager.is_draining() {
                let _ = reply.send(Err(DirectError::ShuttingDown));
                return;
            }
            // SENDING TO SELF IS A CALLER ERROR, not a network one.
            // `DIRECT.md` says the local profile PeerId is
            // `InvalidArgument` and that self-dial never occurs; left to
            // the swarm, libp2p cannot hold a self-connection and the
            // caller would be told `PeerUnreachable` — a network verdict
            // on a local mistake, about a peer that is right here.
            //
            // AFTER THE LEASE GATE, which is what `ENDPOINTS.md`
            // requires: step 1 is "caller must own an active endpoint
            // lease or receive `EndpointNotRegistered`". This used to
            // sit in the public method ahead of everything, so a frame
            // naming an unleased source AND the local peer reported
            // `InvalidArgument` — sending a caller to fix the
            // destination while the missing prerequisite was the lease.
            //
            // BEFORE the trust check, though the contract numbers self
            // as step 5 and profile trust as step 4. Taken literally,
            // step 5 is unreachable: `classify` answers `Unauthorized`
            // for this node's own PeerId — correct for a dial, since
            // this is not a peer to connect to — and step 4 would
            // swallow every self-send as `UnauthorizedPeer`. The
            // contract's OUTCOME is that a self-send is
            // `InvalidArgument`, and this ordering is what delivers it.
            if manager.is_local_peer(&peer) {
                let _ = reply.send(Err(DirectError::InvalidArgument));
                return;
            }
            if manager.classify(&peer) != ConnectionClass::DataPlaneTrusted {
                let _ = reply.send(Err(DirectError::UnauthorizedPeer));
                return;
            }
            // THE SOURCE ENDPOINT'S OWN POLICY NARROWS THIS SEND.
            // `authorize_outbound` applies profile trust first and the
            // endpoint's outbound subset second, and until recently it
            // had no production caller at all — the narrowing existed in
            // the domain layer and bound nothing.
            //
            // `UnauthorizedPeer`, because `ENDPOINTS.md` outbound step 3
            // says so outright: "a destination excluded by that
            // narrowing policy returns `UnauthorizedPeer` locally". This
            // returned `CapabilityDenied` on the reasoning that the
            // ENDPOINT lacked authority rather than the profile — which
            // reads well and is not what the contract specifies, and
            // `CapabilityDenied` tells a caller its local session is
            // missing a capability, sending it after the wrong fix.
            //
            // That also settles the ordering against the profile check.
            // Both answers are now the same, so which runs first is
            // unobservable — where it was not, when the two codes
            // differed and a revoked peer was told the wrong one.
            if !matches!(
                direct_state.registry.authorize_outbound(
                    &frame.source_endpoint,
                    &peer,
                    &direct_state.trust
                ),
                interweave_trust_api::TrustDecision::Allowed
            ) {
                let _ = reply.send(Err(DirectError::UnauthorizedPeer));
                return;
            }
            let peer_id = match to_peer_id(&peer) {
                Ok(id) => id,
                Err(()) => {
                    let _ = reply.send(Err(DirectError::InvalidArgument));
                    return;
                }
            };
            // BOUNDED BEFORE THE EXCHANGE STARTS. Every send inserts an
            // entry that lives until a response or the request timeout,
            // and the command channel does not bound it — its capacity is
            // released as the task drains commands, so a caller can queue
            // far more exchanges than either limit allows. Without this,
            // `pending_direct` and libp2p's own outbound queue grow
            // together with nothing to stop them.
            //
            // Counted by scanning rather than kept in a second index:
            // the map is bounded at 128 by the line below, so the scan is
            // bounded too, and one source of truth cannot disagree with
            // itself.
            if let Err(refused) = admit_outbound(pending_direct.values().map(|p| &p.peer), &peer) {
                let _ = reply.send(Err(refused));
                return;
            }

            // CAPTURED BEFORE THE FRAME IS MOVED. The answer is checked
            // against what was asked, and after `send_direct` takes the
            // frame there is nothing left to compare with.
            let message_id = frame.message_id;
            let requested = frame.destination_endpoint.clone();
            match swarm.send_direct(&peer_id, *frame) {
                Ok(request_id) => {
                    pending_direct.insert(
                        request_id,
                        PendingDirect {
                            message_id,
                            requested,
                            reply,
                            peer,
                        },
                    );
                }
                // NOT CONNECTED, and `DIRECT.md` separates two cases
                // that this used to collapse. Its comment claimed the
                // layer "knows only that there is no connection" — but
                // the manager is right here and knows whether any
                // address was ever recorded:
                //
                //   no usable candidate addresses -> `PeerUnknown`,
                //     without ad hoc discovery;
                //   usable candidates -> could not reach it now.
                //
                // The distinction is what an operator acts on. Nothing
                // to dial is a configuration or discovery problem;
                // something to dial that did not answer is a network
                // one, and telling them apart is the difference between
                // adding an address and chasing a firewall.
                //
                // The contract also allows the ConnectionManager to dial
                // under the command deadline — "may", not must — and
                // this does not. Sequencing a gated dial and then the
                // exchange means holding the caller's reply across a
                // connection outcome, which is a deferred-reply state
                // machine the command loop has nowhere to put yet. The
                // retry scheduler already re-dials known peers, so a
                // send after an idle disconnect recovers on the next
                // tick rather than staying broken.
                Err(NotConnected) => {
                    let known = manager.known_addresses(&peer);
                    let _ = reply.send(Err(if known == 0 {
                        DirectError::PeerUnknown
                    } else {
                        DirectError::PeerUnreachable
                    }));
                }
            }
        }
        SwarmCommand::ConfigureDirect { config, reply } => {
            let _ = reply.send(direct_state.configure(*config));
        }
        SwarmCommand::RevokeEndpoint { endpoint, reply } => {
            let _ = reply.send(direct_state.revoke(&endpoint));
        }
        SwarmCommand::DrainEndpoint { endpoint, reply } => {
            let _ = reply.send(direct_state.drain(&endpoint));
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

/// Everything inbound direct admission owns, held by the Swarm task.
///
/// Constructed empty: a profile with no endpoint leases admits nothing,
/// which is the correct posture for a daemon that has just started and
/// has no local client attached yet (`testing.md` scenario 27).
pub struct DirectState {
    /// Who this profile trusts, mirrored from the manager's sources.
    trust: interweave_transport_runtime::PeerTrustPolicy,
    /// Direct-ingress token buckets.
    ingress: interweave_transport_runtime::ingress::IngressLimiter,
    /// The duplicate cache.
    dedup: interweave_transport_runtime::dedup::DedupCache,
    /// In-flight reservations.
    reservations: interweave_transport_runtime::dedup::ReservationMap,
    /// Configured endpoints and their leases.
    registry: EndpointRegistry,
    /// Open delivery queues.
    queues: EndpointQueues,
    /// Inbound requests whose answer is queued but not yet written.
    ///
    /// `pending_direct` counts OUTBOUND exchanges only, so a shutdown
    /// that consulted it alone would break the loop while an admitted
    /// request's answer was still queued — dropping the accepted queue
    /// and the unsent response, and leaving the sender to retry into a
    /// restarted node. Tracked here so the grace covers both directions.
    ///
    /// A SET OF IDS, not a count, and the type is the enforcement.
    /// `InboundFailure` is emitted for requests that never reached
    /// `handle_direct` at all — a frame the codec refused before
    /// delivery — and those registered no answer. A bare counter
    /// decremented on every completion event consumed some OTHER
    /// exchange's entry, and a shutdown racing that saw zero and dropped
    /// an answer it had promised.
    ///
    /// There is no unit test for this because there is no arithmetic
    /// left to get wrong: removing an id that was never inserted is a
    /// no-op, so the miscount is unrepresentable rather than merely
    /// avoided. `InboundRequestId` has no public constructor either, so
    /// a test could only have exercised `BTreeSet` itself.
    answering: std::collections::BTreeSet<libp2p::request_response::InboundRequestId>,
}

impl DirectState {
    /// Adopt the profile's data-plane trust.
    ///
    /// THE SAME `PeerTrustPolicy` THE MANAGER USES, taken from the one
    /// `TrustSources` a caller supplied rather than kept separately.
    /// Two copies of "who may talk to us" is two answers, and the one a
    /// directed message met would eventually differ from the one that
    /// admitted its connection.
    fn adopt_trust(&mut self, trust: &TrustSources) {
        self.trust = trust.peers.clone();
    }

    /// Install endpoint configuration and open a queue for each lease.
    fn configure(&mut self, config: DirectEndpoints) -> Result<(), SubstrateError> {
        use interweave_transport_runtime::endpoint_registry::LocalSessionId;

        let mut endpoints = std::collections::BTreeMap::new();
        for (name, configured) in &config.endpoints {
            endpoints.insert(name.clone(), configured.clone());
        }
        let mut registry = EndpointRegistry::new(endpoints, config.default.clone());

        // A LEASE PER ENDPOINT, in-process. Stage 6 routes to an
        // in-process `LocalDataSession`; Stage 8 replaces this with a
        // real IPC claim, and the registry cannot tell the difference —
        // which is the point of it holding leases rather than sessions.
        let mut queues = EndpointQueues::new();
        for (name, configured) in &config.endpoints {
            // A DISABLED ENDPOINT IS NOT A FAILURE. It is configured and
            // deliberately closed, so it holds no lease and opens no
            // queue, and inbound routing answers `no_route` for it
            // through `resolve_inbound` rather than through an absence
            // here.
            if !configured.enabled {
                continue;
            }
            // THE SYNTHETIC KIND MUST BE ONE THE ENDPOINT PERMITS.
            // `allowed_client_kinds` used to arrive empty, because the
            // configuration was rebuilt from `RegisteredEndpoint::
            // default()`; now that the real profile reaches here, a
            // hard-coded `in-process` is refused outright by any
            // endpoint restricted to `human-client` or `claude-channel`
            // — which the example profiles are.
            //
            // Stage 8 replaces this with the claim a real session makes,
            // and this stand-in exists only until there is one.
            let kind = configured
                .allowed_client_kinds
                .first()
                .map_or("in-process", String::as_str);
            // AND THE RESULT IS NOT DISCARDED. It used to be `.is_ok()`,
            // so an endpoint that failed to claim got no lease and no
            // queue while `configure_direct` still reported success —
            // every send from it then `EndpointNotRegistered` and every
            // message to it `no_route`, for a configuration the caller
            // was told had installed.
            //
            // NO TEST REACHES THIS ARM, and that is stated rather than
            // implied: with a permitted kind chosen above, a disabled
            // endpoint skipped, names unique per session and duplicates
            // refused by `ProfileConfig::validate`, none of the four
            // `ClaimFailure` variants is currently reachable here. The
            // propagation is what makes a FUTURE change fail loudly
            // instead of producing a silently dead endpoint, which is
            // what the discarded result did.
            registry
                .claim(
                    name,
                    LocalSessionId(format!("in-process-{}", name.as_str())),
                    kind,
                    config.epoch.clone(),
                )
                .map_err(|failure| {
                    SubstrateError::InvalidProfile(vec![format!(
                        "endpoint {} could not be leased: {failure:?}",
                        name.as_str()
                    )])
                })?;
            queues.open(name.clone(), config.queue_bound);
        }
        self.registry = registry;
        self.queues = queues;
        Ok(())
    }

    /// Whether an admitted request's answer is still queued.
    ///
    /// Read by shutdown, which must not break the loop while one is: the
    /// response would never be written and the sender would retry into a
    /// node that had already accepted it.
    ///
    /// Read by `polling_room` as well as by shutdown. An earlier version
    /// of this comment argued the opposite — that the count is too
    /// loosely bounded to buy polling slack — and that was a tidier
    /// bound bought with a liveness failure: with the outbox full and no
    /// other work in flight, polling stopped, the queued answer was
    /// never written, and the remote timed out and retried until an
    /// unrelated local consumer happened to drain.
    fn answering(&self) -> usize {
        self.answering.len()
    }

    /// Take everything waiting for `endpoint`.
    fn drain(&mut self, endpoint: &EndpointId) -> Vec<interweave_transport_runtime::DirectEvent> {
        self.queues.drain(endpoint)
    }

    /// End `endpoint`'s lease and close its queue together.
    ///
    /// ONE OPERATION, because they are one fact. The registry decides
    /// whether an endpoint is leased and the queue holds what arrived for
    /// it, and a revoke that touched only the first would leave a queue
    /// open for an endpoint nothing holds — a daemon-side backlog for an
    /// offline endpoint, which `testing.md` and ADR-0044 both forbid.
    /// Leaving them as two calls means every future caller has to
    /// remember the second one.
    ///
    /// Returns the number of undelivered events discarded, so a caller
    /// can log a real number rather than assume zero.
    fn revoke(&mut self, endpoint: &EndpointId) -> usize {
        self.registry.revoke(endpoint);
        self.queues.close(endpoint)
    }

    /// Build the admission state for a profile that has no leases yet.
    #[must_use]
    pub fn new(now_ms: u64) -> Self {
        use interweave_transport_runtime::dedup::{
            DEFAULT_MAX_ENTRIES, DEFAULT_MAX_RESERVATIONS, DEFAULT_MAX_RESERVATIONS_PER_PEER,
            DEFAULT_TTL_MS, DedupCache, ReservationMap,
        };
        Self {
            trust: interweave_transport_runtime::PeerTrustPolicy::default(),
            ingress: interweave_transport_runtime::ingress::IngressLimiter::with_defaults(now_ms),
            answering: std::collections::BTreeSet::new(),
            dedup: DedupCache::new(DEFAULT_MAX_ENTRIES, DEFAULT_TTL_MS),
            reservations: ReservationMap::new(
                DEFAULT_MAX_RESERVATIONS,
                DEFAULT_MAX_RESERVATIONS_PER_PEER,
            ),
            registry: EndpointRegistry::new(std::collections::BTreeMap::new(), None),
            queues: EndpointQueues::new(),
        }
    }
}

/// Endpoint configuration for this profile's direct routing.
///
/// Stage 6 shape: every configured endpoint is leased in-process. Stage 8
/// The direct runtime's endpoint state, DERIVED from a validated
/// profile configuration.
///
/// # Why this is not its own configuration model
///
/// It used to be one: a list of bare ids, a default, a depth and an
/// epoch, assembled by hand at the call site. Every field the canonical
/// `ProfileConfig` had and this did not was a field silently replaced by
/// `RegisteredEndpoint::default()` — and the two that mattered were the
/// inbound and outbound trust policies, whose default INHERITS profile
/// trust. An endpoint configured to exclude a peer accepted that peer.
///
/// The answer is not more checks here. Each one would restate a rule
/// `ProfileConfig::validate` already enforces, and the next field added
/// there would be silently dropped here exactly as these were. So there
/// is one constructor, it takes the canonical configuration, and it
/// refuses anything that configuration would refuse — duplicate ids, a
/// default naming an absent or disabled endpoint, and too many
/// endpoints all come from the validator rather than from code written
/// again here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectEndpoints {
    /// Every endpoint, with the configuration it was actually given.
    endpoints: Vec<(EndpointId, RegisteredEndpoint)>,
    /// The endpoint an omitted destination resolves to.
    ///
    /// `None` means a message with no destination is `no_route` — which
    /// is a configuration, not a failure: a profile may require every
    /// sender to name where it is going.
    default: Option<EndpointId>,
    /// Bound on each endpoint's delivery queue.
    queue_bound: usize,
    /// The lease epoch to grant.
    epoch: interweave_transport_runtime::Generation,
}

impl DirectEndpoints {
    /// Derive runtime endpoint state from a profile configuration.
    ///
    /// # Errors
    /// [`SubstrateError::InvalidProfile`] carrying every rule the
    /// configuration broke — reported together rather than one at a
    /// time, because an operator fixing sixty endpoints should not
    /// discover their mistakes one run apart.
    ///
    /// Also [`SubstrateError::InvalidConfig`] when `queue_bound` is
    /// outside `1..=MAX_EVENT_QUEUE`, which is a property of this
    /// runtime rather than of the profile.
    pub fn from_profile(
        profile: &interweave_profile_config::ProfileConfig,
        queue_bound: usize,
        epoch: interweave_transport_runtime::Generation,
    ) -> Result<Self, SubstrateError> {
        let errors = profile.validate();
        if !errors.is_empty() {
            return Err(SubstrateError::InvalidProfile(
                errors.iter().map(ToString::to_string).collect(),
            ));
        }
        if queue_bound == 0 || queue_bound > interweave_local_client_api::MAX_EVENT_QUEUE {
            return Err(SubstrateError::InvalidConfig {
                field: "direct.queue_bound",
                got: queue_bound,
                allowed: (1, interweave_local_client_api::MAX_EVENT_QUEUE),
            });
        }
        let endpoints = profile
            .endpoints
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.id.clone(),
                    RegisteredEndpoint {
                        enabled: entry.enabled,
                        allowed_client_kinds: entry
                            .allowed_client_kinds
                            .iter()
                            .map(|k| k.as_str().to_owned())
                            .collect(),
                        inbound: entry.inbound.clone(),
                        outbound: entry.outbound.clone(),
                    },
                )
            })
            .collect();
        Ok(Self {
            endpoints,
            default: profile.endpoints.default_direct_endpoint.clone(),
            queue_bound,
            epoch,
        })
    }
}

/// The facts one loop iteration hands to direct handling.
///
/// A struct rather than three more parameters: they are all properties
/// of THIS iteration rather than of the connection or the state, and
/// grouping them keeps the reason they travel together visible.
#[derive(Debug, Clone, Copy)]
struct DirectTick {
    /// Monotonic milliseconds since the runtime started.
    now_ms: u64,
    /// The profile's effective payload limit, for the deferred parse.
    max_payload_bytes: usize,
    /// Unix-epoch milliseconds, for the receipt time on a delivery.
    ///
    /// Separate from `now_ms`, which is monotonic-since-startup and
    /// governs rate limits and deadlines. A receipt time taken from that
    /// clock restarts near zero with the process.
    wall_ms: u64,
    /// Whether the node has begun draining.
    draining: bool,
    /// Whether an accepted delivery may be buffered for the consumer.
    ///
    /// Decided by [`may_buffer_delivery`] from the BASE capacity, so a
    /// delivery cannot spend the slack reserved for in-flight work.
    may_buffer_delivery: bool,
}

/// Whether a swarm event was a direct-protocol one.
enum DirectHandled {
    /// It was, and has been fully handled.
    Consumed,
    /// It was not; here it is back.
    ///
    /// Boxed because a `SwarmEvent` is far larger than the unit variant,
    /// and every non-direct event — which is most of them — would
    /// otherwise pay for the difference.
    Passed(Box<Libp2pSwarmEvent<SubstrateBehaviourEvent>>),
}

/// Handle one direct-protocol event, or hand it back.
fn handle_direct(
    event: Libp2pSwarmEvent<SubstrateBehaviourEvent>,
    swarm: &mut GatedSwarm,
    state: &mut DirectState,
    pending: &mut HashMap<libp2p::request_response::OutboundRequestId, PendingDirect>,
    outbox: &mut std::collections::VecDeque<SwarmEvent>,
    tick: DirectTick,
) -> DirectHandled {
    let DirectTick {
        now_ms,
        max_payload_bytes,
        wall_ms,
        draining,
        may_buffer_delivery,
    } = tick;
    use crate::direct_codec::DirectResponse;
    use libp2p::request_response::{Event as RrEvent, Message as RrMessage};

    let Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Direct(direct)) = event else {
        return DirectHandled::Passed(Box::new(event));
    };

    match direct {
        RrEvent::Message {
            peer,
            message:
                RrMessage::Request {
                    request,
                    channel,
                    request_id,
                    ..
                },
            ..
        } => {
            let Ok(source) = to_transport_identity(&peer) else {
                // A PeerId the neutral grammar rejects cannot be
                // classified or accounted for. Nothing is answered: a
                // response would require naming the peer we cannot name.
                return DirectHandled::Consumed;
            };

            // ADMISSION RUNS BEFORE THE FRAME IS PARSED. The codec
            // keeps only the bytes it read, because decoding a maximum
            // frame allocates a second payload buffer and doing that
            // first let an infrastructure-only connection — excluded
            // from direct v2 outright — or a peer already over its rate
            // choose how much work this node did for it.
            //
            // Sixteen bytes for the id is the only exception, and it has
            // to be: a response echoes the id it answers, so without one
            // nothing can be refused on the wire at all.
            let crate::direct_codec::InboundRequest::Inbound { bytes, oversize } = request else {
                // `Outbound` is what this node SENDS; the codec never
                // reads one.
                return DirectHandled::Consumed;
            };

            let gated = {
                let mut ctx = AdmissionContext {
                    trust: &state.trust,
                    ingress: &mut state.ingress,
                    dedup: &mut state.dedup,
                    reservations: &mut state.reservations,
                    registry: &state.registry,
                    queues: &mut state.queues,
                    draining,
                };
                admit_prefix(&source, now_ms, &mut ctx)
            };

            // A GATE'S REFUSAL OUTRANKS THE BYTES. An untrusted or
            // rate-limited peer learns that, not that its frame was
            // malformed — and learns it without this node parsing the
            // frame to find out.
            if let Err(refusal) = gated {
                if let Some(message_id) = crate::direct_codec::recover_id(&bytes) {
                    let _answered = swarm
                        .answer_direct(
                            channel,
                            DirectResponse::Rejected {
                                message_id,
                                reason: refusal.to_wire(),
                            },
                        )
                        .is_ok();
                    if _answered {
                        state.answering.insert(request_id);
                    }
                }
                return DirectHandled::Consumed;
            }

            let request =
                match crate::direct_codec::parse_inbound(&bytes, oversize, max_payload_bytes) {
                    Ok(frame) => frame,
                    Err((message_id, reason)) => {
                        // SPIKE-002 finding 2: a produced response is not
                        // evidence the peer heard it. Nothing is retried —
                        // a peer that sent an unparsable frame and vanished
                        // is owed nothing further.
                        if let Some(message_id) = message_id
                            && swarm
                                .answer_direct(
                                    channel,
                                    DirectResponse::Rejected { message_id, reason },
                                )
                                .is_ok()
                        {
                            state.answering.insert(request_id);
                        }
                        return DirectHandled::Consumed;
                    }
                };

            let outcome = {
                let mut ctx = AdmissionContext {
                    trust: &state.trust,
                    ingress: &mut state.ingress,
                    dedup: &mut state.dedup,
                    reservations: &mut state.reservations,
                    registry: &state.registry,
                    queues: &mut state.queues,
                    draining,
                };
                admit_structured(
                    &request,
                    &source,
                    interweave_transport_runtime::Clocks {
                        monotonic_ms: now_ms,
                        wall_ms,
                    },
                    &mut ctx,
                )
            };

            let response = match &outcome {
                AdmissionOutcome::Accepted { resolved_endpoint }
                | AdmissionOutcome::DuplicateAccepted { resolved_endpoint } => {
                    DirectResponse::Accepted {
                        message_id: request.message_id,
                        resolved_endpoint: resolved_endpoint.clone(),
                    }
                }
                // A WAITER IS ANSWERED, not dropped. The previous version
                // returned here and let the `ResponseChannel` fall out of
                // scope, which sends nothing at all — the remote then
                // waits out its own deadline for a message this profile
                // had already decided about.
                //
                // It read as harmless because it is currently
                // UNREACHABLE: `admit_inbound` acquires and releases the
                // reservation inside one synchronous call, so nothing is
                // ever in flight when the next request arrives. That is a
                // property of today's admission, not of the protocol, and
                // a silent drop waiting for admission to become async is
                // the kind of thing that surfaces as an unexplained
                // timeout much later.
                //
                // The owner has therefore already settled, and its result
                // is in the dedup cache — which is exactly what the
                // waiter should receive. A cache miss means the owner is
                // genuinely still in flight (a future async admission) or
                // was refused, and `overloaded` is the honest answer to
                // "I cannot hold this open for you".
                AdmissionOutcome::AttachedAsWaiter => {
                    waiter_response(&state.dedup, &request, &source)
                }
                AdmissionOutcome::Refused(refusal) => DirectResponse::Rejected {
                    message_id: request.message_id,
                    reason: refusal.to_wire(),
                },
            };

            // SPIKE-002 FINDING 2: producing a response is not evidence
            // the peer heard it. `send_response` fails when the
            // connection that carried the request is gone, and that is
            // reported rather than discarded — but it is NOT a reason to
            // undo the delivery. The event is on a local queue; the
            // remote simply will not learn that it arrived, and will
            // retry, which dedup answers.
            let answered = swarm.answer_direct(channel, response).is_ok();
            if answered {
                state.answering.insert(request_id);
            }

            if let AdmissionOutcome::Accepted { resolved_endpoint } = outcome {
                let _ = answered;
                // PROGRESS CAPACITY IS RESERVED FROM DELIVERIES, not
                // shared with them. The Swarm is polled while the outbox
                // has room for the exchanges still in flight; buffering
                // deliveries into that same allowance lets a peer refill
                // it and stop the polling that settles those exchanges —
                // the freeze this slack was added to prevent, reached
                // one layer down.
                //
                // DROPPING THE NOTIFICATION IS NOT DROPPING THE MESSAGE.
                // The event is already in the endpoint's bounded queue —
                // that admission is what `AcceptedV2` promised (ADR-0018)
                // — and `drain_endpoint` still returns it. What is lost
                // under sustained backpressure is a wake-up, from a
                // consumer that by construction is not reading.
                if may_buffer_delivery {
                    outbox.push_back(SwarmEvent::DirectDelivered {
                        endpoint: resolved_endpoint,
                        peer: source,
                    });
                }
            }
            DirectHandled::Consumed
        }

        RrEvent::Message {
            message:
                RrMessage::Response {
                    request_id,
                    response,
                },
            ..
        } => {
            let Some(PendingDirect {
                message_id,
                requested,
                reply,
                ..
            }) = pending.remove(&request_id)
            else {
                return DirectHandled::Consumed;
            };
            let _ = reply.send(validate_response(&response, message_id, requested.as_ref()));
            DirectHandled::Consumed
        }

        RrEvent::OutboundFailure {
            request_id, error, ..
        } => {
            if let Some(PendingDirect { reply, .. }) = pending.remove(&request_id) {
                let _ = reply.send(Err(outbound_error(&error)));
            }
            DirectHandled::Consumed
        }

        // An inbound failure means a request we were answering is gone.
        // Nothing to undo: admission already decided, and the remote will
        // retry into dedup.
        RrEvent::InboundFailure { request_id, .. } | RrEvent::ResponseSent { request_id, .. } => {
            // EITHER WAY THAT ANSWER IS NO LONGER PENDING: it was
            // written, or the stream carrying it died. Neither is a
            // reason to undo the delivery — admission already decided,
            // and the remote retries into dedup.
            //
            // BY ID, so a failure for a request this side never answered
            // — one the codec refused before delivery — releases nothing
            // that belongs to another exchange.
            state.answering.remove(&request_id);
            DirectHandled::Consumed
        }
    }
}

/// What a request that attached as a waiter is told.
///
/// See the call site for why this is a cache lookup rather than a held
/// channel: the owner settles synchronously, so by the time a waiter
/// could exist its answer is already recorded.
fn waiter_response(
    dedup: &interweave_transport_runtime::dedup::DedupCache,
    request: &DirectMessageV2,
    source: &TransportIdentity,
) -> crate::direct_codec::DirectResponse {
    use crate::direct_codec::DirectResponse;
    let key = interweave_transport_runtime::direct_inbound::dedup_key(request, source);
    match dedup.get(&key) {
        Some(record) => DirectResponse::Accepted {
            message_id: request.message_id,
            resolved_endpoint: record.resolved_endpoint.clone(),
        },
        None => DirectResponse::Rejected {
            message_id: request.message_id,
            reason: DirectRejectReason::Overloaded,
        },
    }
}

/// Validate a response before a caller may believe it.
///
/// `DIRECT.md`: a sender validates every remote field before caching or
/// surfacing it. A response that does not satisfy this is a local
/// `ProtocolViolation` and creates no positive result.
fn validate_response(
    response: &crate::direct_codec::DirectResponse,
    asked: interweave_transport_api::MessageId,
    requested: Option<&EndpointId>,
) -> Result<EndpointId, DirectError> {
    use crate::direct_codec::DirectResponse;

    // THE ID MUST BE THE ONE WE SENT, on either shape. A response
    // carrying somebody else's id is not an answer to this exchange, and
    // believing it would let a peer settle a request it was never given
    // — including, for an acceptance, caching a route for a message this
    // caller did not send.
    let echoed = match response {
        DirectResponse::Accepted { message_id, .. }
        | DirectResponse::Rejected { message_id, .. } => *message_id,
    };
    if echoed != asked {
        return Err(DirectError::ProtocolViolation);
    }

    match response {
        DirectResponse::Accepted {
            resolved_endpoint, ..
        } => {
            // AN EXPLICIT DESTINATION HAS EXACTLY ONE CORRECT ANSWER.
            // Accepting a different endpoint would let a remote silently
            // reroute a directed message to another local application —
            // the one thing endpoint addressing exists to prevent — and
            // the caller would surface that endpoint as where its
            // message went.
            //
            // An OMITTED destination is the other case: there the
            // resolved endpoint IS the answer, which is how a caller
            // learns the remote's configured default, so there is
            // nothing to compare it against.
            if let Some(asked_for) = requested
                && resolved_endpoint != asked_for
            {
                return Err(DirectError::ProtocolViolation);
            }
            Ok(resolved_endpoint.clone())
        }
        DirectResponse::Rejected { reason, .. } => Err(match reason {
            // REMOTE no_route BECOMES A LOCAL DIAGNOSTIC. The peer told
            // us nothing about which of its five branches applied, and
            // this is the local name for "it would not route it".
            DirectRejectReason::NoRoute => DirectError::RemoteEndpointUnavailable,
            DirectRejectReason::UnauthorizedPeer => DirectError::UnauthorizedPeer,
            DirectRejectReason::Overloaded => DirectError::Overloaded,
            DirectRejectReason::Malformed => DirectError::ProtocolViolation,
            DirectRejectReason::TooLarge => DirectError::PayloadTooLarge,
            DirectRejectReason::ShuttingDown => DirectError::BackendUnavailable,
            // NOT `CapabilityDenied`. The vocabulary has a variant for
            // exactly this — the remote does not support the protocol —
            // and reusing the authorization one leaves a caller unable
            // to tell version incompatibility from being refused.
            DirectRejectReason::Unsupported => DirectError::ProtocolUnsupported,
        }),
    }
}

/// Map an outbound failure onto a local error.
fn outbound_error(error: &libp2p::request_response::OutboundFailure) -> DirectError {
    use libp2p::request_response::OutboundFailure;
    match error {
        // SPIKE-002 FINDING 1: timeout attribution is a RACE. When both
        // sides time out, whichever fires first decides whether the
        // requester reads `Timeout` or `Io`, so the two are not reliably
        // distinguishable and reporting them apart would surface
        // scheduler luck as a diagnosis. Both are "the exchange did not
        // complete", which is what `PeerUnreachable` says.
        // OUR OWN DECODER'S VERDICT ARRIVES AS `Io`. `read_response`
        // reports an unknown tag, a bad endpoint label, trailing bytes
        // or an over-ceiling response as `InvalidData`, and
        // request-response wraps that in the same variant a broken
        // socket produces. Reading them alike told the caller the peer
        // was unreachable when the peer had in fact answered — with
        // bytes this protocol refuses.
        //
        // The KIND is what separates them, and only that kind: every
        // other io error stays a reachability answer, so finding 1's
        // timeout/EOF race below is untouched.
        OutboundFailure::Io(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            DirectError::ProtocolViolation
        }
        OutboundFailure::Timeout | OutboundFailure::Io(_) => DirectError::PeerUnreachable,
        // FINDING 3: the major-version signal. A peer that does not speak
        // this protocol id is not unreachable — it is incompatible, and
        // an operator fixes that differently.
        // The same misclassification on the negotiation path, and the
        // same answer. SPIKE-002 finding 3 makes this the MAJOR-VERSION
        // signal, which is a protocol fact and not an authorization one.
        OutboundFailure::UnsupportedProtocols => DirectError::ProtocolUnsupported,
        OutboundFailure::DialFailure => DirectError::PeerUnreachable,
        OutboundFailure::ConnectionClosed => DirectError::PeerUnreachable,
    }
}

#[cfg(test)]
mod response_validation_tests {
    use super::validate_response;
    use crate::direct_codec::DirectResponse;
    use interweave_transport_api::{
        DirectRejectReason, EndpointId, MessageId, TransportError as DirectError,
    };

    fn endpoint(name: &str) -> EndpointId {
        EndpointId::parse(name).expect("valid endpoint id")
    }

    fn asked() -> MessageId {
        MessageId::from_bytes([1; 16])
    }

    #[test]
    fn an_explicit_destination_must_be_answered_by_that_endpoint() {
        let honest = DirectResponse::Accepted {
            message_id: asked(),
            resolved_endpoint: endpoint("claude"),
        };
        assert_eq!(
            validate_response(&honest, asked(), Some(&endpoint("claude"))),
            Ok(endpoint("claude"))
        );

        // THE REROUTE. A remote answering with a DIFFERENT endpoint than
        // the one addressed would silently deliver to another local
        // application, and the caller would surface that endpoint as
        // where its message went.
        let rerouted = DirectResponse::Accepted {
            message_id: asked(),
            resolved_endpoint: endpoint("human"),
        };
        assert_eq!(
            validate_response(&rerouted, asked(), Some(&endpoint("claude"))),
            Err(DirectError::ProtocolViolation),
            "an explicit destination has exactly one correct answer"
        );
    }

    /// An omitted destination is the other case: the resolved endpoint
    /// IS the answer, so there is nothing to compare it against.
    #[test]
    fn an_omitted_destination_accepts_whatever_the_default_resolved_to() {
        let response = DirectResponse::Accepted {
            message_id: asked(),
            resolved_endpoint: endpoint("human"),
        };
        assert_eq!(
            validate_response(&response, asked(), None),
            Ok(endpoint("human"))
        );
    }

    #[test]
    fn a_response_echoing_the_wrong_id_is_not_an_answer_to_this_exchange() {
        let other = MessageId::from_bytes([9; 16]);
        for response in [
            DirectResponse::Accepted {
                message_id: other,
                resolved_endpoint: endpoint("claude"),
            },
            DirectResponse::Rejected {
                message_id: other,
                reason: DirectRejectReason::NoRoute,
            },
        ] {
            assert_eq!(
                validate_response(&response, asked(), Some(&endpoint("claude"))),
                Err(DirectError::ProtocolViolation),
                "a mismatched id is refused on both shapes"
            );
        }
    }

    /// The id is checked before the reason is mapped, so a rejection
    /// cannot settle an exchange it does not belong to either.
    #[test]
    fn a_rejection_with_the_right_id_still_maps_its_reason() {
        let response = DirectResponse::Rejected {
            message_id: asked(),
            reason: DirectRejectReason::NoRoute,
        };
        assert_eq!(
            validate_response(&response, asked(), Some(&endpoint("claude"))),
            Err(DirectError::RemoteEndpointUnavailable)
        );
    }
}

#[cfg(test)]
mod waiter_tests {
    use super::waiter_response;
    use crate::direct_codec::DirectResponse;
    use interweave_transport_api::{
        DirectMessageV2, DirectRejectReason, EndpointId, MediaType, MessageId, Payload,
        TransportIdentity,
    };
    use interweave_transport_runtime::dedup::{DEFAULT_TTL_MS, DedupCache};
    use interweave_transport_runtime::direct_inbound::dedup_key;
    use interweave_transport_runtime::fingerprint::direct_content_fingerprint_v1;

    const P1: &str = "12D3KooWA9hFCGwGCpCbWWfLmYSpqPzXgLmPvbBrgWGNvNGSDVpS";

    fn peer() -> TransportIdentity {
        TransportIdentity::parse(P1).expect("valid peer id")
    }

    fn endpoint(name: &str) -> EndpointId {
        EndpointId::parse(name).expect("valid endpoint id")
    }

    fn request() -> DirectMessageV2 {
        DirectMessageV2 {
            message_id: MessageId::from_bytes([4; 16]),
            sent_at_ms: 1,
            source_endpoint: endpoint("human"),
            destination_endpoint: Some(endpoint("claude")),
            payload: Payload::at_ceiling(
                Some(MediaType::parse("text/plain").expect("valid media type")),
                b"hi".to_vec(),
            )
            .expect("within the ceiling"),
        }
    }

    /// THE OWNER HAS SETTLED, so the waiter receives the owner's route —
    /// the same answer, never a second enqueue.
    #[test]
    fn a_waiter_receives_the_owners_recorded_route() {
        let mut dedup = DedupCache::new(64, DEFAULT_TTL_MS);
        let req = request();
        let fingerprint = direct_content_fingerprint_v1(Some("text/plain"), b"hi").expect("hashes");
        dedup.record_accepted(dedup_key(&req, &peer()), endpoint("claude"), fingerprint, 0);

        assert_eq!(
            waiter_response(&dedup, &req, &peer()),
            DirectResponse::Accepted {
                message_id: req.message_id,
                resolved_endpoint: endpoint("claude"),
            }
        );
    }

    /// NEVER SILENCE. A cache miss means the owner is genuinely still in
    /// flight or was refused; `overloaded` is the honest answer to "I
    /// cannot hold this open for you", and it is an answer rather than a
    /// dropped channel the remote waits out its deadline on.
    #[test]
    fn a_waiter_with_no_recorded_owner_is_told_overloaded_not_nothing() {
        let dedup = DedupCache::new(64, DEFAULT_TTL_MS);
        let req = request();
        assert_eq!(
            waiter_response(&dedup, &req, &peer()),
            DirectResponse::Rejected {
                message_id: req.message_id,
                reason: DirectRejectReason::Overloaded,
            }
        );
    }

    /// The id is echoed on both shapes, so a waiter's answer settles the
    /// exchange it belongs to rather than being discarded by the sender's
    /// own id check.
    #[test]
    fn a_waiters_answer_echoes_the_request_id() {
        let dedup = DedupCache::new(64, DEFAULT_TTL_MS);
        let req = request();
        match waiter_response(&dedup, &req, &peer()) {
            DirectResponse::Accepted { message_id, .. }
            | DirectResponse::Rejected { message_id, .. } => {
                assert_eq!(message_id, req.message_id);
            }
        }
    }
}

#[cfg(test)]
mod outbound_bound_tests {
    use super::{MAX_OUTBOUND_DIRECT, MAX_OUTBOUND_DIRECT_PER_PEER, admit_outbound};
    use interweave_transport_api::{TransportError, TransportIdentity};

    const P1: &str = "12D3KooWA9hFCGwGCpCbWWfLmYSpqPzXgLmPvbBrgWGNvNGSDVpS";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";
    const P3: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn identity(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid peer id")
    }

    #[test]
    fn an_empty_table_admits() {
        assert!(admit_outbound(std::iter::empty(), &identity(P1)).is_ok());
    }

    #[test]
    fn the_per_peer_bound_refuses_that_peer_and_no_other() {
        let loud = identity(P1);
        let quiet = identity(P2);
        let held: Vec<TransportIdentity> =
            std::iter::repeat_n(loud.clone(), MAX_OUTBOUND_DIRECT_PER_PEER).collect();

        assert_eq!(
            admit_outbound(held.iter(), &loud),
            Err(TransportError::Overloaded),
            "the peer at its own bound is refused"
        );
        // THE ASYMMETRY IS THE POINT. A bound that refused everyone once
        // any peer filled up would pass a test that only asked about the
        // loud one.
        assert!(
            admit_outbound(held.iter(), &quiet).is_ok(),
            "a peer that has spent nothing keeps its own allowance"
        );
    }

    #[test]
    fn the_global_bound_refuses_a_peer_with_nothing_in_flight() {
        // Spread over two peers so neither reaches the per-peer bound
        // alone -- the refusal can then only be the global one.
        let held: Vec<TransportIdentity> = (0..MAX_OUTBOUND_DIRECT)
            .map(|i| {
                if i % 2 == 0 {
                    identity(P1)
                } else {
                    identity(P3)
                }
            })
            .collect();
        assert_eq!(held.len(), MAX_OUTBOUND_DIRECT);

        assert_eq!(
            admit_outbound(held.iter(), &identity(P2)),
            Err(TransportError::Overloaded),
            "the global ceiling binds every peer, including an idle one"
        );
    }

    #[test]
    fn one_below_each_bound_still_admits() {
        let loud = identity(P1);
        let held: Vec<TransportIdentity> =
            std::iter::repeat_n(loud.clone(), MAX_OUTBOUND_DIRECT_PER_PEER - 1).collect();
        assert!(
            admit_outbound(held.iter(), &loud).is_ok(),
            "the bound is a ceiling reached, not one approached"
        );
    }
}

#[cfg(test)]
mod backpressure_tests {
    use super::{may_buffer_delivery, polling_room};

    /// The bound still bounds: with nothing in flight, the base capacity
    /// is the whole allowance.
    #[test]
    fn a_stalled_consumer_with_nothing_in_flight_stops_polling() {
        assert!(polling_room(0, 1, 0, 0, 0));
        assert!(!polling_room(1, 1, 0, 0, 0));
    }

    /// In-flight exchanges buy room, because polling is what settles
    /// them. Without this `send_direct` waits past its own deadline for
    /// a response nothing will ever process.
    #[test]
    fn in_flight_exchanges_keep_polling_alive() {
        assert!(
            polling_room(1, 1, 0, 1, 0),
            "one exchange in flight, one event buffered: still polling"
        );
        assert!(
            !polling_room(2, 1, 0, 1, 0),
            "and the slack is exactly one, not unbounded"
        );
    }

    /// THE SECOND DEFECT. Deliveries may fill the base capacity and no
    /// further, so the slack an in-flight exchange bought stays its own.
    /// Sharing it lets a peer refill the allowance and stop the polling
    /// that would settle the exchange — the same freeze, one layer down.
    #[test]
    fn a_delivery_may_not_spend_the_slack_an_exchange_bought() {
        // One exchange in flight, base capacity one, one event already
        // buffered. Polling continues...
        assert!(polling_room(1, 1, 0, 1, 0));
        // ...and that remaining slot is NOT available to a delivery.
        assert!(
            !may_buffer_delivery(1, 1),
            "the slot belongs to progress, not to another notification"
        );
    }

    /// AN INBOUND ANSWER EARNS ROOM TOO. It is queued and unwritten,
    /// and only polling writes it — so a full outbox that stopped
    /// polling would strand it, and the remote would time out and retry
    /// until an unrelated local consumer drained. This omission is the
    /// third way this predicate has been wrong.
    #[test]
    fn a_queued_inbound_answer_keeps_polling_alive() {
        assert!(
            polling_room(1, 1, 0, 0, 1),
            "nothing else in flight, but an answer is waiting to be written"
        );
        assert!(
            !polling_room(2, 1, 0, 0, 1),
            "and that slack is exactly one, like the others"
        );
    }

    /// Below the base capacity a delivery is buffered normally — the
    /// reservation is a ceiling on deliveries, not a refusal of them.
    #[test]
    fn a_delivery_within_the_base_capacity_is_buffered() {
        assert!(may_buffer_delivery(0, 1));
        assert!(may_buffer_delivery(3, 4));
    }

    /// A LISTENER'S SLOT IS RESERVED FROM DELIVERIES TOO.
    ///
    /// This test asserted the opposite, on the reasoning that "a pending
    /// listener is waiting on a command reply rather than on a response
    /// the Swarm must carry". That is wrong: a listener waits for
    /// `NewListenAddr`, which is a Swarm event needing a slot in this
    /// same outbox. A delivery allowed into that slot leaves `listen()`
    /// waiting for an address that arrives only once some unrelated
    /// consumer drains — and the delivery's own exchange finishing frees
    /// nothing, because `answering` drops back to zero and polling stops
    /// before the queued `NewListenAddr` is ever processed.
    ///
    /// The fourth way this pair has been wrong in one stage, and the
    /// fourth to be a comment that sounded reasonable.
    #[test]
    fn a_listeners_slot_is_not_available_to_a_delivery() {
        // Outbox full at base capacity, one listener waiting. Polling
        // continues on the listener's account...
        assert!(polling_room(1, 1, 1, 0, 0));
        // ...and that slot is NOT a delivery's to take.
        assert!(
            !may_buffer_delivery(1, 1),
            "the listener's slot carries its own address event"
        );
    }
}

#[cfg(test)]
mod shutdown_grace_tests {
    use super::shutdown_settled;

    #[test]
    fn nothing_in_flight_finishes_at_once() {
        assert!(shutdown_settled(0, 0, false));
    }

    #[test]
    fn an_outbound_exchange_holds_the_grace() {
        assert!(!shutdown_settled(1, 0, false));
    }

    /// THE SECOND DIRECTION. `pending_direct` counts outbound only, so a
    /// verdict reading it alone breaks the loop while an admitted
    /// request's answer is still queued — the response never written and
    /// the sender left to retry into a restarted node.
    #[test]
    fn an_inbound_answer_holds_it_too() {
        assert!(
            !shutdown_settled(0, 1, false),
            "an answer queued but unwritten is still work in flight"
        );
    }

    /// The deadline ends it either way, which is what makes the grace
    /// BOUNDED rather than a second protocol timeout.
    #[test]
    fn the_deadline_outranks_both() {
        assert!(shutdown_settled(5, 5, true));
    }
}
