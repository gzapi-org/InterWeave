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
use interweave_transport_api::TransportError as DirectError;
use interweave_transport_api::{EndpointId, TransportIdentity};
use interweave_transport_runtime::{
    ConnectionManager, ConnectionPolicy, DialDenial, DialOrigin, TrustSources,
};
use libp2p::{PeerId, noise, tcp, yamux};
use tokio::sync::{mpsc, oneshot};

use crate::behaviour::SubstrateBehaviour;
use crate::gated_swarm::{GatedSwarm, mesh_admits};
use crate::outbound_gate::{InFlightTickets, OutboundAdmission};

mod broadcast;
mod commands;
mod config;
mod dialing;
mod direct;
mod endpoints;
mod handle;
pub mod kademlia_driver;
mod messages;

// Re-exported so `lib.rs` and every call site keep the paths they had:
// this split moved code, not the public surface.
use commands::{handle_command, translate};
use dialing::{
    ActiveListeners, Announce, OpenConnection, PendingListens, attempt_dial, now_ms,
    settle_outcome, wall_ms,
};
use direct::{DirectHandled, DirectTick, handle_direct};

pub use broadcast::{BroadcastChannels, BroadcastState};
pub use direct::{DirectEndpoints, DirectState};
pub use endpoints::DirectoryResult;

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
    pending_direct: usize,
    pending_directory: usize,
    answering_direct: usize,
    answering_directory: usize,
    past_deadline: bool,
) -> bool {
    // FOUR COUNTS, NOT TWO, and the widening is deliberate: directory
    // exchanges and their queued answers widen `polling_room`'s slack
    // exactly as direct ones do, so a verdict that omitted them broke the
    // loop while a directory query or a queued directory answer was still
    // in flight — dropping the caller, or the control response, and
    // skipping the grace that already-accepted control work is owed.
    // Naming all four makes the omission a compile error at every call
    // site rather than a divergence to be noticed later.
    past_deadline
        || (pending_direct == 0
            && pending_directory == 0
            && answering_direct == 0
            && answering_directory == 0)
}

/// Hand the consumer what is already queued, before the loop ends.
///
/// SHUTDOWN SETTLEMENTS ARE IN HERE. The Kademlia driver answers a
/// shutdown with one `QueryFailed { ShuttingDown }` per outstanding
/// query, and the provider's budget releases a permit only on a
/// completion — so a `break` that dropped the outbox discarded the very
/// events the shutdown path exists to produce, and queued them for
/// nobody. Review finding on PR #61: invoking the driver is not the
/// same as delivering what it returned.
///
/// BEST EFFORT, and the limit is stated rather than hidden: `try_send`
/// never blocks, so a consumer that has stopped reading gets what its
/// channel can still hold and no more. Awaiting room instead would let
/// a consumer that is not reading hang the shutdown it was asked to
/// perform, which is worse than an undelivered notification.
fn flush_outbox(outbox: &mut VecDeque<SwarmEvent>, tx: &mpsc::Sender<SwarmEvent>) {
    while let Some(event) = outbox.pop_front() {
        if tx.try_send(event).is_err() {
            return;
        }
    }
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
    outstanding_queries: usize,
) -> bool {
    buffered
        < event_capacity
            .saturating_add(pending_listens)
            .saturating_add(pending_exchanges)
            .saturating_add(answering_inbound)
            .saturating_add(outstanding_queries)
}

/// Whether a Kademlia query SETTLEMENT may be buffered.
///
/// A FOURTH KIND OF CALLER EARNS PROGRESS SLACK. The other three —
/// listeners awaiting an address, exchanges awaiting a response,
/// inbound answers queued — are all callers whose work only completes
/// when a Swarm event reaches them. An outstanding Kademlia query is
/// the same shape: the provider bound a budget permit before issuing
/// it, and only a completion releases that permit.
///
/// So a settlement is not a notification and must not be judged as one.
/// Gating it on base capacity alone dropped it whenever the outbox was
/// MOMENTARILY full — including when the consumer is perfectly active
/// and merely lost a `select!` race — and the permit was then gone for
/// the life of the process. Pushing it unconditionally was the other
/// error: the command branch is not gated by [`polling_room`], so a
/// caller could drain the bounded command channel into an unbounded
/// outbox one refusal at a time.
///
/// The slack is bounded by what the driver can have outstanding, which
/// is `max_concurrent_queries` — its own ceiling, refused above it. So
/// this cannot become the unbounded queue the capacity exists to rule
/// out, and it cannot lose a settlement a live provider is waiting on.
const fn may_buffer_settlement(
    buffered: usize,
    event_capacity: usize,
    outstanding_queries: usize,
) -> bool {
    buffered < event_capacity.saturating_add(outstanding_queries)
}

// The directory's own pending queries and queued answers are folded into
// `pending_exchanges` and `answering_inbound` at the call site, so the
// predicate above needs no directory-specific term: a directory exchange
// costs a slot exactly as a direct one does.

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
/// What a scheduled retry leaves behind on the peer's claim.
#[derive(Debug, PartialEq, Eq)]
enum RetryClaim {
    /// A ticket owns it. `record_success` / `record_failure` /
    /// `record_permanent_failure` will settle it when the outcome
    /// arrives, so this tick must not touch it.
    Held,
    /// Offer this peer again on the next tick, without waiting out a
    /// fresh backoff it did not earn. "A denied dial must not reset
    /// retry state" applies to the scheduler exactly as it does to every
    /// other dial origin.
    Released,
    /// Do not reconsider until something else changes. Retrying an
    /// unauthorized, non-data-plane or draining peer on the very next
    /// tick would not become true by waiting a second.
    Cleared,
}

/// Does this refusal end the walk through a peer's candidate addresses?
///
/// A refusal about the PEER settles every address at once, so trying the
/// next one asks a question already answered. A refusal about this
/// address does not.
const fn refusal_settles_the_peer(refusal: &DialRefusal) -> bool {
    matches!(
        refusal,
        DialRefusal::Policy(
            DialDenial::Unauthorized
                | DialDenial::NotAuthorizedForDataPlane
                | DialDenial::ShuttingDown
        )
    )
}

/// The claim verdict for one peer after its candidates were walked.
///
/// Extracted from the retry arm because it was unreachable from a test:
/// the decision sat inside a `tokio::select!` branch that needs a live
/// interval, a Swarm and a ConnectionManager to enter at all. The rule
/// it encodes — release on an ordinary refusal, clear only when
/// authorization itself no longer holds — is the difference between a
/// peer that reconnects on the next tick and one that waits out a
/// backoff it never earned.
const fn retry_claim(ticketed: bool, last: Option<&DialRefusal>) -> RetryClaim {
    if ticketed {
        return RetryClaim::Held;
    }
    match last {
        Some(refusal) if refusal_settles_the_peer(refusal) => RetryClaim::Cleared,
        _ => RetryClaim::Released,
    }
}

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

        // THE ROOT ADMISSION EXISTS BEFORE THE SWARM. The outbound gate
        // inside the behaviour admits behaviour-originated dials through
        // the manager's snapshot handle, so the manager — and the clock
        // and in-flight set it shares with the runtime loop — must be
        // constructed first. The ordering CLAUDE.md §3 demands, made
        // structural: a behaviour cannot be built without the admission
        // it consults.
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
        // therefore evaluated at the same instant forever. `Instant`
        // rather than wall time so a clock adjustment cannot move a
        // deadline. Shared with the gate: two clock origins would
        // timestamp admissions and settlements on different axes, which
        // is SPIKE-003's F8b in a new disguise.
        let started = tokio::time::Instant::now();

        // Tickets for dials the Swarm has accepted and not yet reported
        // on. Keyed by the connection id the dial was built with, which
        // is knowable before dialling and is what the outcome event
        // carries back. SHARED with the gate, which deposits a
        // behaviour dial's ticket in its pending hook and re-binds it
        // at establishment.
        let in_flight = InFlightTickets::default();
        let outbound = OutboundAdmission::new(manager.handle(), in_flight.clone(), started);

        // The Kademlia behaviour exists only when configured: a profile
        // with no enabled kademlia entry advertises nothing, answers
        // nothing, and dials nothing (§13). Validated by
        // `SubstrateConfig::validate` before anything was built.
        let local_pid = libp2p::PeerId::from_public_key(&keypair.public());
        let (kad_toggle, mut kademlia_state) = match &config.kademlia {
            Some(settings) => (
                libp2p::swarm::behaviour::toggle::Toggle::from(Some(
                    kademlia_driver::build_behaviour(settings, local_pid)
                        .map_err(SubstrateError::Kademlia)?,
                )),
                Some(kademlia_driver::KademliaState::new(settings)),
            ),
            None => (libp2p::swarm::behaviour::toggle::Toggle::from(None), None),
        };

        let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| SubstrateError::Transport(e.to_string()))?
            // The behaviour is now fallible: GossipSub refuses a
            // configuration whose authenticity and validation mode
            // disagree, at construction. Boxed because the builder wants
            // an error that implements `Error`, and a contradiction here
            // should stop the runtime starting rather than panic inside
            // the task that would have driven it.
            .with_behaviour(|key| {
                SubstrateBehaviour::new(key, config.preauth, outbound, kad_toggle)
                    .map_err(Box::<dyn std::error::Error + Send + Sync>::from)
            })
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
        // Listeners that have bound. See `ActiveListeners`.
        let mut active: ActiveListeners = HashMap::new();

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
        // Broadcast holds its own copy for the same reason, and its own
        // ingress buckets: ADR-0026's amendment accounts the two modes
        // apart so neither can spend the other's allowance.
        let mut broadcast_state = broadcast::BroadcastState::new(&trust);

        // A shutdown that is waiting for in-flight exchanges: the
        // deadline it must not pass, and the caller to answer when it
        // finishes or expires.
        let mut stopping: Option<(tokio::time::Instant, oneshot::Sender<()>)> = None;
        let mut pending_direct: HashMap<
            libp2p::request_response::OutboundRequestId,
            PendingDirect,
        > = HashMap::new();

        // The directory's task state: the advisory cache, the query
        // budget, and the answers still being written. Built from the
        // runtime config, like the direct dedup and reservation limits.
        let mut directory_state = endpoints::DirectoryState::new(
            now_ms(started),
            config.directory_cache_peers,
            config.directory_cache_ttl_ms,
        );
        // Outbound directory exchanges awaiting a response, keyed by the
        // request id the way `pending_direct` is.
        let mut pending_endpoints: HashMap<
            libp2p::request_response::OutboundRequestId,
            endpoints::PendingQuery,
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
                        pending_endpoints.len(),
                        direct_state.answering(),
                        directory_state.answering(),
                        tokio::time::Instant::now() >= *deadline,
                    )
                {
                    flush_outbox(&mut outbox, &event_tx);
                    if let Some((_, reply)) = stopping.take() {
                        let _ = reply.send(());
                    }
                    break;
                }

                // The deadline, when a shutdown is waiting out its
                // grace. `None` the rest of the time, and the select
                // branch below is inert then.
                let grace_deadline = stopping.as_ref().map(|(deadline, _)| *deadline);

                let outstanding_queries = kademlia_state
                    .as_ref()
                    .map_or(0, |s| s.outstanding_queries());
                let room = polling_room(
                    outbox.len(),
                    config.event_capacity,
                    listens.len(),
                    pending_direct.len() + pending_endpoints.len(),
                    direct_state.answering() + directory_state.answering(),
                    outstanding_queries,
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
                                    &in_flight,
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
                                    Err(refusal) => {
                                        let settled = refusal_settles_the_peer(&refusal);
                                        last = Some(refusal);
                                        if settled {
                                            break;
                                        }
                                    }
                                }
                            }
                            match retry_claim(ticketed, last.as_ref()) {
                                RetryClaim::Held => {}
                                RetryClaim::Cleared => manager.clear_retry_claim(&peer),
                                RetryClaim::Released => manager.release_retry_claim(&peer),
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
                                // BASE CAPACITY ONLY. The slack above
                                // `event_capacity` is PROGRESS capacity:
                                // `polling_room` adds one slot per
                                // pending listener, exchange and inbound
                                // response precisely so the Swarm keeps
                                // being polled until those finish.
                                // Counting `listens.len()` here let this
                                // diagnostic take the slot reserved for
                                // observing `NewListenAddr` — after
                                // which `polling_room` is false, the
                                // Swarm is no longer polled, and the
                                // `listen()` caller waits forever for
                                // progress the runtime has just disabled.
                                // The deadlock this reservation exists to
                                // prevent, caused by an informational
                                // event that nothing is waiting on.
                                if may_buffer_delivery(outbox.len(), config.event_capacity) {
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
                                // THE DRIVER STOPS ON THIS PATH TOO.
                                // Review finding on PR #61: the drain
                                // arm told the driver to shut down, but
                                // `SwarmRuntime::shutdown` sends
                                // `Shutdown`, which is intercepted here
                                // and never reaches `handle_command`.
                                // So an ordinary shutdown dropped every
                                // outstanding Kademlia query without a
                                // `QueryFailed`, and the provider's
                                // budget permits — settled only by a
                                // completion — leaked for good; and
                                // through the grace below the behaviour
                                // went on serving and querying while the
                                // rest of the runtime wound down.
                                //
                                // Before the early break, so the
                                // common case is covered rather than
                                // only the graceful one.
                                if let Some(state) = kademlia_state.as_mut()
                                    && let Some(behaviour) = swarm.kademlia_mut()
                                {
                                    for event in kademlia_driver::handle_command(
                                        state,
                                        behaviour,
                                        &manager,
                                        interweave_kademlia_control_api::KademliaCommand::Shutdown,
                                        now_ms(started),
                                    ) {
                                        outbox.push_back(SwarmEvent::Kademlia { event });
                                    }
                                }
                                // NOTHING IN FLIGHT IS THE COMMON CASE,
                                // and it still stops immediately.
                                if shutdown_settled(
                                    pending_direct.len(),
                                    pending_endpoints.len(),
                                    direct_state.answering(),
                                    directory_state.answering(),
                                    false,
                                ) || stopping.is_some()
                                {
                                    flush_outbox(&mut outbox, &event_tx);
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
                                    &mut active,
                                    &mut pending_direct,
                                    &mut pending_endpoints,
                                    &mut direct_state,
                                    &mut directory_state,
                                    &mut broadcast_state,
                                    &in_flight,
                                    kademlia_state.as_mut(),
                                    config.max_pending_listens,
                                    config.max_active_listeners,
                                    config.max_payload_bytes,
                                    now_ms(started),
                                    wall_ms(),
                                    &mut outbox,
                                    config.event_capacity,
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
                        // BROADCAST FIRST, and it consumes its own
                        // events before `translate` sees them — the same
                        // shape direct uses, for the same reason: a
                        // shape conversion knows nothing about admission
                        // and would announce a message this node has not
                        // decided about.
                        let event = match broadcast::handle_broadcast(
                            event,
                            &mut swarm,
                            &mut broadcast_state,
                            &mut outbox,
                            broadcast::BroadcastTick {
                                now_ms: now_ms(started),
                                wall_ms: wall_ms(),
                                max_payload_bytes: config.max_payload_bytes,
                                draining: manager.is_draining(),
                                may_buffer_delivery: may_buffer,
                                event_capacity: config.event_capacity,
                            },
                        ) {
                            broadcast::BroadcastHandled::Consumed => continue,
                            broadcast::BroadcastHandled::Passed(event) => *event,
                        };
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

                        // THE DIRECTORY, consumed before `translate` for
                        // the same reason direct and broadcast are: an
                        // inbound query carries a `ResponseChannel` that
                        // cannot be borrowed out of a shared reference,
                        // and `translate` is a shape conversion that
                        // knows nothing about trust or the budget.
                        let event = match endpoints::handle_endpoints(
                            event,
                            &mut swarm,
                            &mut direct_state,
                            &mut directory_state,
                            &manager,
                            &mut pending_endpoints,
                            endpoints::EndpointsTick {
                                now_ms: now_ms(started),
                                wall_ms: wall_ms(),
                            },
                        ) {
                            endpoints::Handled::Consumed => continue,
                            endpoints::Handled::Passed(event) => *event,
                        };

                        // THE DRIVER SEES IT FIRST: kad events fold
                        // onto the port and stop here; Identify is
                        // peeked (F3) and passes on to the settlement
                        // and translation below.
                        let event = if let Some(state) = kademlia_state.as_mut() {
                            let mut kad_events = Vec::new();
                            let handled = kademlia_driver::handle_kademlia(
                                event,
                                &mut swarm,
                                state,
                                &manager,
                                now_ms(started),
                                &mut kad_events,
                            );
                            for event in kad_events {
                                outbox.push_back(SwarmEvent::Kademlia { event });
                            }
                            match handled {
                                kademlia_driver::KadHandled::Consumed => continue,
                                kademlia_driver::KadHandled::Passed(event) => *event,
                            }
                        } else {
                            event
                        };

                        let mut refuse = Vec::new();
                        let announce = settle_outcome(
                            &event,
                            &mut manager,
                            &in_flight,
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
                            Announce::Yes => {
                                translate(event, &mut listens, &mut active, &mut abandoned)
                            }
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
                        // THE MESH LEARNS THE CLASS HERE. GossipSub does
                        // no connection admission of its own, so a peer
                        // reaching it has already passed the dial gate --
                        // but the gate answers once, at connection time,
                        // and a class can change while a connection stays
                        // up. Syncing on every announced connection is the
                        // half that costs nothing; `SetTrust` is the half
                        // that matters.
                        if let Some(SwarmEvent::Connected { peer }) = translated.as_ref()
                            && let Ok(id) = to_peer_id(peer)
                        {
                            let trusted = mesh_admits(manager.classify(peer));
                            swarm.sync_broadcast_admission(&id, trusted);
                        }
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
            // LEAVE THE MESH BEFORE DROPPING IT. A dropped swarm closes
            // connections, and a peer that sees a connection close learns
            // nothing about subscriptions -- it keeps this node in its
            // topic set until its own timers age the entry out, and goes
            // on forwarding to a peer that is gone. Unsubscribing first
            // sends the leave while there is still a connection to send
            // it on.
            //
            // `shutdown_settled`'s arithmetic decided WHEN to get here and
            // is deliberately untouched: three prior mistakes are recorded
            // in its comment, and this is not a fourth.
            let leaving = !broadcast_state.channels.is_empty();
            for wire in broadcast_state.channels.keys() {
                swarm.unsubscribe_topic(&libp2p::gossipsub::IdentTopic::new(wire.clone()));
            }

            // AND THEN POLL, because unsubscribing only queues the RPC
            // into the behaviour. Dropping the swarm here would discard
            // it unsent, and the leave would exist in this node's memory
            // and nowhere else -- a shutdown that looks correct from the
            // inside and changes nothing on the wire. The first version
            // of this passed its test in isolation on exactly that
            // timing luck, and failed under a loaded suite.
            //
            // Bounded, because this runs after the caller was answered:
            // a peer that cannot accept the leave promptly must not hold
            // shutdown open. What it costs when it expires is the same
            // stale topic entry that not sending at all would leave.
            if leaving {
                let flush = tokio::time::sleep(Duration::from_millis(250));
                tokio::pin!(flush);
                loop {
                    tokio::select! {
                        () = &mut flush => break,
                        _ = swarm.select_next_some() => {}
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
}

#[cfg(test)]
mod flush_tests {
    #![allow(clippy::expect_used)]
    use super::{SwarmEvent, flush_outbox};
    use interweave_kademlia_control_api::QueryHandle;
    use std::collections::VecDeque;
    use tokio::sync::mpsc;

    fn kad_settlement() -> SwarmEvent {
        SwarmEvent::Kademlia {
            event: interweave_kademlia_control_api::KademliaEvent::QueryFailed {
                handle: QueryHandle::commanded(1),
                class: interweave_kademlia_control_api::QueryClass::Exploration,
                reason: interweave_kademlia_control_api::QueryFailure::ShuttingDown,
            },
        }
    }

    #[tokio::test]
    async fn a_shutdown_delivers_the_settlements_it_queued() {
        // Review finding on PR #61: the shutdown path invoked the driver
        // and pushed its `QueryFailed` events into the outbox, and the
        // `break` on the very next line dropped the queue. A query permit
        // is released only by a completion, so the settlement the
        // shutdown exists to produce reached nobody.
        let (tx, mut rx) = mpsc::channel(8);
        let mut outbox: VecDeque<SwarmEvent> = VecDeque::new();
        outbox.push_back(kad_settlement());
        outbox.push_back(kad_settlement());

        flush_outbox(&mut outbox, &tx);
        assert!(outbox.is_empty(), "everything the channel could take went");
        assert!(
            matches!(rx.try_recv(), Ok(SwarmEvent::Kademlia { .. })),
            "and the consumer actually receives it"
        );
        assert!(matches!(rx.try_recv(), Ok(SwarmEvent::Kademlia { .. })));
    }

    #[tokio::test]
    async fn a_full_channel_ends_the_flush_rather_than_blocking_it() {
        // BEST EFFORT is the contract, not an accident: awaiting room
        // would let a consumer that stopped reading hang the shutdown it
        // was asked to perform.
        let (tx, _rx) = mpsc::channel(1);
        let mut outbox: VecDeque<SwarmEvent> = VecDeque::new();
        outbox.push_back(kad_settlement());
        outbox.push_back(kad_settlement());
        outbox.push_back(kad_settlement());

        flush_outbox(&mut outbox, &tx);
        assert_eq!(
            outbox.len(),
            1,
            "one delivered, one consumed by the failed send, and the rest left \
             rather than the loop spinning or awaiting"
        );
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

    #[test]
    fn a_retry_diagnostic_cannot_consume_a_pending_listener_s_progress_slot() {
        // The reserved slack above `event_capacity` is what keeps the
        // Swarm polled until a pending listener sees `NewListenAddr`.
        // A failed scheduled retry that spends it stalls `listen()`
        // forever: the runtime stops polling for the very event that
        // would resolve the wait.
        //
        // Stated as the two calls the loop actually makes, at the exact
        // state the reporter identified: base capacity 1, one buffered
        // event, one pending listener.
        let event_capacity = 1;
        let buffered = 1;
        let listens = 1;

        assert!(
            polling_room(buffered, event_capacity, listens, 0, 0, 0),
            "with a listener pending the Swarm must still be polled"
        );
        assert!(
            !may_buffer_delivery(buffered, event_capacity),
            "and an informational event must not be buffered into that slot"
        );

        // The old spelling admitted it, and the admission is what turns
        // the next `polling_room` false.
        let old_spelling = buffered < event_capacity + listens;
        assert!(old_spelling, "the previous condition admitted the push");
        assert!(
            !polling_room(buffered + 1, event_capacity, listens, 0, 0, 0),
            "which is precisely the state where the listener can never resolve"
        );
    }

    /// The bound still bounds: with nothing in flight, the base capacity
    /// is the whole allowance.
    #[test]
    fn a_stalled_consumer_with_nothing_in_flight_stops_polling() {
        assert!(polling_room(0, 1, 0, 0, 0, 0));
        assert!(!polling_room(1, 1, 0, 0, 0, 0));
    }

    /// In-flight exchanges buy room, because polling is what settles
    /// them. Without this `send_direct` waits past its own deadline for
    /// a response nothing will ever process.
    #[test]
    fn in_flight_exchanges_keep_polling_alive() {
        assert!(
            polling_room(1, 1, 0, 1, 0, 0),
            "one exchange in flight, one event buffered: still polling"
        );
        assert!(
            !polling_room(2, 1, 0, 1, 0, 0),
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
        assert!(polling_room(1, 1, 0, 1, 0, 0));
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
            polling_room(1, 1, 0, 0, 1, 0),
            "nothing else in flight, but an answer is waiting to be written"
        );
        assert!(
            !polling_room(2, 1, 0, 0, 1, 0),
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
        assert!(polling_room(1, 1, 1, 0, 0, 0));
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
        assert!(shutdown_settled(0, 0, 0, 0, false));
    }

    #[test]
    fn an_outbound_direct_exchange_holds_the_grace() {
        assert!(!shutdown_settled(1, 0, 0, 0, false));
    }

    /// THE SECOND DIRECTION. `pending_direct` counts outbound only, so a
    /// verdict reading it alone breaks the loop while an admitted
    /// request's answer is still queued — the response never written and
    /// the sender left to retry into a restarted node.
    #[test]
    fn an_inbound_direct_answer_holds_it_too() {
        assert!(
            !shutdown_settled(0, 0, 1, 0, false),
            "an answer queued but unwritten is still work in flight"
        );
    }

    /// AND THE DIRECTORY, both directions. A pending outbound query and a
    /// queued directory answer are work in flight exactly as their direct
    /// counterparts are; a verdict blind to them exits the grace early.
    #[test]
    fn a_directory_query_or_answer_holds_it() {
        assert!(
            !shutdown_settled(0, 1, 0, 0, false),
            "an in-flight outbound directory query is work"
        );
        assert!(
            !shutdown_settled(0, 0, 0, 1, false),
            "a queued directory answer is work"
        );
    }

    /// The deadline ends it either way, which is what makes the grace
    /// BOUNDED rather than a second protocol timeout.
    #[test]
    fn the_deadline_outranks_them_all() {
        assert!(shutdown_settled(5, 5, 5, 5, true));
    }
}

#[cfg(test)]
mod retry_claim_tests {
    use super::{DialDenial, DialRefusal, RetryClaim, refusal_settles_the_peer, retry_claim};

    /// A ticket owns the claim, so the tick leaves it alone.
    ///
    /// Settling it here would race the outcome: `record_success` and its
    /// siblings are what release it when the dial actually resolves, and
    /// a second release from this tick would let the next one start a
    /// parallel dial for the same peer.
    #[test]
    fn a_ticketed_peer_keeps_its_claim() {
        assert_eq!(retry_claim(true, None), RetryClaim::Held);
        assert_eq!(
            retry_claim(true, Some(&DialRefusal::Backend("ignored".into()))),
            RetryClaim::Held,
            "a refusal from an earlier candidate does not undo the ticket"
        );
    }

    /// An ordinary refusal RELEASES, so the peer is offered again next
    /// tick without waiting out a backoff it did not earn.
    #[test]
    fn an_ordinary_refusal_releases_rather_than_clearing() {
        for refusal in [
            DialRefusal::Backend("transport said no".into()),
            DialRefusal::Policy(DialDenial::PeerBackoff),
        ] {
            assert_eq!(
                retry_claim(false, Some(&refusal)),
                RetryClaim::Released,
                "{refusal:?} must not reset retry state"
            );
        }
    }

    /// Authorization that no longer holds CLEARS.
    ///
    /// Waiting a second does not make an unauthorized peer authorized,
    /// so re-offering it every tick is a busy loop against a decision
    /// that will not change on its own.
    #[test]
    fn authorization_failures_clear_the_claim() {
        for denial in [
            DialDenial::Unauthorized,
            DialDenial::NotAuthorizedForDataPlane,
            DialDenial::ShuttingDown,
        ] {
            assert_eq!(
                retry_claim(false, Some(&DialRefusal::Policy(denial))),
                RetryClaim::Cleared,
                "{denial:?} will not become true by waiting"
            );
            assert!(
                refusal_settles_the_peer(&DialRefusal::Policy(denial)),
                "{denial:?} settles every address, so the walk stops"
            );
        }
    }

    /// An address-specific refusal does not settle the peer.
    ///
    /// The next candidate is a different question, so the walk continues
    /// — which is the whole reason a peer has more than one address.
    #[test]
    fn an_address_refusal_leaves_the_other_candidates_worth_trying() {
        assert!(!refusal_settles_the_peer(&DialRefusal::Backend(
            "bad addr".into()
        )));
        assert!(!refusal_settles_the_peer(&DialRefusal::Policy(
            DialDenial::PeerBackoff
        )));
    }
}
