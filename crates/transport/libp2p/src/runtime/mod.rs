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

use crate::attribution::{Attributing, DialAttribution, always};
use crate::behaviour::SubstrateBehaviour;
use crate::gated_swarm::{GatedSwarm, mesh_admits};
use crate::outbound_gate::{InFlightTickets, OutboundAdmission};
use crate::refusals::DialRefusals;

mod broadcast;
mod commands;
mod config;
mod dialing;
/// The one `(peer, address)` key, for the outbound gate.
///
/// Re-exported rather than duplicated: the gate's established hook writes
/// this key into the ticket, so it must be the same function the admission
/// and the address book use. Review finding on PR #86.
pub(crate) use dialing::canonical_for_peer;
pub mod autonat_driver;
pub mod autonat_server_driver;
pub mod dcutr_driver;
mod direct;
mod endpoints;
mod handle;
pub mod kademlia_driver;
pub mod mdns_driver;
mod messages;
mod network_change;
mod path_race;
pub mod relay_driver;
pub mod relay_server_driver;

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

pub use messages::{
    DialRefusal, HolePunchOutcome, PathChange, PeerPath, RelayReservationOutcome,
    RelayServerOutcome, SwarmCommand, SwarmEvent,
};

pub use config::{
    DEFAULT_COMMAND_CAPACITY, DEFAULT_EVENT_CAPACITY, MAX_CONFIGURED_CAPACITY, SubstrateConfig,
    SubstrateError,
};

/// The relay client follows the direct-inbound verdict: a
/// `ConnectivityChanged` from the AutoNAT adapter sets the reservation
/// target the moment it is produced, in the same turn, rather than on
/// the next tick.
fn follow_verdict(
    event: &SwarmEvent,
    relay_state: Option<&mut relay_driver::RelayState>,
    swarm: &mut GatedSwarm,
    now_ms: u64,
    outbox: &mut VecDeque<SwarmEvent>,
    event_capacity: usize,
) {
    if let (SwarmEvent::ConnectivityChanged { direct_inbound, .. }, Some(state)) =
        (event, relay_state)
    {
        let mut relay_events = Vec::new();
        relay_driver::set_direct_inbound(state, swarm, *direct_inbound, now_ms, &mut relay_events);
        buffer_informational(outbox, event_capacity, relay_events);
    }
}

/// Queue informational events under the base-capacity rule: dropped
/// when the outbox has no base room, like every other diagnostic.
fn buffer_informational(
    outbox: &mut VecDeque<SwarmEvent>,
    event_capacity: usize,
    events: Vec<SwarmEvent>,
) {
    for event in events {
        if may_buffer_delivery(outbox.len(), event_capacity) {
            outbox.push_back(event);
        }
    }
}

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
///
/// AND A FOURTH TIME, ACROSS PROTOCOLS (review R1 on fa3eab8). A Kademlia
/// settlement buffered for a query that is no longer outstanding -- an
/// immediate `NoRoutingPeers` refusal, say -- sat in the outbox with no
/// allowance left to cover it, and took the slot a pending direct or
/// directory exchange had earned: at `event_capacity` 1 with one
/// notification and one such settlement buffered, a direct request made
/// `2 < 1 + 1`, false, and the request waited for an unrelated consumer.
/// So query transactions are judged APART: they are left out of the
/// count the allowances cover and never stop polling. What bounds them
/// is at their source: past `kademlia_driver::MAX_QUERY_TRANSACTION_EVENTS`
/// undelivered, the driver stops tracking new library-started queries
/// (`KademliaState::set_backlogged`, set each turn of the loop), and a
/// commanded query is bounded by the provider's permits. Two earlier
/// versions got this wrong in opposite directions: adding each buffered
/// transaction to the allowance made it pay for its own room, so the
/// count cancelled and the outbox grew without limit behind a stalled
/// consumer (#117's blind review, F1); stopping the Swarm at the
/// transaction bound fixed that and stalled every caller waiting on
/// Swarm progress behind 48 Kademlia events (its re-review, F1).
const fn polling_room(
    buffered: usize,
    event_capacity: usize,
    pending_listens: usize,
    pending_exchanges: usize,
    answering_inbound: usize,
    outstanding_queries: usize,
    buffered_query_transactions: usize,
) -> bool {
    buffered.saturating_sub(buffered_query_transactions)
        < event_capacity
            .saturating_add(pending_listens)
            .saturating_add(pending_exchanges)
            .saturating_add(answering_inbound)
            .saturating_add(outstanding_queries)
}

/// Tell the driver whether the outbox holds a full call's worth of
/// undelivered query transactions, and return how many it holds.
///
/// ONE CALL for both, on purpose: the count is what `polling_room` needs
/// every turn, so the flag that bounds the backlog at its source
/// (`KademliaState::set_backlogged`) cannot be dropped from the loop
/// without the count going with it. The loop is what makes the flag
/// true; without it both of the driver's guards go inert and the backlog
/// is unbounded again (#117's blind re-review, round 3, F1).
/// `the_backlog_flag_follows_the_outbox` pins the threshold.
fn mark_query_backlog(
    kademlia_state: Option<&mut kademlia_driver::KademliaState>,
    outbox: &VecDeque<SwarmEvent>,
) -> usize {
    let transactions = buffered_query_transactions(outbox);
    if let Some(state) = kademlia_state {
        state.set_backlogged(transactions >= kademlia_driver::MAX_QUERY_TRANSACTION_EVENTS);
    }
    transactions
}

/// The query transaction events waiting in the outbox
/// (`kademlia_driver::is_query_transaction`).
fn buffered_query_transactions(outbox: &VecDeque<SwarmEvent>) -> usize {
    outbox
        .iter()
        .filter(|event| {
            matches!(event, SwarmEvent::Kademlia { event } if kademlia_driver::is_query_transaction(event))
        })
        .count()
}

/// Whether a Kademlia query TRANSACTION event may be buffered.
///
/// A settlement is not a notification and must not be judged as one:
/// the provider bound a budget permit before issuing the query, and only
/// its completion releases it. The rule has been wrong in both
/// directions. Gated on base capacity, a settlement was dropped whenever
/// the outbox was MOMENTARILY full and the permit was gone for the life
/// of the process. Pushed unconditionally, the command branch -- which
/// [`polling_room`] does not gate -- could drain the bounded command
/// channel into an unbounded outbox, one immediate refusal at a time.
/// And judged against the outbox length plus the queries outstanding
/// (review R1/R2 on fa3eab8), it did both at once: a settlement for a
/// query already refused could be DROPPED behind notifications (R2), and
/// one it did buffer took a slot another protocol's exchange had
/// earned (R1).
///
/// So transactions are counted apart, against a bound of their own that
/// no permit-holding provider can reach
/// (`kademlia_driver::MAX_BUFFERED_QUERY_TRANSACTIONS`), and
/// notifications neither make room for them nor take it. A caller
/// issuing queries past every permit it could hold is refused at that
/// bound, which keeps the outbox bounded without costing a real provider
/// a permit. `the_command_paths_bound_sits_above_everything_the_swarm_side_can_buffer`
/// pins the arithmetic.
const fn may_buffer_settlement(buffered_query_transactions: usize) -> bool {
    buffered_query_transactions < kademlia_driver::MAX_BUFFERED_QUERY_TRANSACTIONS
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

/// Deliver what mDNS holds, ONE EVENT PER SLOT.
///
/// It used to wait for room for both events at once
/// (`may_buffer_delivery(outbox.len() + 1, ..)`), which at an
/// `event_capacity` of one -- a value `SubstrateConfig::validate`
/// accepts -- is never true: the first hold ended mDNS delivery for
/// good, and every later event was held behind it and then counted over
/// the bound (#111 mDNS review F3). The discovery and retraction holds
/// are disjoint by pair, so splitting them across slots cannot net a pair
/// wrongly; retractions go first so a consumer at capacity has the room
/// before the discoveries that need it. Held failures follow, one a slot.
/// `held_mdns_changes_flush_one_slot_at_a_time` pins it at capacity 1.
fn flush_held_mdns(
    state: &mut mdns_driver::MdnsState,
    outbox: &mut VecDeque<SwarmEvent>,
    event_capacity: usize,
    now_ms: u64,
) {
    // RETRACTIONS FIRST. The provider holds a fixed capacity, as the
    // crate's record store does, so a held discovery delivered before a
    // held retraction can be refused for want of the room the retraction
    // was about to make (#112, the automated review's P1, at the crate;
    // the same order here).
    if state.holds_expired() && may_buffer_delivery(outbox.len(), event_capacity) {
        let expired = state.take_held_expired();
        outbox.push_back(SwarmEvent::MdnsExpired { expired });
    }
    if state.holds_discovered() && may_buffer_delivery(outbox.len(), event_capacity) {
        let candidates = state.take_held_discovered(now_ms);
        outbox.push_back(SwarmEvent::MdnsDiscovered { candidates });
    }
    while may_buffer_delivery(outbox.len(), event_capacity)
        && let Some((address, detail)) = state.take_held_failure()
    {
        outbox.push_back(SwarmEvent::MdnsInterfaceFailed { address, detail });
    }
    if may_buffer_delivery(outbox.len(), event_capacity)
        && let Some(detail) = state.take_held_watcher_failure()
    {
        outbox.push_back(SwarmEvent::MdnsWatcherFailed { detail });
    }
    if may_buffer_delivery(outbox.len(), event_capacity)
        && let Some(detail) = state.take_held_rebuild_failure()
    {
        outbox.push_back(SwarmEvent::MdnsRebuildFailed { detail });
    }
}

/// One mDNS crate event, delivered or HELD -- NOT DROPPED -- when the
/// outbox has no room, and held too while anything else is, so a fresh
/// event cannot overtake an older held one for the same pair (#111
/// review F1). Its own function so every hold branch is unit-tested
/// (`every_mdns_event_is_held_behind_a_full_outbox_or_an_older_hold`,
/// #112 blind review N7); inline in the Swarm task, no test reached them.
fn deliver_mdns<'a>(
    state: &mut mdns_driver::MdnsState,
    heard: libp2p::mdns::Event,
    own_listeners: impl IntoIterator<Item = &'a str> + Clone,
    now_ms: u64,
    outbox: &mut VecDeque<SwarmEvent>,
    event_capacity: usize,
) {
    let deliverable = |state: &mdns_driver::MdnsState, outbox: &VecDeque<SwarmEvent>| {
        !state.holds_anything() && may_buffer_delivery(outbox.len(), event_capacity)
    };
    match heard {
        libp2p::mdns::Event::Discovered(pairs) => {
            let candidates = state.on_discovered(&pairs, own_listeners, now_ms);
            deliver_mdns_candidates(state, candidates, outbox, event_capacity);
        }
        libp2p::mdns::Event::Expired(pairs) => {
            let expired = state.on_expired(&pairs);
            if !expired.is_empty() {
                if deliverable(state, outbox) {
                    outbox.push_back(SwarmEvent::MdnsExpired { expired });
                } else {
                    state.hold_expired(expired);
                }
            }
        }
        // ADR-0053 rule 5: the crate's failures, which used to stop
        // inside it.
        libp2p::mdns::Event::InterfaceFailed { address, reason } => {
            if deliverable(state, outbox) {
                outbox.push_back(SwarmEvent::MdnsInterfaceFailed {
                    address,
                    detail: reason,
                });
            } else {
                state.hold_failure(address, reason);
            }
        }
        // ADR-0053 rule 5: the watcher's own failure, once until it
        // recovers.
        libp2p::mdns::Event::WatcherFailed { reason } => {
            // The rebuild that answers it runs on the next refresh tick
            // (`mdns_tick`), whether or not this report is delivered now.
            state.want_rebuild();
            if deliverable(state, outbox) {
                outbox.push_back(SwarmEvent::MdnsWatcherFailed { detail: reason });
            } else {
                state.hold_watcher_failure(reason);
            }
        }
    }
}

/// Candidates delivered, or held behind a full outbox or an older hold,
/// exactly as a discovery is (`deliver_mdns`); a refresh takes the same
/// path so it can neither overtake nor be lost behind one.
fn deliver_mdns_candidates(
    state: &mut mdns_driver::MdnsState,
    candidates: Vec<interweave_discovery_api::CandidatePeer>,
    outbox: &mut VecDeque<SwarmEvent>,
    event_capacity: usize,
) {
    if candidates.is_empty() {
        return;
    }
    if !state.holds_anything() && may_buffer_delivery(outbox.len(), event_capacity) {
        outbox.push_back(SwarmEvent::MdnsDiscovered { candidates });
    } else {
        state.hold_discovered(candidates);
    }
}

/// Re-push what the mDNS crate's store still holds (ADR-0053 rule 10).
///
/// `records` is the crate's store as `discovered_records` yields it; a
/// record whose expiry is not after `at` is one the crate has not yet
/// swept, and is left for the crate's own `Expired` rather than kept
/// alive by this push. What survives goes through the boundary again
/// (`MdnsState::on_refresh`) and out as an `MdnsDiscovered`, which the
/// provider's dedup turns into an extended expiry.
fn refresh_mdns<'a>(
    state: &mut mdns_driver::MdnsState,
    records: impl IntoIterator<Item = (PeerId, libp2p::Multiaddr, std::time::Instant)>,
    at: std::time::Instant,
    own_listeners: impl IntoIterator<Item = &'a str> + Clone,
    now_ms: u64,
    outbox: &mut VecDeque<SwarmEvent>,
    event_capacity: usize,
) {
    let live: Vec<(PeerId, libp2p::Multiaddr)> = records
        .into_iter()
        .filter(|(_, _, expiry)| *expiry > at)
        .map(|(peer, address, _)| (peer, address))
        .collect();
    let candidates = state.on_refresh(&live, own_listeners, now_ms);
    deliver_mdns_candidates(state, candidates, outbox, event_capacity);
}

/// The mDNS refresh timer (ADR-0053 rule 10): first due one
/// `REFRESH_INTERVAL` after start, since a record the crate has held for
/// less was just reported as a discovery, and every `REFRESH_INTERVAL`
/// after. `Delay`, so a task that was busy does not fire a backlog of
/// ticks, each a refresh and possibly a rebuild.
/// `the_mdns_refresh_timer_fires_every_refresh_interval` pins the period.
fn mdns_refresh_timer() -> tokio::time::Interval {
    let mut timer = tokio::time::interval_at(
        tokio::time::Instant::now() + mdns_driver::REFRESH_INTERVAL,
        mdns_driver::REFRESH_INTERVAL,
    );
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    timer
}

/// One mDNS refresh tick: the refresh (ADR-0053 rule 10), then, if a
/// `WatcherFailed` has arrived since the last successful rebuild, the
/// rebuild (rule 5).
///
/// THE REBUILD WAITS FOR THE TICK rather than answering the event: a
/// replacement whose watcher fails at once reports again, and a rebuild
/// per report would be the spin rule 5 stopped, one layer up. So there is
/// at most one rebuild per call, and the runtime calls this once per
/// [`mdns_refresh_timer`] tick. `build` is called only when a rebuild is
/// due.
///
/// The refresh comes FIRST, reading the store the rebuild is about to
/// replace, so the provider keeps what the old crate held for one more
/// TTL while the fresh one rediscovers it. Then what the replaced
/// behaviour has not delivered is drained and delivered (or held) like
/// any event, rather than dropped with it; then the swap, which keeps its
/// drop counts (`mdns_driver::rebuild`). A rebuild that cannot build a
/// watcher keeps the running behaviour -- it still serves the interfaces
/// it has -- stays due for the next tick, and is reported as
/// `MdnsRebuildFailed`, held behind a full outbox or an older hold.
#[allow(clippy::too_many_arguments)]
fn mdns_tick<P: libp2p::mdns::Provider>(
    state: &mut mdns_driver::MdnsState,
    field: &mut libp2p::swarm::behaviour::toggle::Toggle<
        crate::mdns_scope::MdnsScope<libp2p::mdns::Behaviour<P>>,
    >,
    build: impl FnOnce() -> std::io::Result<crate::mdns_scope::MdnsScope<libp2p::mdns::Behaviour<P>>>,
    listening: &[(libp2p::core::transport::ListenerId, libp2p::Multiaddr)],
    counts: &mdns_driver::DropCountsCell,
    at: std::time::Instant,
    now_ms: u64,
    outbox: &mut VecDeque<SwarmEvent>,
    event_capacity: usize,
) {
    let own: Vec<String> = listening.iter().map(|(_, a)| a.to_string()).collect();
    if let Some(mdns) = field.as_ref() {
        let records: Vec<(PeerId, libp2p::Multiaddr, std::time::Instant)> = mdns
            .inner()
            .discovered_records()
            .map(|(peer, address, expiry)| (*peer, address.clone(), expiry))
            .collect();
        refresh_mdns(
            state,
            records,
            at,
            own.iter().map(String::as_str),
            now_ms,
            outbox,
            event_capacity,
        );
    }
    if !state.rebuild_due() {
        return;
    }
    match build() {
        Ok(fresh) => {
            for heard in mdns_driver::drain_replaced(field) {
                deliver_mdns(
                    state,
                    heard,
                    own.iter().map(String::as_str),
                    now_ms,
                    outbox,
                    event_capacity,
                );
            }
            mdns_driver::rebuild(
                field,
                fresh,
                listening.iter().map(|(id, a)| (*id, a)),
                counts,
            );
            state.rebuilt();
        }
        Err(why) => {
            let detail = why.to_string();
            if !state.holds_anything() && may_buffer_delivery(outbox.len(), event_capacity) {
                outbox.push_back(SwarmEvent::MdnsRebuildFailed { detail });
            } else {
                state.hold_rebuild_failure(detail);
            }
        }
    }
}

/// The host resolver configuration, or an empty one and the reason.
///
/// An empty configuration names no nameserver, so every lookup fails at
/// dial as an ordinary lookup failure and a literal-only profile works
/// exactly as before the DNS transport was built.
/// `a_missing_resolver_configuration_degrades_to_an_empty_one` pins the
/// mapping and that the empty configuration builds a transport.
fn resolver_or_empty<E: std::fmt::Display>(
    read: Result<(libp2p::dns::ResolverConfig, libp2p::dns::ResolverOpts), E>,
) -> (
    libp2p::dns::ResolverConfig,
    libp2p::dns::ResolverOpts,
    Option<SwarmEvent>,
) {
    match read {
        Ok((config, opts)) => (config, opts, None),
        Err(e) => (
            libp2p::dns::ResolverConfig::from_parts(None, Vec::new(), Vec::new()),
            libp2p::dns::ResolverOpts::default(),
            Some(SwarmEvent::ResolverUnavailable {
                detail: e.to_string(),
            }),
        ),
    }
}

/// What an mDNS construction outcome becomes: a behaviour, or a reason.
///
/// `None` means the profile did not ask for LAN discovery. `Some(Ok)`
/// is the provider. `Some(Err)` is the case this function exists for --
/// `providers/mdns.md` §Failure's degraded-not-fatal rule, which the
/// runtime honours by coming up without the provider and saying so.
///
/// It returns no `Result`, so nothing it decides can take the transport
/// and the static and cache providers down with an optional provider.
/// That holds for THIS function and not for its caller: a `?` applied to
/// the construction result before it gets here (`.transpose()?`, as the
/// #111 re-review showed) would bypass it, and no test catches that --
/// the one failure that produces the degraded path is the kernel's
/// interface watcher, which a test cannot break without a test-only knob
/// in production configuration, and this repository has none.
///
/// It returns the EVENT to report, not a string, so the event's shape is
/// what the unit test pins; what remains untested is only the one line
/// in `start` that pushes it.
fn mdns_or_degraded<B>(
    built: Option<std::io::Result<B>>,
) -> (
    Option<B>,
    Option<mdns_driver::MdnsState>,
    Option<SwarmEvent>,
) {
    match built {
        None => (None, None, None),
        Some(Ok(behaviour)) => (Some(behaviour), Some(mdns_driver::MdnsState::new()), None),
        Some(Err(why)) => (
            None,
            None,
            Some(SwarmEvent::MdnsUnavailable {
                detail: why.to_string(),
            }),
        ),
    }
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
    /// What the outbound gate refused, kept HERE because nowhere else
    /// can reach it.
    ///
    /// The gate is moved into `SubstrateBehaviour`, which lives inside
    /// a private `GatedSwarm` inside the Swarm task, so once the
    /// runtime starts there is no path back to it. Cloning the handle
    /// before the move is what makes the record readable at all — and
    /// a record nobody can read leaves a denied behaviour dial exactly
    /// as invisible as it was, which is the defect this whole
    /// mechanism exists to close.
    refusals: DialRefusals,
    /// The AutoNAT server's counters, kept for the same reason as
    /// `refusals`; `None` when the profile serves no probes.
    autonat_server_counters: Option<crate::probe_server::ProbeCounterHandle>,
    /// The DCUtR wrapper's counters, likewise; `None` when the profile
    /// never hole punches.
    dcutr_counters: Option<crate::hole_punch::HolePunchCounterHandle>,
    /// The root funnel's counters. Not an `Option`: every Swarm this
    /// runtime builds has the funnel, whatever the profile enables.
    root_funnel_counters: crate::root_funnel::RootFunnelCounterHandle,
    /// The operator's door (ADR-0052 rule 9): what `add_address` records
    /// here is admitted at every learn site and at the root funnel
    /// whatever its class. The same set the Swarm task reads.
    operator: crate::operator_set::OperatorSet,
    /// Every store's learn-site counts (ADR-0052 rule 8).
    stores: crate::store_refusals::StoreRefusals,
    /// What ADR-0053's bounds dropped inside the mDNS crate, across
    /// rebuilds; `None` when the profile runs no mDNS.
    mdns_drop_counts: Option<mdns_driver::DropCountsCell>,
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
        Self::start_with_resolver(
            identity,
            config,
            trust,
            hickory_resolver::system_conf::read_system_conf(),
        )
    }

    /// [`Self::start`], with the host resolver configuration handed in.
    ///
    /// THE SEAM FOR ONE TEST: the host's `/etc/resolv.conf` cannot be
    /// removed from a test, so without this the "starts with no resolver"
    /// invariant rested on one untested line of `start` (#111 re-review,
    /// risk 1). `a_runtime_whose_resolver_read_fails_starts_and_says_so`
    /// starts a real runtime through it.
    fn start_with_resolver<E: std::fmt::Display>(
        identity: &ProfileIdentity,
        config: SubstrateConfig,
        trust: TrustSources,
        resolver: Result<(libp2p::dns::ResolverConfig, libp2p::dns::ResolverOpts), E>,
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
        // WHICH BEHAVIOUR ASKED, shared between the wrappers that write
        // it and the gate that reads it. One map per Swarm: a
        // `ConnectionId` is unique within a Swarm and means nothing
        // outside one.
        let attribution = DialAttribution::default();
        let outbound = OutboundAdmission::new(
            manager.handle(),
            in_flight.clone(),
            attribution.clone(),
            started,
        );
        // CLONED BEFORE THE MOVE, for the same reason `refusals` is:
        // the behaviour is about to take ownership of everything it
        // needs, and `ClassGated` needs the policy handle at
        // construction rather than later.
        let class_policy = manager.handle();
        // CLONED BEFORE THE MOVE. `outbound` is about to disappear into
        // the behaviour, and the Swarm discards a denied behaviour
        // dial, so this handle is the only way anything outside the
        // Swarm task learns a dial was refused.
        let refusals = outbound.refusals();
        // THE OPERATOR'S DOOR (ADR-0052 rule 9), one set for the whole
        // runtime, held beside the class-policy handle as the rule says.
        // Every place that applies the peer-supplied boundary consults
        // this set -- the root funnel, the Kademlia offer stash and
        // query-candidate hook, the AutoNAT and relay learned lists, the
        // Identify and mDNS learn sites -- and admits what is in it
        // whatever its class, because no peer chose it. (The list omitted
        // the three hooks this PR wired last; #111 review P3-9.)
        //
        // Seeded here from the profile's own configuration -- the static
        // relays, the static AutoNAT servers, and `operator_addresses`
        // for what no block carries (the static bootstrap peers) -- the
        // first half of the operator's door; `SwarmRuntime::add_address`
        // is the second. An address in a block that does not parse is
        // not an operator address anyone can dial, so it is simply not
        // recorded -- the validator refuses it long before this.
        let operator = crate::operator_set::OperatorSet::new();
        // Every store's learn-site count, one handle, readable from
        // `SwarmRuntime::store_refusals` after the Swarm moves into its
        // task (ADR-0052 rule 8).
        let stores = crate::store_refusals::StoreRefusals::new();
        for address in config.operator_seed() {
            if let Ok(parsed) = address.parse::<libp2p::Multiaddr>() {
                let _ = operator.insert(&parsed);
            }
        }

        // The Kademlia behaviour exists only when configured: a profile
        // with no enabled kademlia entry advertises nothing, answers
        // nothing, and dials nothing (§13). Validated by
        // `SubstrateConfig::validate` before anything was built.
        let local_pid = libp2p::PeerId::from_public_key(&keypair.public());
        let (kad_toggle, mut kademlia_state) = match &config.kademlia {
            Some(settings) => (
                libp2p::swarm::behaviour::toggle::Toggle::from(Some(Attributing::new(
                    kademlia_driver::build_behaviour(settings, local_pid)
                        .map_err(SubstrateError::Kademlia)?,
                    always(DialOrigin::KademliaQuery),
                    attribution.clone(),
                ))),
                Some({
                    let mut state = kademlia_driver::KademliaState::new(settings);
                    state.set_boundary(operator.clone(), stores.clone());
                    state
                }),
            ),
            None => (libp2p::swarm::behaviour::toggle::Toggle::from(None), None),
        };

        // The AutoNAT client, likewise only when configured (the owner's
        // 2026-09-07 ruling: gated off; `SubstrateConfig::autonat_client`
        // is the switch). Its state is the driver's -- the manager, the
        // re-test schedule, the counters -- and lives beside the Kademlia
        // state for the same reason: every mutation stays in the Swarm
        // task.
        // mDNS, likewise only when configured. It is a DISCOVERY
        // provider rather than a connectivity behaviour, so the owner's
        // 2026-09-07 gated-off ruling is not what places it here -- what
        // does is that a profile which did not ask for LAN discovery
        // must not join a multicast group and announce itself. The
        // socket is the side effect worth gating.
        // AN ENVIRONMENT FAILURE HERE DEGRADES THE PROVIDER, IT DOES NOT
        // KILL THE NODE. `providers/mdns.md` §Failure: "Networks may
        // block multicast, containers may lack multicast routing, and
        // interfaces may change. Such failures make this provider
        // degraded/unavailable but do not kill transport or static/cache
        // discovery."
        //
        // WHAT THIS ARM COVERS IS ONE OF THOSE CAUSES, not the list. Of
        // §Failure's causes only a failed interface watcher reaches here.
        // A per-interface bind or multicast join that fails, and a send
        // or receive error, arrive later as `MdnsInterfaceFailed`
        // (ADR-0053 rule 5; as released the crate logged them and emitted
        // nothing). A domain that silently drops packets is still not
        // detected, so `DISCOVERY-CONFORMANCE.md` guarantees 7 and 8 --
        // operational failures become health transitions -- are met for
        // the causes above and not for that one. An error the interface
        // watcher reports AFTER start was only logged until #112 and
        // arrives now as `MdnsWatcherFailed`, once until the watcher
        // recovers or, failing twice in a row, is no longer polled. An earlier version of this comment said they were met
        // here outright (#111 mDNS review F4).
        //
        // `build_behaviour`'s WHOLE failure surface is
        // `mdns::tokio::Behaviour::new`, which fails only at
        // `P::new_watcher()` -- the interface watcher, not a multicast
        // socket (a per-interface socket failure happens inside the
        // crate's own `poll`, skips that interface, and arrives later as
        // `MdnsInterfaceFailed`, ADR-0053 rule 5). None of the three
        // settings fields can cause it, so everything reaching this arm
        // is the environment. A settings rule the driver refuses is a
        // different question and is already fatal, in
        // `SubstrateConfig::validate`.
        //
        // The enforcement is `mdns_or_degraded`'s SIGNATURE, not this
        // comment: it returns no `Result`, so this arm has no `?` to
        // reintroduce.
        let (mdns_behaviour, mdns_state, mdns_unavailable) = mdns_or_degraded(
            config
                .mdns
                .as_ref()
                .map(|settings| mdns_driver::build_behaviour(settings, local_pid)),
        );
        // ADR-0053 rule 7: the crate's drop counts, taken while the
        // behaviour is still ours to reach, so they stay readable after
        // the Swarm owns it.
        // A CELL, because a rebuild (ADR-0053 rule 5) brings a behaviour
        // with counts of its own, and the handle's numbers must not reset.
        let mdns_drop_counts = mdns_behaviour
            .as_ref()
            .map(|b| mdns_driver::DropCountsCell::new(b.inner().drop_counts()));
        let task_mdns_drop_counts = mdns_drop_counts.clone();
        let mdns_toggle = libp2p::swarm::behaviour::toggle::Toggle::from(mdns_behaviour);
        let mut mdns_state =
            mdns_state.map(|state| state.with_boundary(operator.clone(), stores.clone()));

        let (autonat_toggle, mut autonat_state) = match &config.autonat_client {
            Some(settings) => (
                libp2p::swarm::behaviour::toggle::Toggle::from(Some(
                    autonat_driver::build_behaviour(settings),
                )),
                Some({
                    let mut state = autonat_driver::AutonatState::new(settings)
                        .map_err(SubstrateError::Autonat)?;
                    state.set_boundary(operator.clone(), stores.clone());
                    state
                }),
            ),
            None => (libp2p::swarm::behaviour::toggle::Toggle::from(None), None),
        };

        // The AutoNAT SERVER, under the same ruling and the same switch
        // shape. It holds no driver state of its own: the rules live in
        // `ProbeServer` where the events are, and the runtime only ticks
        // its clock and translates its events. Whether it exists is
        // read by the inbound arm below (`serving_probes`).
        let serving_probes = config.autonat_server.is_some();
        // The counter handle is CLONED BEFORE THE MOVE, like `refusals`:
        // once the field is in the Swarm, this is the only way a
        // consumer reads what the server refused and served.
        let (autonat_server_toggle, autonat_server_counters) = match &config.autonat_server {
            Some(settings) => {
                let (field, counters) = autonat_server_driver::build_behaviour(
                    settings,
                    attribution.clone(),
                    manager.handle(),
                );
                (field, Some(counters))
            }
            None => (libp2p::swarm::behaviour::toggle::Toggle::from(None), None),
        };

        // The relay CLIENT, under the same ruling and the same switch
        // shape -- with one difference the builder forces: the client is
        // a transport AND a behaviour, made together by
        // `with_relay_client`, so the switch is taken at the builder and
        // the two paths below build the same `Swarm` type. A profile
        // that reserves on no relay composes no relay transport at all,
        // and a `/p2p-circuit` address stays undialable for it. The
        // driver's state lives beside the AutoNAT state for the same
        // reason: every mutation stays in the Swarm task.
        let mut relay_state = match &config.relay_client {
            Some(settings) => Some({
                let mut state =
                    relay_driver::RelayState::new(settings).map_err(SubstrateError::Relay)?;
                state.set_boundary(operator.clone(), stores.clone());
                state
            }),
            None => None,
        };
        let relay_attribution = attribution.clone();
        let relay_policy = manager.handle();

        // THE SAME HANDLE THE OUTBOUND GATE READS. One snapshot source
        // for the whole behaviour: the gate decides whether a dial may
        // be made, `ClassGated` decides which protocols a connection is
        // offered, and both must agree about a peer's class or the
        // second is a second opinion rather than an enforcement.
        //
        // The behaviour is fallible: GossipSub refuses a configuration
        // whose authenticity and validation mode disagree, at
        // construction. Boxed because the builder wants an error that
        // implements `Error`, and a contradiction here should stop the
        // runtime starting rather than panic inside the task that would
        // have driven it.
        // The relay SERVER, under the same ruling and the same switch
        // shape as the AutoNAT server: no driver state of its own, the
        // ceilings in the crate's configuration translated from the
        // profile's, its events translated. Whether it exists is read by
        // the inbound arm below (`serving_relays`).
        let serving_relays = config.relay_server.is_some();
        let relay_server_toggle = match &config.relay_server {
            Some(settings) => {
                relay_server_driver::build_behaviour(settings, local_pid, manager.handle())
            }
            None => libp2p::swarm::behaviour::toggle::Toggle::from(None),
        };
        // THE KEEPALIVE on relay control connections (section 14 item
        // 5), with either relay role: switched per relay by the relay
        // driver as its reservations move (`relay_driver::sync`).
        let mut relay_reserved = relay_server_driver::Reserved::default();
        let relay_keepalive_field = crate::relay_keepalive::build_field(
            relay_state.is_some(),
            serving_relays,
            manager.handle(),
        );
        // DCUtR, under the same ruling and the same switch shape: the
        // crate under the attempt lifecycle, the attribution and the
        // data-plane class gate (`dcutr_driver.rs`).
        let (dcutr_toggle, dcutr_counters) = match &config.dcutr {
            Some(settings) => {
                let (field, counters) = dcutr_driver::build_behaviour(
                    settings,
                    local_pid,
                    attribution.clone(),
                    manager.handle(),
                );
                (field, Some(counters))
            }
            None => (libp2p::swarm::behaviour::toggle::Toggle::from(None), None),
        };
        let preauth = config.preauth;
        let funnel_operator = operator.clone();
        let make_behaviour =
            move |key: &libp2p::identity::Keypair, relay_client: relay_driver::ClientField| {
                SubstrateBehaviour::new(
                    key,
                    preauth,
                    outbound,
                    crate::behaviour::Configured {
                        kad: kad_toggle,
                        autonat_client: autonat_toggle,
                        autonat_server: autonat_server_toggle,
                        relay_client,
                        relay_server: relay_server_toggle,
                        relay_keepalive: relay_keepalive_field,
                        dcutr: dcutr_toggle,
                        mdns: mdns_toggle,
                    },
                    class_policy,
                )
                // THE ROOT FUNNEL (ADR-0052 A 2026-09-25 D1), around the
                // WHOLE composite and at the one site both builder
                // branches construct it, so no Swarm this runtime builds
                // is without it. A behaviour-extended dial -- Kademlia's
                // walk, the relay client's reservation -- is extended
                // only from what the root returns, and the root is this.
                // `tests/root_funnel.rs` measured that on real sockets.
                .map(|behaviour| {
                    crate::root_funnel::RootFunnel::new(behaviour, funnel_operator.clone())
                })
                .map_err(Box::<dyn std::error::Error + Send + Sync>::from)
            };
        let (resolver_config, resolver_opts, resolver_unavailable) = resolver_or_empty(resolver);
        let builder = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| SubstrateError::Transport(e.to_string()))?
            // THE DNS TRANSPORT, and the feature flag alone was never
            // this. `static-bootstrap.md` §DNS ownership says resolution
            // happens when the dial path consumes the multiaddress, so
            // until the builder wrapped the base transport a `/dns4`
            // address failed `MultiaddrNotSupported` -- classified
            // structural, correctly for a build that would fail it the
            // same way every time, so `record_permanent_failure` dropped
            // the address from the book instead of retrying it. Wrapping
            // it here is what turns a lookup failure back into an
            // ordinary dial diagnostic the ConnectionManager retries,
            // which is what ADR-0010 and that contract both say.
            //
            //
            // The host resolver configuration is read ONCE, here: a
            // DHCP or VPN change that moves the nameserver is not seen
            // until restart (#111 DNS review P3-5). And a host with none
            // gets an empty resolver and `ResolverUnavailable` rather than
            // a node that refuses to start -- see `resolver_or_empty`.
            .with_dns_config(resolver_config, resolver_opts);
        // THE HANDSHAKE TIMEOUT, taken from the same limits the
        // pre-auth gate enforces rather than left to libp2p's
        // default. The two happen to agree at ten seconds today,
        // and a configuration that narrowed one without the other
        // would produce a listener whose accounting and whose
        // transport disagreed about when a handshake is over --
        // slots reclaimed while the socket was still negotiating,
        // or the reverse.
        let handshake = Duration::from_millis(config.preauth.handshake_timeout_ms());
        let swarm = if relay_state.is_some() {
            builder
                .with_relay_client(noise::Config::new, yamux::Config::default)
                .map_err(|e| SubstrateError::Transport(e.to_string()))?
                .with_behaviour(|key, client| {
                    make_behaviour(
                        key,
                        relay_driver::build_behaviour(client, relay_attribution, relay_policy),
                    )
                })
                .map_err(|e| SubstrateError::Transport(e.to_string()))?
                .with_swarm_config(|c| c.with_idle_connection_timeout(config.idle_timeout))
                .with_connection_timeout(handshake)
                .build()
        } else {
            builder
                .with_behaviour(|key| {
                    make_behaviour(key, libp2p::swarm::behaviour::toggle::Toggle::from(None))
                })
                .map_err(|e| SubstrateError::Transport(e.to_string()))?
                .with_swarm_config(|c| c.with_idle_connection_timeout(config.idle_timeout))
                .with_connection_timeout(handshake)
                .build()
        };
        let mut swarm = GatedSwarm::new(swarm);
        // TAKEN HERE, before the Swarm moves into its task: afterwards
        // nothing outside the task can reach the behaviour, and a
        // removal nobody can read records nothing.
        let root_funnel_counters = swarm.root_funnel_counters();

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
        // The best path last announced per LOGICAL peer, from which
        // `Connected`, `PeerPathChanged` and `Disconnected` are derived
        // once per peer rather than once per connection
        // (`contracts/CONNECTIVITY.md` §5). Bounded by `open`, from
        // which every entry is computed.
        let mut paths: HashMap<TransportIdentity, messages::PeerPath> = HashMap::new();
        // `DialPeer`'s deferred circuit dials (§12's head-start, step 9)
        // and how long the head-start is: the relay client's setting,
        // since only a profile with the relay transport dials a circuit;
        // without one the book's circuit routes are undialable and no
        // race is ever deferred.
        let mut races = path_race::Races::default();
        // The bound set as last observed, for section 14's network
        // change (step 10).
        let mut network = network_change::NetworkSet::default();
        let head_start_ms = config
            .relay_client
            .as_ref()
            .map_or(0, |c| c.direct_head_start_ms);
        // The stability interval a punched direct connection must hold
        // before it is the peer's path (`DCUTR.md` §4, step 9); with no
        // DCUtR there is no punch and the interval decides nothing.
        let stability_ms = config
            .dcutr
            .as_ref()
            .map_or(0, |d| d.direct_stability_period_ms);

        // The scheduler's heartbeat. `Delay` rather than `Burst` so a
        // task that was busy does not then fire a backlog of ticks it
        // slept through, each one walking the retry table again.
        let mut retries = tokio::time::interval(config.retry_tick);
        retries.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // THE mDNS REFRESH, and the rebuild behind it (`mdns_tick`). It
        // ticks whether or not mDNS is configured; the arm is disabled
        // when it is not.
        let mut mdns_refresh = mdns_refresh_timer();

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

        // The Swarm task's own handle on the operator set; the runtime
        // keeps `operator` for `add_address`, the operator's command.
        let task_operator = operator.clone();
        let task_stores = stores.clone();
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

            // BEFORE ANYTHING ELSE, because this is the event that stops
            // ONE kind of degraded provider -- the one with no interface
            // watcher -- from being a silent one (the per-interface causes
            // arrive later as `MdnsInterfaceFailed`; see
            // `SwarmEvent::MdnsUnavailable`). A profile
            // that set `SubstrateConfig.mdns`, got no interface watcher
            // and heard nothing would hold a provider that looks
            // configured and never announces -- the shape this
            // repository names "a gate that looks like it is working".
            if let Some(event) = mdns_unavailable {
                outbox.push_back(event);
            }
            // And the resolver's, for the same reason: a node resolving
            // nothing must say so before its first dial to a name fails.
            if let Some(event) = resolver_unavailable {
                outbox.push_back(event);
            }

            loop {
                // mDNS STATE CHANGES HELD UNDER BACKPRESSURE go out as soon
                // as there is room, before anything this iteration adds.
                // Held rather than dropped because a lost `Discovered` is
                // not re-emitted until the crate's TTL lapses; see
                // `MdnsState::hold_discovered`.
                if let Some(state) = mdns_state.as_mut() {
                    flush_held_mdns(state, &mut outbox, config.event_capacity, now_ms(started));
                }

                // BOUNDED, per the resource rules: a consumer that stops
                // draining must not let a remote peer choose this
                // process's memory. The cap is the channel's own
                // capacity plus the progress slack `polling_room`
                // grants, and, apart from them, the query transactions'
                // own bound (`kademlia_driver::MAX_BUFFERED_QUERY_TRANSACTIONS`):
                // a stalled consumer costs a fixed multiple of what it
                // agreed to buffer, never a count the network chooses.
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

                // The earliest head-start to run out, as an instant on
                // the runtime's clock; inert when no race waits.
                let race_due = races
                    .next_due_ms()
                    .map(|due| started + Duration::from_millis(due));

                // THE BACKLOG IS THE DRIVER'S TO RESPECT, not a reason to
                // stop polling (`KademliaState::set_backlogged`).
                let transactions = mark_query_backlog(kademlia_state.as_mut(), &outbox);
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
                    transactions,
                );

                tokio::select! {
                    // THE HEAD-START RAN OUT (§12, step 9): a circuit
                    // route deferred behind a direct dial is dialled now
                    // unless a direct connection to the peer landed
                    // meanwhile -- in which case the race is over and
                    // the relay stays a route in the book for later.
                    () = tokio::time::sleep_until(race_due.unwrap_or_else(tokio::time::Instant::now)), if race_due.is_some() => {
                        let now = now_ms(started);
                        for (peer, relayed) in races.take_due(now) {
                            if open.values().any(|c| c.peer == peer && c.is_direct_data_plane()) {
                                continue;
                            }
                            let mut last = None;
                            for address in &relayed {
                                match attempt_dial(
                                    &mut swarm,
                                    &mut manager,
                                    &in_flight,
                                    &peer,
                                    address,
                                    DialOrigin::RelayCircuit,
                                    now,
                                ) {
                                    Ok(()) => {
                                        last = None;
                                        break;
                                    }
                                    Err(refusal) => last = Some(refusal),
                                }
                            }
                            // REPORTED, as a scheduled retry's refusal
                            // is (below): the caller was answered when
                            // the direct dial was admitted, so nobody
                            // holds a reply channel for the circuit,
                            // and a deferred dial the gate refused --
                            // the peer revoked meanwhile, the route
                            // quarantined, the runtime draining --
                            // would otherwise leave the consumer with
                            // one direct failure and no word of the
                            // race (PR #103 round 1). Base capacity
                            // only, dropped not queued, for the
                            // scheduler's reasons. Pinned by
                            // `tests/connectivity/tests/path_race.rs`'s
                            // `a_deferred_circuit_the_gate_refuses_is_reported`.
                            if let Some(refusal) = last
                                && may_buffer_delivery(outbox.len(), config.event_capacity)
                            {
                                outbox.push_back(SwarmEvent::DialFailed {
                                    peer: Some(peer.clone()),
                                    detail: format!("deferred circuit: {refusal:?}"),
                                });
                            }
                        }
                    }
                    // THE mDNS REFRESH TICK: what the crate's store still
                    // holds goes out again, and a rebuild that is due runs
                    // (`mdns_tick`).
                    _ = mdns_refresh.tick(), if mdns_state.is_some() => {
                        // All three are there together or not at all: the
                        // state and the cell are made from the behaviour
                        // the settings built.
                        if let (Some(state), Some(settings), Some(counts)) = (
                            mdns_state.as_mut(),
                            config.mdns.as_ref(),
                            task_mdns_drop_counts.as_ref(),
                        ) {
                            let listening: Vec<(libp2p::core::transport::ListenerId, libp2p::Multiaddr)> = active
                                .iter()
                                .flat_map(|(id, addresses)| addresses.iter().map(|a| (*id, a.clone())))
                                .collect();
                            mdns_tick(
                                state,
                                swarm.mdns_mut(),
                                || mdns_driver::build_behaviour(settings, local_pid),
                                &listening,
                                counts,
                                std::time::Instant::now(),
                                now_ms(started),
                                &mut outbox,
                                config.event_capacity,
                            );
                        }
                    }
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
                            // NOT THIS SCHEDULER'S TO REACH. A failed
                            // dial under a reachability origin -- the
                            // AutoNAT adapter's, toward an
                            // infrastructure-only server -- schedules
                            // a retry like any other, but this
                            // scheduler dials under its own origin,
                            // which the gate refuses for that class:
                            // the claim would be cleared with a
                            // `DialFailed` nobody can act on, once per
                            // failure, beside the adapter's own retry.
                            // The adapter re-dials what it dialled;
                            // this walks past it. ONLY the adapter's
                            // own targets: every other peer's retry --
                            // a revoked one, or one demoted to
                            // infrastructure that nobody re-dials --
                            // still goes to the gate and is refused
                            // and reported, which is the diagnostic
                            // `stage5_dial_admission::a_revoked_peer_is_not_retried`
                            // and `autonat_client::a_peer_demoted_to_infrastructure_…`
                            // pin for an operator watching a peer that
                            // never reconnects. Review findings on PR
                            // #89, rounds 1 and 4.
                            if autonat_state
                                .as_ref()
                                .is_some_and(|s| s.is_target(&peer))
                            {
                                manager.clear_retry_claim(&peer);
                                continue;
                            }
                            // A REVOKED PEER IS REFUSED AND REPORTED,
                            // before its candidates are asked for.
                            // `set_trust` now drops a revoked peer from
                            // the address book once it is past the
                            // retired bound (review R3 on fa3eab8), so
                            // its retry could find nothing to try below
                            // and be cleared in silence -- and an operator
                            // watching a peer that never reconnects
                            // would lose the one diagnostic that says
                            // why, which the gate's refusal used to
                            // give (`stage5_dial_admission::a_revoked_peer_is_not_retried`).
                            if matches!(
                                manager.classify(&peer),
                                interweave_transport_runtime::ConnectionClass::Unauthorized
                            ) {
                                manager.clear_retry_claim(&peer);
                                if may_buffer_delivery(outbox.len(), config.event_capacity) {
                                    outbox.push_back(SwarmEvent::DialFailed {
                                        peer: Some(peer.clone()),
                                        detail: format!(
                                            "scheduled retry: {:?}",
                                            DialRefusal::Policy(
                                                DialDenial::Unauthorized
                                            )
                                        ),
                                    });
                                }
                                continue;
                            }
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
                            // it refused. EXCEPT A CIRCUIT ROUTE (step
                            // 7): the book holds the circuit a peer was
                            // reached over, and the gate pairs a circuit
                            // address with `RelayCircuit` and no other
                            // origin -- under the scheduler's own the
                            // dial is undialable and the route is
                            // forgotten as a structural failure.
                            let mut last: Option<DialRefusal> = None;
                            let mut ticketed = false;
                            for address in candidates {
                                let origin = dialing::book_origin(
                                    &address,
                                    DialOrigin::ConnectionManager,
                                );
                                match attempt_dial(
                                    &mut swarm,
                                    &mut manager,
                                    &in_flight,
                                    &peer,
                                    &address,
                                    origin,
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

                        // THE TICK RECONCILES TOO. Review finding on
                        // PR #64: the library's automatic bootstrap can
                        // start on a connection that is already
                        // established — the routing insertion comes
                        // from `Identify::Received` — and then dials
                        // nothing, so it produces no Swarm event at all
                        // between starting and completing. Reconciling
                        // only after events left such a query uncharged
                        // for its whole lifetime on an otherwise idle
                        // node, with commanded work free to spend the
                        // entire budget beside it.
                        //
                        // This tick already exists and already walks a
                        // bounded table, so the uncharged window
                        // becomes `retry_tick` rather than the query's
                        // lifetime. Pushed like the event path pushes; a
                        // backlogged driver announces nothing new here
                        // either (`KademliaState::set_backlogged`).
                        if let Some(state) = kademlia_state.as_mut() {
                            let mut kad_events = Vec::new();
                            kademlia_driver::reconcile(
                                state,
                                &mut swarm,
                                &mut kad_events,
                            );
                            for event in kad_events {
                                outbox.push_back(SwarmEvent::Kademlia { event });
                            }
                        }

                        // ONE CLOCK READ for the wrapper's tick and the
                        // runtime's stability sample below: read twice,
                        // the wrapper's elapsed could fall short of the
                        // runtime's by the straddle of a millisecond, and
                        // a punched connection the runtime had announced
                        // as the path would sit unpruned at the wrapper
                        // for one more tick, its close in that second a
                        // stability failure and a cooldown (PR #103 round
                        // 2). The same for the establishment arm below.
                        let now = now_ms(started);
                        // THE AUTONAT SERVER'S TICK: its rate windows and
                        // in-flight horizon read the runtime's clock.
                        autonat_server_driver::tick(swarm.autonat_server_mut(), now);
                        // THE DCUTR WRAPPER'S TICK: the attempt horizon
                        // and the cooldowns read the same clock, and the
                        // listeners this profile bound are candidates for
                        // its CONNECT (each offered once).
                        dcutr_driver::tick(swarm.dcutr_mut(), now);
                        dcutr_driver::offer_listeners(swarm.dcutr_mut(), active.values().flatten());

                        // THE STABILITY GATE AND THE RETIREMENT (step 9).
                        // A punched direct connection becomes the peer's
                        // path once it has held for the interval, which
                        // no event marks: the derivation is re-asked here
                        // for every peer holding a punched connection,
                        // and answers only when the path moved. And once
                        // a stable direct connection is the announced
                        // path -- a punched one past its interval, or a
                        // dialled or accepted one, whose completed
                        // handshake is its evidence -- the relayed
                        // connections to that peer are redundant and
                        // closed WHEN SAFE -- no exchange this profile
                        // started with the peer awaits its answer --
                        // else left for the next tick; so every peer
                        // holding a relayed connection is asked too.
                        let candidates: std::collections::BTreeSet<TransportIdentity> = open
                            .values()
                            .filter(|c| c.punched || c.path == PeerPath::Relayed)
                            .map(|c| c.peer.clone())
                            .collect();
                        for peer in candidates {
                            if let Some(event) = dialing::path_events(
                                open.values().map(|c| (&c.peer, c.sample(now, stability_ms))),
                                &mut paths,
                                &peer,
                            ) && may_buffer_delivery(outbox.len(), config.event_capacity)
                            {
                                outbox.push_back(event);
                            }
                            let awaiting = pending_direct.values().any(|p| p.peer == peer)
                                || pending_endpoints.values().any(|p| p.peer == peer);
                            let redundant = dialing::retirable(
                                open.iter().map(|(id, c)| {
                                    (*id, &c.peer, c.sample(now, stability_ms), c.retiring)
                                }),
                                &peer,
                                awaiting,
                            );
                            for id in redundant {
                                swarm.close_connection(id);
                                if let Some(connection) = open.get_mut(&id) {
                                    connection.retiring = true;
                                }
                                if may_buffer_delivery(outbox.len(), config.event_capacity) {
                                    outbox.push_back(SwarmEvent::RelayedConnectionRetired {
                                        peer: peer.clone(),
                                    });
                                }
                            }
                        }

                        // THE AUTONAT ADAPTER'S TICK: evidence expiry,
                        // the candidate set, the static servers it
                        // dials, and the re-tests that have come due.
                        // Its events are settlement-tier for the verdict
                        // (a `ConnectivityChanged` is what the consumer
                        // advertises from) and informational for the
                        // rest, pushed under the same base-capacity rule
                        // as a scheduled retry's diagnostic.
                        if let Some(state) = autonat_state.as_mut() {
                            let mut autonat_events = Vec::new();
                            autonat_driver::reconcile(
                                state,
                                &mut swarm,
                                &mut manager,
                                autonat_driver::AutonatTick {
                                    in_flight: &in_flight,
                                    open: &open,
                                    listeners: active.values().flatten().cloned().collect(),
                                    now_ms: now,
                                },
                                &mut autonat_events,
                            );
                            for event in autonat_events {
                                follow_verdict(&event, relay_state.as_mut(), &mut swarm, now, &mut outbox, config.event_capacity);
                                if matches!(event, SwarmEvent::ConnectivityChanged { .. })
                                    || may_buffer_delivery(outbox.len(), config.event_capacity)
                                {
                                    outbox.push_back(event);
                                }
                            }
                        }
                        // THE RELAY CLIENT'S TICK, after the verdict it
                        // follows: the manager asks and releases, the
                        // driver listens and withdraws, and what the
                        // Swarm advertises is brought to the manager's
                        // set. Its events are informational.
                        if let Some(state) = relay_state.as_mut() {
                            let mut relay_events = Vec::new();
                            relay_driver::reconcile(state, &mut swarm, &manager, now, &mut relay_events);
                            buffer_informational(&mut outbox, config.event_capacity, relay_events);
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
                                    relay_state.as_mut(),
                                    config.max_pending_listens,
                                    config.max_active_listeners,
                                    config.max_payload_bytes,
                                    now_ms(started),
                                    wall_ms(),
                                    &mut outbox,
                                    config.event_capacity,
                                    &mut races,
                                    head_start_ms,
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
                            // ADR-0052 rule 3 asks what this node
                            // listens on NOW, and Identify's
                            // `listen_addrs` reach the routing table
                            // through this dispatch. See
                            // `KademliaState::own_listeners`.
                            state.set_own_listeners(
                                active.values().flatten().map(ToString::to_string),
                            );
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

                        // THE AUTONAT ADAPTER SEES IT NEXT: a probe
                        // outcome is consumed here; Identify and the
                        // connection outcomes are peeked and pass on.
                        let event = if let Some(state) = autonat_state.as_mut() {
                            let mut autonat_events = Vec::new();
                            // Rule 3 at the learned-server hook asks
                            // what this node listens on NOW.
                            state.set_own_listeners(
                                active.values().flatten().map(ToString::to_string),
                            );
                            let handled = autonat_driver::handle_autonat(
                                event,
                                &mut swarm,
                                state,
                                &manager,
                                &open,
                                now_ms(started),
                                &mut autonat_events,
                            );
                            for event in autonat_events {
                                follow_verdict(&event, relay_state.as_mut(), &mut swarm, now_ms(started), &mut outbox, config.event_capacity);
                                if matches!(event, SwarmEvent::ConnectivityChanged { .. })
                                    || may_buffer_delivery(outbox.len(), config.event_capacity)
                                {
                                    outbox.push_back(event);
                                }
                            }
                            match handled {
                                autonat_driver::AutonatHandled::Consumed => continue,
                                autonat_driver::AutonatHandled::Passed(event) => *event,
                            }
                        } else {
                            event
                        };

                        // THE RELAY CLIENT'S ADAPTER SEES IT NEXT: a
                        // reservation listener's address or close, and
                        // the client's own events, are consumed here --
                        // before `translate`, which would otherwise
                        // report a reservation as an ordinary listener.
                        // Identify is peeked for a relay to learn and
                        // passes on.
                        let event = if let Some(state) = relay_state.as_mut() {
                            let mut relay_events = Vec::new();
                            // Rule 3 at the learned-relay hook asks
                            // what this node listens on NOW.
                            state.set_own_listeners(
                                active.values().flatten().map(ToString::to_string),
                            );
                            let handled = relay_driver::handle_relay(
                                event,
                                &mut swarm,
                                state,
                                &manager,
                                now_ms(started),
                                &mut relay_events,
                            );
                            buffer_informational(&mut outbox, config.event_capacity, relay_events);
                            match handled {
                                relay_driver::RelayHandled::Consumed => continue,
                                relay_driver::RelayHandled::Passed(event) => *event,
                            }
                        } else {
                            event
                        };

                        // THE SERVER'S OWN EVENTS are the wrapper's, translated
                        // and consumed here; nothing below reads them.
                        if let libp2p::swarm::SwarmEvent::Behaviour(crate::behaviour::SubstrateBehaviourEvent::AutonatServer(
                            served,
                        )) = event
                        {
                            if let Some(event) = autonat_server_driver::translate(served)
                                && may_buffer_delivery(outbox.len(), config.event_capacity)
                            {
                                outbox.push_back(event);
                            }
                            continue;
                        }
                        // THE RELAY SERVER'S EVENTS, likewise -- and who
                        // holds a reservation here, which the keepalive
                        // pings (section 14 item 5), followed from them
                        // whether or not the event is buffered.
                        if let libp2p::swarm::SwarmEvent::Behaviour(crate::behaviour::SubstrateBehaviourEvent::RelayServer(
                            served,
                        )) = event
                        {
                            if relay_reserved.follow(&served, |peer| {
                                open.values().any(|c| c.peer.as_str() == peer.to_base58())
                            })
                                && let Some(keepalive) = swarm.relay_keepalive_mut().as_mut()
                            {
                                keepalive.inner_mut().set_reserved(relay_reserved.peers());
                            }
                            if let Some(event) = relay_server_driver::translate(served)
                                && may_buffer_delivery(outbox.len(), config.event_capacity)
                            {
                                outbox.push_back(event);
                            }
                            continue;
                        }
                        // AND mDNS'S, which is the only place a multicast
                        // announcement becomes anything. The driver applies
                        // ADR-0052's boundary HERE, at the learn site, using
                        // this node's bound listeners for rule 3 -- a private
                        // candidate is admitted only beside a private listener
                        // of its family, which on a link-local multicast domain
                        // is what "on this LAN" means.
                        if let libp2p::swarm::SwarmEvent::Behaviour(
                            crate::behaviour::SubstrateBehaviourEvent::Mdns(heard),
                        ) = event
                        {
                            if let Some(state) = mdns_state.as_mut() {
                                let own: Vec<String> =
                                    active.values().flatten().map(ToString::to_string).collect();
                                deliver_mdns(
                                    state,
                                    heard,
                                    own.iter().map(String::as_str),
                                    now_ms(started),
                                    &mut outbox,
                                    config.event_capacity,
                                );
                            }
                            continue;
                        }
                        // AND THE DCUTR WRAPPER'S.
                        if let libp2p::swarm::SwarmEvent::Behaviour(crate::behaviour::SubstrateBehaviourEvent::Dcutr(
                            punch,
                        )) = event
                        {
                            if let Some(event) = dcutr_driver::translate(punch)
                                && may_buffer_delivery(outbox.len(), config.event_capacity)
                            {
                                outbox.push_back(event);
                            }
                            continue;
                        }

                        let mut refuse = Vec::new();
                        // THE ORIGIN AN INBOUND IS RETAINED UNDER, when this
                        // profile is infrastructure for it: the relay
                        // server retains every authorized inbound under
                        // `RelayReservation`, the AutoNAT server under
                        // `AutonatProbe` (and the AutoNAT client its known
                        // servers' under the same); `None` asks as before.
                        let infrastructure_origin =
                            |peer: &TransportIdentity,
                             open: &HashMap<libp2p::swarm::ConnectionId, OpenConnection>| {
                                if serving_relays {
                                    Some(DialOrigin::RelayReservation)
                                } else if serving_probes
                                    || autonat_state
                                        .as_ref()
                                        .is_some_and(|s| s.is_connected_server(peer, open))
                                {
                                    Some(DialOrigin::AutonatProbe)
                                } else {
                                    None
                                }
                            };
                        // ONE CLOCK READ for the settlement and the path
                        // events it leads to: a punched connection's
                        // `since_ms` and the wrapper's interval start are
                        // the same instant (PR #103 round 2).
                        let settled_at = now_ms(started);
                        // REBUILT PER EVENT, not held: rule 3 asks what
                        // this node listens on NOW, and a node that
                        // binds a private interface between two Identify
                        // messages must judge the second against the
                        // listeners it has then.
                        let own_listeners: Vec<String> =
                            active.values().flatten().map(ToString::to_string).collect();
                        let mut advertised = dialing::AdvertisedBoundary {
                            own_listeners: &own_listeners,
                            stores: &task_stores,
                            operator: &task_operator,
                        };
                        let announce = settle_outcome(
                            &event,
                            &mut manager,
                            &in_flight,
                            &mut open,
                            &mut refuse,
                            &infrastructure_origin,
                            &mut advertised,
                            settled_at,
                        );
                        // An inbound connection the ceiling cannot
                        // account for is closed rather than kept.
                        // Deferred out of `settle_outcome` so that
                        // function stays free of the Swarm.
                        for id in refuse {
                            swarm.close_connection(id);
                        }

                        // THE PATH EVENTS, per logical peer: a connection
                        // event names its peer, the open set says what
                        // paths remain, and the difference from the last
                        // announcement is what the consumer is told
                        // (`contracts/CONNECTIVITY.md` §5). Computed after
                        // the settlement, from the set it left.
                        let path_event = match &event {
                            libp2p::swarm::SwarmEvent::ConnectionEstablished {
                                peer_id,
                                connection_id,
                                ..
                            } => {
                                // A direct connection whose establishment
                                // ended a DCUtR attempt toward the peer is
                                // the punch (`DCUTR.md` section 7's
                                // `reason=dcutr`), whichever end dialled it
                                // -- recorded on the connection, since it
                                // becomes the peer's path only once it has
                                // held for the stability interval (step 9).
                                let now = settled_at;
                                if dcutr_driver::take_punched(swarm.dcutr_mut(), *connection_id, now)
                                    && let Some(connection) = open.get_mut(connection_id)
                                {
                                    connection.punched = true;
                                }
                                // A DIRECT DATA-PLANE CONNECTION LANDED:
                                // the race, if one waits for this peer,
                                // is won.
                                if let Some(connection) = open.get(connection_id)
                                    && connection.is_direct_data_plane()
                                {
                                    let _ = races.forget(&connection.peer);
                                }
                                to_transport_identity(peer_id).ok().and_then(|peer| {
                                    dialing::path_events(
                                        open.values().map(|c| (&c.peer, c.sample(now, stability_ms))),
                                        &mut paths,
                                        &peer,
                                    )
                                })
                            }
                            libp2p::swarm::SwarmEvent::ConnectionClosed { peer_id, .. } => {
                                let now = settled_at;
                                to_transport_identity(peer_id).ok().and_then(|peer| {
                                    dialing::path_events(
                                        open.values().map(|c| (&c.peer, c.sample(now, stability_ms))),
                                        &mut paths,
                                        &peer,
                                    )
                                })
                            }
                            _ => None,
                        };

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
                                // A LISTENER THAT JUST BOUND is a DCUtR
                                // candidate from this moment, not from
                                // the next tick: a circuit can arrive
                                // between the two, and its CONNECT would
                                // carry only what a peer had observed.
                                match &event {
                                    libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } => {
                                        dcutr_driver::offer_listeners(
                                            swarm.dcutr_mut(),
                                            std::iter::once(address),
                                        );
                                    }
                                    // And forgotten as they go, so the
                                    // offered set holds what is bound.
                                    libp2p::swarm::SwarmEvent::ExpiredListenAddr {
                                        address, ..
                                    } => {
                                        dcutr_driver::forget_listeners(
                                            swarm.dcutr_mut(),
                                            std::iter::once(address),
                                        );
                                    }
                                    libp2p::swarm::SwarmEvent::ListenerClosed {
                                        addresses, ..
                                    } => {
                                        dcutr_driver::forget_listeners(
                                            swarm.dcutr_mut(),
                                            addresses.iter(),
                                        );
                                    }
                                    _ => {}
                                }
                                let listener_event = matches!(
                                    event,
                                    libp2p::swarm::SwarmEvent::NewListenAddr { .. }
                                        | libp2p::swarm::SwarmEvent::ExpiredListenAddr { .. }
                                        | libp2p::swarm::SwarmEvent::ListenerClosed { .. }
                                );
                                let translated =
                                    translate(event, &mut listens, &mut active, &mut abandoned);
                                // A NETWORK CHANGE (section 14, step 10)
                                // is a change in the bound set, seen
                                // here, once, as the listener event that
                                // made it lands -- and told to every
                                // subsystem holding network-dependent
                                // state in the same turn, whether or not
                                // the AutoNAT client is on: its verdict
                                // to unknown (published, so the relay
                                // target follows it now), its candidates
                                // re-tested within the jitter, the DCUtR
                                // wrapper's attempts given up and its
                                // cooldowns lifted -- and every
                                // connection running from an IP the
                                // change took off this host CLOSED
                                // (item 5, the rule since 2026-09-26):
                                // as first built nothing was closed, on
                                // the belief that what died with its
                                // interface would close on its own, and
                                // SPIKE-004 phase B's `ifchange` row
                                // measured it standing for minutes. What
                                // is over an address still bound is
                                // kept. Pinned by `tests/connectivity/
                                // tests/network_change.rs` and `dcutr.rs`'s
                                // `a_network_change_lifts_the_cooldown_and_keeps_the_reservation`.
                                if listener_event
                                    && let Some(change) = network.observe(active.values().flatten())
                                {
                                    let now = now_ms(started);
                                    // ONLY A REMOVAL INVALIDATES (§14 item
                                    // 1): an address joining is reported
                                    // and offered; what was known about
                                    // the addresses still held stands.
                                    if change.invalidates()
                                        && let Some(state) = autonat_state.as_mut()
                                    {
                                        let mut autonat_events = Vec::new();
                                        autonat_driver::network_changed(state, &mut swarm, active.values().flatten(), now, &mut autonat_events);
                                        for event in autonat_events {
                                            follow_verdict(&event, relay_state.as_mut(), &mut swarm, now, &mut outbox, config.event_capacity);
                                            if matches!(event, SwarmEvent::ConnectivityChanged { .. })
                                                || may_buffer_delivery(outbox.len(), config.event_capacity)
                                            {
                                                outbox.push_back(event);
                                            }
                                        }
                                    }
                                    if change.invalidates() {
                                        dcutr_driver::network_changed(swarm.dcutr_mut());
                                        let departed = network_change::departed_ips(&change, active.values().flatten());
                                        for (id, connection) in &open {
                                            if connection.local_ip.is_some_and(|ip| departed.contains(&ip)) {
                                                // The close is a request; the
                                                // `ConnectionClosed` it raises is
                                                // what settles the record and tells
                                                // the consumer, as for any close.
                                                swarm.close_connection(*id);
                                            }
                                        }
                                    }
                                    dcutr_driver::offer_listeners(swarm.dcutr_mut(), active.values().flatten());
                                    if may_buffer_delivery(outbox.len(), config.event_capacity) {
                                        outbox.push_back(SwarmEvent::NetworkChanged {
                                            removed: change.removed,
                                            added: change.added,
                                        });
                                    }
                                }
                                translated
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
                        if let Some(SwarmEvent::Connected { peer, .. }) = path_event.as_ref()
                            && let Ok(id) = to_peer_id(peer)
                        {
                            let trusted = mesh_admits(manager.classify(peer));
                            swarm.sync_broadcast_admission(&id, trusted);
                        }
                        if let Some(event) = path_event
                            && may_buffer_delivery(outbox.len(), config.event_capacity)
                        {
                            outbox.push_back(event);
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
            refusals,
            autonat_server_counters,
            dcutr_counters,
            root_funnel_counters,
            operator,
            stores,
            mdns_drop_counts,
        })
    }

    /// This profile's PeerId.
    #[must_use]
    pub const fn local_peer(&self) -> &TransportIdentity {
        &self.local_peer
    }

    /// Dials the outbound gate refused, and why.
    ///
    /// The ONLY observation of a denied behaviour-originated dial.
    /// libp2p handles a behaviour-emitted dial as
    /// `if let Ok(()) = self.dial(opts)` and discards the denial, so
    /// there is no `Dialing` event, no `OutgoingConnectionError`, and
    /// nothing on this runtime's event stream. A caller that wants to
    /// know why Kademlia is not reaching anyone reads this.
    #[must_use]
    pub fn dial_refusals(&self) -> DialRefusals {
        self.refusals.clone()
    }

    /// What the AutoNAT server refused and served (`AUTONAT.md` §9's
    /// `autonat_server_probes_total`), or `None` when the profile
    /// serves no probes. A budget refusal reaches the event stream only
    /// while the outbox has room; this count always moves.
    #[must_use]
    pub fn autonat_server_counters(&self) -> Option<crate::probe_server::ProbeCounters> {
        self.autonat_server_counters.as_ref().map(|c| c.snapshot())
    }

    /// `DCUTR.md` §8's counters -- attempts by outcome, declines by
    /// reason, in flight, peers in cooldown -- or `None` when the
    /// profile never hole punches. An outcome reaches the event stream
    /// only while the outbox has room; this count always moves.
    #[must_use]
    pub fn dcutr_counters(&self) -> Option<crate::hole_punch::HolePunchCounters> {
        self.dcutr_counters.as_ref().map(|c| c.snapshot())
    }

    /// What the root funnel did (ADR-0052 rule 5): behaviour-contributed
    /// addresses removed by class, those that passed, and dials denied
    /// because it removed every contributed address and the dial named
    /// none of its own.
    ///
    /// The only place such a denial is visible: the Swarm discards the
    /// denial of a behaviour-originated dial (SPIKE-004), so nothing
    /// reaches this runtime's event stream for it.
    #[must_use]
    pub fn root_funnel_counters(&self) -> crate::root_funnel::RootFunnelCounters {
        self.root_funnel_counters.snapshot()
    }

    /// What ADR-0053's bounds dropped inside the mDNS crate (rule 7):
    /// evicted and refused records, dropped queue entries and packets,
    /// unanswered queries, lost failure reports. `None` when the profile
    /// runs no mDNS.
    ///
    /// What tells a flood from a quiet LAN without reading logs: nothing
    /// the bounds drop is logged with its address. (The MDNS entry of
    /// `store_refusals` rises too; the crate's INFO lines for a record it
    /// inserts or expires name the peer and, since ADR-0053 rule 8, no
    /// address.) The counts of every behaviour a rebuild replaced are
    /// kept, so they only grow (`mdns_driver::DropCountsCell`, and the
    /// namespace test its doc names).
    #[must_use]
    pub fn mdns_drop_counts(&self) -> Option<mdns_driver::MdnsDropCounts> {
        self.mdns_drop_counts
            .as_ref()
            .map(mdns_driver::DropCountsCell::read)
    }

    /// What each store's learn site admitted and refused, by class
    /// (ADR-0052 rule 8), keyed by `store_refusals::store` names.
    ///
    /// The only trace a refusal leaves: rule 5 keeps the refused address
    /// out of every log, and a store that refuses everything and a peer
    /// that advertises nothing would otherwise look the same.
    #[must_use]
    pub fn store_refusals(
        &self,
    ) -> std::collections::BTreeMap<&'static str, crate::store_refusals::StoreCounts> {
        self.stores.snapshot()
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
    use super::{
        SwarmEvent, deliver_mdns, flush_held_mdns, may_buffer_delivery, mdns_driver::MdnsState,
        mdns_or_degraded, mdns_refresh_timer, mdns_tick, polling_room, refresh_mdns,
    };
    use std::collections::{BTreeSet, VecDeque};

    /// #111 DNS review P2-3: an unreadable resolver configuration is a
    /// degraded node, not a refusal to start -- an empty configuration
    /// that BUILDS a DNS transport, and the event naming why. A readable
    /// one is the control: passed through, no event.
    #[tokio::test]
    async fn a_missing_resolver_configuration_degrades_to_an_empty_one() {
        let (config, opts, event) =
            super::resolver_or_empty::<&str>(Err("no nameservers found in config"));
        assert!(config.name_servers().is_empty());
        assert!(matches!(
            event,
            Some(SwarmEvent::ResolverUnavailable { ref detail }) if detail.contains("nameservers")
        ));
        let _transport = libp2p::dns::tokio::Transport::custom(
            libp2p::core::transport::MemoryTransport::default(),
            config,
            opts,
        );

        let readable = libp2p::dns::ResolverConfig::from_parts(None, Vec::new(), Vec::new());
        let (_, _, none) =
            super::resolver_or_empty::<&str>(Ok((readable, libp2p::dns::ResolverOpts::default())));
        assert!(none.is_none(), "a readable configuration reports nothing");
    }

    /// The invariant itself, on a real runtime: a failed resolver read
    /// starts the node, and its first event names why. The same start
    /// with a readable configuration is the control -- nothing reported.
    #[tokio::test]
    async fn a_runtime_whose_resolver_read_fails_starts_and_says_so() {
        use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
        let trust = || {
            interweave_transport_runtime::TrustSources::new(
                PeerTrustPolicy::new(std::iter::empty()).expect("empty"),
                InfrastructureSet::default(),
            )
        };
        let identity = interweave_profile_identity::ProfileIdentity::generate();

        let mut degraded = super::SwarmRuntime::start_with_resolver::<&str>(
            &identity,
            super::SubstrateConfig::default(),
            trust(),
            Err("no nameservers found in config"),
        )
        .expect("a node with no resolver configuration still starts");
        let first = tokio::time::timeout(std::time::Duration::from_secs(5), degraded.next_event())
            .await
            .expect("the reason arrives at once");
        assert!(
            matches!(first, Some(SwarmEvent::ResolverUnavailable { .. })),
            "and says why first: {first:?}"
        );
        degraded.shutdown().await.expect("clean shutdown");

        let mut healthy = super::SwarmRuntime::start_with_resolver::<&str>(
            &identity,
            super::SubstrateConfig::default(),
            trust(),
            Ok((
                libp2p::dns::ResolverConfig::from_parts(None, Vec::new(), Vec::new()),
                libp2p::dns::ResolverOpts::default(),
            )),
        )
        .expect("starts");
        let quiet =
            tokio::time::timeout(std::time::Duration::from_millis(300), healthy.next_event()).await;
        assert!(
            !matches!(quiet, Ok(Some(SwarmEvent::ResolverUnavailable { .. }))),
            "the control: a readable configuration reports nothing: {quiet:?}"
        );
        healthy.shutdown().await.expect("clean shutdown");
    }

    /// ADR-0053 rule 5 under backpressure (#112 blind review F4): failures
    /// the outbox could not take are held one per interface, the latest
    /// reason winning -- bounded by this node's own interfaces -- and flushed
    /// one per free slot. THE CONTROL is the second interface: a hold that
    /// kept only one failure overall would lose it.
    #[test]
    fn held_mdns_failures_are_one_per_interface_and_flush_a_slot_at_a_time() {
        let a: std::net::IpAddr = "10.99.0.1".parse().expect("ip");
        let b: std::net::IpAddr = "10.99.0.2".parse().expect("ip");
        let mut state = MdnsState::new();
        state.hold_failure(a, "first".to_owned());
        state.hold_failure(a, "latest".to_owned());
        state.hold_failure(b, "other".to_owned());
        let mut outbox = VecDeque::new();
        let mut delivered = Vec::new();
        for _ in 0..3 {
            flush_held_mdns(&mut state, &mut outbox, 1, 0);
            assert!(outbox.len() <= 1, "one slot, one event");
            if let Some(SwarmEvent::MdnsInterfaceFailed { address, detail }) = outbox.pop_front() {
                delivered.push((address, detail));
            }
        }
        assert_eq!(
            delivered,
            vec![(a, "latest".to_owned()), (b, "other".to_owned())],
            "one per interface, the latest reason, each delivered"
        );
        assert!(!state.holds_anything(), "nothing left behind");
    }

    /// ADR-0053 rule 5's watcher failure under backpressure: held as ONE,
    /// the latest reason winning, and delivered when a slot frees. The
    /// crate emits it once until the watcher recovers; this is the
    /// driver's half of that bound.
    #[test]
    fn a_held_watcher_failure_is_one_and_the_latest() {
        let mut state = MdnsState::new();
        state.hold_watcher_failure("first".to_owned());
        state.hold_watcher_failure("latest".to_owned());
        let mut outbox = VecDeque::new();
        flush_held_mdns(&mut state, &mut outbox, 1, 0);
        assert!(matches!(
            outbox.pop_front(),
            Some(SwarmEvent::MdnsWatcherFailed { ref detail }) if detail == "latest"
        ));
        flush_held_mdns(&mut state, &mut outbox, 1, 0);
        assert!(outbox.is_empty(), "only one was held");
        assert!(!state.holds_anything());
    }

    /// #111 mDNS review F3: at an `event_capacity` of one, held changes
    /// still get out -- one per free slot, the discovery and then the
    /// retraction as the consumer drains. A full outbox takes nothing,
    /// which is the control: the flush respects the bound.
    #[test]
    fn held_mdns_changes_flush_one_slot_at_a_time() {
        let identity = || {
            let key = libp2p::identity::Keypair::generate_ed25519();
            super::to_transport_identity(&key.public().to_peer_id()).expect("canonical")
        };
        let (a, b) = (identity(), identity());
        let mut state = MdnsState::new();
        state.hold_discovered(vec![interweave_discovery_api::CandidatePeer {
            peer_id: a,
            addresses: BTreeSet::from(["/ip4/8.8.8.8/tcp/1".to_owned()]),
            source: "mdns".to_owned(),
            observed_at: 0,
            expires_at: None,
            protocol_observations: BTreeSet::new(),
        }]);
        state.hold_expired(vec![(b, "/ip4/1.1.1.1/tcp/1".to_owned())]);
        let mut outbox = VecDeque::new();

        outbox.push_back(SwarmEvent::MdnsUnavailable {
            detail: "occupying the one slot".to_owned(),
        });
        flush_held_mdns(&mut state, &mut outbox, 1, 0);
        assert_eq!(outbox.len(), 1, "a full outbox takes nothing");
        assert!(state.holds_anything());

        let _ = outbox.pop_front();
        flush_held_mdns(&mut state, &mut outbox, 1, 0);
        assert!(
            matches!(outbox.pop_front(), Some(SwarmEvent::MdnsExpired { .. })),
            "one free slot delivers the held retraction FIRST: a consumer at \
             capacity needs the room before the discovery (#112)"
        );
        assert!(outbox.is_empty());
        flush_held_mdns(&mut state, &mut outbox, 1, 0);
        assert!(
            matches!(outbox.pop_front(), Some(SwarmEvent::MdnsDiscovered { .. })),
            "and the next free slot the held discovery"
        );
        assert!(!state.holds_anything(), "nothing is left behind");
    }

    /// #112 blind review N7: each of the four mDNS events is delivered
    /// when the outbox has room and nothing is held, and HELD otherwise --
    /// behind a full outbox, and behind an older hold even with room, so
    /// it cannot overtake it. THE CONTROL is the first delivery of each:
    /// a `deliver_mdns` that held everything would fail it.
    #[test]
    fn every_mdns_event_is_held_behind_a_full_outbox_or_an_older_hold() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let address: libp2p::Multiaddr = "/ip4/10.99.0.2/tcp/4001".parse().expect("multiaddr");
        let iface: std::net::IpAddr = "10.99.0.1".parse().expect("ip");
        let events = || {
            vec![
                libp2p::mdns::Event::Discovered(vec![(peer, address.clone())]),
                libp2p::mdns::Event::Expired(vec![(peer, address.clone())]),
                libp2p::mdns::Event::InterfaceFailed {
                    address: iface,
                    reason: "send".to_owned(),
                },
                libp2p::mdns::Event::WatcherFailed {
                    reason: "watch".to_owned(),
                },
            ]
        };
        let own = ["/ip4/10.99.0.1/tcp/4001"];

        for event in events() {
            let name = format!("{event:?}");
            let mut state = MdnsState::new();
            let mut outbox = VecDeque::new();
            deliver_mdns(&mut state, event, own, 0, &mut outbox, 1);
            assert_eq!(outbox.len(), 1, "room and no hold: delivered ({name})");
            assert!(!state.holds_anything(), "and nothing held ({name})");
        }

        for event in events() {
            let name = format!("{event:?}");
            let mut state = MdnsState::new();
            let mut outbox = VecDeque::new();
            outbox.push_back(SwarmEvent::MdnsUnavailable {
                detail: "occupying the one slot".to_owned(),
            });
            deliver_mdns(&mut state, event, own, 0, &mut outbox, 1);
            assert_eq!(outbox.len(), 1, "a full outbox takes nothing ({name})");
            assert!(
                state.holds_anything(),
                "the event is held, not dropped ({name})"
            );
        }

        for event in events() {
            let name = format!("{event:?}");
            let mut state = MdnsState::new();
            state.hold_watcher_failure("older".to_owned());
            let mut outbox = VecDeque::new();
            deliver_mdns(&mut state, event, own, 0, &mut outbox, 8);
            assert!(
                outbox.is_empty(),
                "room, but an older hold: held behind it ({name})"
            );
        }
    }

    /// ADR-0053 rule 10, at the runtime's function: every record the
    /// crate holds whose expiry is still ahead goes out as one
    /// `MdnsDiscovered`, stamped now; a record at or past its expiry is
    /// left for the crate's own `Expired`; the boundary runs again, so a
    /// private pair whose private listener is gone is not refreshed; and a
    /// refresh tallies nothing as admitted or refused (the undercount that
    /// costs is `on_refresh`'s to state). THE CONTROL for the
    /// listener half is the same record beside a private listener.
    #[test]
    fn the_refresh_repushes_what_the_crate_holds_through_the_boundary() {
        let peer = || {
            libp2p::identity::Keypair::generate_ed25519()
                .public()
                .to_peer_id()
        };
        let (live, lapsed) = (peer(), peer());
        let address: libp2p::Multiaddr = "/ip4/192.168.1.5/tcp/4001".parse().expect("multiaddr");
        let at = std::time::Instant::now();
        let records = || {
            vec![
                (
                    live,
                    address.clone(),
                    at + std::time::Duration::from_secs(30),
                ),
                (lapsed, address.clone(), at),
            ]
        };

        let mut state = MdnsState::new();
        let mut outbox = VecDeque::new();
        refresh_mdns(
            &mut state,
            records(),
            at,
            ["/ip4/192.168.1.20/tcp/4001"],
            61_000,
            &mut outbox,
            8,
        );
        match outbox.pop_front() {
            Some(SwarmEvent::MdnsDiscovered { candidates }) => {
                let peers: Vec<String> = candidates
                    .iter()
                    .map(|c| c.peer_id.as_str().to_owned())
                    .collect();
                assert_eq!(peers, vec![live.to_string()], "the live record, alone");
                assert_eq!(candidates[0].observed_at, 61_000, "stamped now");
            }
            other => panic!("expected one MdnsDiscovered, got {other:?}"),
        }
        assert!(outbox.is_empty());
        let counts = state.counters();
        assert_eq!(
            (counts.admitted, counts.refused_total()),
            (0, 0),
            "a refresh tallies nothing (mdns_driver's `on_refresh` states the undercount)"
        );

        let mut outbox = VecDeque::new();
        refresh_mdns(
            &mut state,
            records(),
            at,
            std::iter::empty::<&str>(),
            61_000,
            &mut outbox,
            8,
        );
        assert!(
            outbox.is_empty(),
            "with no private listener left, the private pair is not refreshed"
        );
        assert_eq!(state.counters().refused_total(), 0, "and not tallied");
    }

    /// A scripted interface watcher: it yields `script` in order, then
    /// waits forever. No interface ever comes up, so a behaviour built on
    /// it opens no socket and sends nothing on the host's network.
    #[derive(Debug)]
    struct Scripted(std::collections::VecDeque<std::io::Result<if_watch::IfEvent>>);

    impl futures::Stream for Scripted {
        type Item = std::io::Result<if_watch::IfEvent>;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            self.0
                .pop_front()
                .map_or(std::task::Poll::Pending, |event| {
                    std::task::Poll::Ready(Some(event))
                })
        }
    }

    thread_local! {
        /// What the next `Quiet` watcher yields.
        static SCRIPT: std::cell::RefCell<Vec<std::io::Result<if_watch::IfEvent>>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// The tokio runtime with a `Scripted` watcher.
    enum Quiet {}

    impl libp2p::mdns::Provider for Quiet {
        type Socket = <libp2p::mdns::tokio::Tokio as libp2p::mdns::Provider>::Socket;
        type Timer = <libp2p::mdns::tokio::Tokio as libp2p::mdns::Provider>::Timer;
        type Watcher = Scripted;
        type TaskHandle = <libp2p::mdns::tokio::Tokio as libp2p::mdns::Provider>::TaskHandle;

        fn new_watcher() -> Result<Self::Watcher, std::io::Error> {
            Ok(Scripted(
                SCRIPT.with(|s| s.borrow_mut().drain(..).collect()),
            ))
        }

        fn spawn(task: impl std::future::Future<Output = ()> + Send + 'static) -> Self::TaskHandle {
            <libp2p::mdns::tokio::Tokio as libp2p::mdns::Provider>::spawn(task)
        }
    }

    type QuietField = libp2p::swarm::behaviour::toggle::Toggle<
        crate::mdns_scope::MdnsScope<libp2p::mdns::Behaviour<Quiet>>,
    >;

    /// A behaviour whose watcher yields `script` and then waits.
    fn quiet(
        script: Vec<std::io::Result<if_watch::IfEvent>>,
    ) -> crate::mdns_scope::MdnsScope<libp2p::mdns::Behaviour<Quiet>> {
        SCRIPT.with(|s| *s.borrow_mut() = script);
        let config = libp2p::mdns::Config {
            ttl: std::time::Duration::from_secs(360),
            query_interval: std::time::Duration::from_secs(90),
            enable_ipv6: false,
        };
        crate::mdns_scope::MdnsScope::new(
            libp2p::mdns::Behaviour::new(
                config,
                libp2p::identity::Keypair::generate_ed25519()
                    .public()
                    .to_peer_id(),
            )
            .expect("a scripted watcher"),
        )
    }

    fn running(field: &QuietField) -> std::sync::Arc<libp2p::mdns::DropCounts> {
        field.as_ref().expect("configured").inner().drop_counts()
    }

    /// ADR-0053 rule 5, the wiring's first half: delivering a
    /// `WatcherFailed` makes a rebuild due, and no other event does --
    /// whether the report goes out now or is held.
    #[test]
    fn a_watcher_failure_and_nothing_else_makes_a_rebuild_due() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let address: libp2p::Multiaddr = "/ip4/8.8.8.8/tcp/4001".parse().expect("multiaddr");
        let others = [
            libp2p::mdns::Event::Discovered(vec![(peer, address.clone())]),
            libp2p::mdns::Event::Expired(vec![(peer, address)]),
            libp2p::mdns::Event::InterfaceFailed {
                address: "10.99.0.1".parse().expect("ip"),
                reason: "send".to_owned(),
            },
        ];
        for event in others {
            let mut state = MdnsState::new();
            deliver_mdns(
                &mut state,
                event,
                [] as [&str; 0],
                0,
                &mut VecDeque::new(),
                8,
            );
            assert!(!state.rebuild_due());
        }
        for capacity in [8, 0] {
            let mut state = MdnsState::new();
            deliver_mdns(
                &mut state,
                libp2p::mdns::Event::WatcherFailed {
                    reason: "netlink".to_owned(),
                },
                [] as [&str; 0],
                0,
                &mut VecDeque::new(),
                capacity,
            );
            assert!(state.rebuild_due(), "at capacity {capacity}");
        }
    }

    /// A tick with no rebuild due builds nothing and keeps the running
    /// behaviour: the rebuild is only ever the answer to a failure.
    #[tokio::test]
    async fn a_tick_with_no_rebuild_due_builds_nothing() {
        let mut field: QuietField =
            libp2p::swarm::behaviour::toggle::Toggle::from(Some(quiet(vec![])));
        let before = running(&field);
        let cell = super::mdns_driver::DropCountsCell::new(before.clone());
        let mut state = MdnsState::new();
        let mut outbox = VecDeque::new();
        mdns_tick(
            &mut state,
            &mut field,
            || panic!("built with no rebuild due"),
            &[],
            &cell,
            std::time::Instant::now(),
            0,
            &mut outbox,
            8,
        );
        assert!(std::sync::Arc::ptr_eq(&running(&field), &before));
        assert!(outbox.is_empty());
    }

    /// A due tick rebuilds once: the fresh behaviour runs, the rebuild is
    /// no longer due, the handle's counts follow the fresh behaviour, and
    /// what the replaced one had not delivered -- here its own
    /// `WatcherFailed`, pending in it -- is delivered, not dropped with it.
    /// A second tick builds nothing.
    #[tokio::test]
    async fn a_due_tick_rebuilds_once_and_delivers_what_the_replaced_behaviour_held() {
        let failing = || Err(std::io::Error::other("netlink ended"));
        let mut field: QuietField =
            libp2p::swarm::behaviour::toggle::Toggle::from(Some(quiet(vec![failing(), failing()])));
        let cell = super::mdns_driver::DropCountsCell::new(running(&field));
        let mut state = MdnsState::new();
        state.want_rebuild();
        let fresh = quiet(vec![]);
        let fresh_counts = fresh.inner().drop_counts();
        let mut outbox = VecDeque::new();
        mdns_tick(
            &mut state,
            &mut field,
            || Ok(fresh),
            &[],
            &cell,
            std::time::Instant::now(),
            0,
            &mut outbox,
            8,
        );
        assert!(
            std::sync::Arc::ptr_eq(&running(&field), &fresh_counts),
            "the fresh one runs"
        );
        assert!(!state.rebuild_due(), "and the rebuild is done");
        assert!(
            cell.reads(&fresh_counts),
            "the handle's counts follow the fresh behaviour"
        );
        assert!(
            matches!(
                outbox.pop_front(),
                Some(SwarmEvent::MdnsWatcherFailed { .. })
            ),
            "the replaced behaviour's pending report was drained and delivered"
        );
        assert!(outbox.is_empty());

        mdns_tick(
            &mut state,
            &mut field,
            || panic!("rebuilt twice"),
            &[],
            &cell,
            std::time::Instant::now(),
            0,
            &mut outbox,
            8,
        );
    }

    /// A due tick whose watcher cannot be built keeps the running
    /// behaviour -- it still serves the interfaces it has -- stays due,
    /// and reports `MdnsRebuildFailed`, not `MdnsUnavailable`. Behind a
    /// full outbox the report is HELD and goes out on the next flush.
    #[tokio::test]
    async fn a_rebuild_that_cannot_build_keeps_the_running_behaviour_and_reports_it() {
        let mut field: QuietField =
            libp2p::swarm::behaviour::toggle::Toggle::from(Some(quiet(vec![])));
        let before = running(&field);
        let cell = super::mdns_driver::DropCountsCell::new(before.clone());
        for capacity in [8, 0] {
            let mut state = MdnsState::new();
            state.want_rebuild();
            let mut outbox = VecDeque::new();
            mdns_tick(
                &mut state,
                &mut field,
                || Err(std::io::Error::other("no netlink")),
                &[],
                &cell,
                std::time::Instant::now(),
                0,
                &mut outbox,
                capacity,
            );
            assert!(
                std::sync::Arc::ptr_eq(&running(&field), &before),
                "kept, at {capacity}"
            );
            assert!(state.rebuild_due(), "still due, at {capacity}");
            if capacity == 0 {
                assert!(outbox.is_empty() && state.holds_anything(), "held");
                flush_held_mdns(&mut state, &mut outbox, 1, 0);
            }
            assert!(
                matches!(
                    outbox.pop_front(),
                    Some(SwarmEvent::MdnsRebuildFailed { ref detail }) if detail == "no netlink"
                ),
                "reported, at {capacity}"
            );
        }
    }

    /// A rebuild failure still held when a later rebuild succeeds is
    /// dropped, not delivered after the success; one held while none has
    /// succeeded is kept, the control.
    #[test]
    fn a_successful_rebuild_drops_a_held_report_of_an_earlier_failure() {
        let mut state = MdnsState::new();
        state.hold_rebuild_failure("an earlier tick's".to_owned());
        assert!(state.holds_anything(), "the control: held");
        state.rebuilt();
        assert_eq!(state.take_held_rebuild_failure(), None);
        assert!(!state.holds_anything());
    }

    /// The refresh timer's period, advanced on a paused clock: the first
    /// tick one `REFRESH_INTERVAL` after start, the next one later.
    #[tokio::test(start_paused = true)]
    async fn the_mdns_refresh_timer_fires_every_refresh_interval() {
        let start = tokio::time::Instant::now();
        let mut timer = mdns_refresh_timer();
        timer.tick().await;
        assert_eq!(start.elapsed(), super::mdns_driver::REFRESH_INTERVAL);
        timer.tick().await;
        assert_eq!(start.elapsed(), 2 * super::mdns_driver::REFRESH_INTERVAL);
    }

    /// The cell's sum: what a replaced behaviour counted is kept, field by
    /// field, and a sum at the top saturates rather than wrapping.
    #[test]
    fn drop_counts_add_field_by_field_and_saturate() {
        let one = super::mdns_driver::MdnsDropCounts {
            records_evicted: 1,
            records_refused: 2,
            discovered_dropped: 3,
            packets_dropped: 4,
            queries_unanswered: 5,
            failures_dropped: 6,
        };
        let sum = one.plus(one);
        assert_eq!(
            (
                sum.records_evicted,
                sum.records_refused,
                sum.discovered_dropped,
                sum.packets_dropped,
                sum.queries_unanswered,
                sum.failures_dropped
            ),
            (2, 4, 6, 8, 10, 12)
        );
        let top = super::mdns_driver::MdnsDropCounts {
            queries_unanswered: u64::MAX,
            ..one
        };
        assert_eq!(top.plus(one).queries_unanswered, u64::MAX);
    }

    /// A refresh behind a full outbox is held like a discovery, and goes
    /// out when there is room -- neither dropped nor overtaking.
    #[test]
    fn a_refresh_behind_a_full_outbox_is_held_and_then_delivered() {
        let live = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let address: libp2p::Multiaddr = "/ip4/8.8.8.8/tcp/4001".parse().expect("multiaddr");
        let at = std::time::Instant::now();
        let mut state = MdnsState::new();
        let mut outbox = VecDeque::new();
        outbox.push_back(SwarmEvent::MdnsUnavailable {
            detail: "occupying the one slot".to_owned(),
        });
        refresh_mdns(
            &mut state,
            [(live, address, at + std::time::Duration::from_secs(30))],
            at,
            std::iter::empty::<&str>(),
            0,
            &mut outbox,
            1,
        );
        assert_eq!(outbox.len(), 1, "a full outbox takes nothing");
        assert!(state.holds_discovered(), "the refresh is held");
        outbox.clear();
        flush_held_mdns(&mut state, &mut outbox, 1, 5);
        assert!(matches!(
            outbox.pop_front(),
            Some(SwarmEvent::MdnsDiscovered { .. })
        ));
    }

    /// `providers/mdns.md` §Failure, as a test rather than a citation.
    ///
    /// The claim is that a failed INTERFACE WATCHER -- the one mDNS
    /// environment failure that surfaces at construction; the
    /// per-interface ones arrive as `MdnsInterfaceFailed`, pinned in
    /// `tests/mdns_bounds.rs` -- leaves the node
    /// running without the provider AND reports it. This pins the
    /// mapping: the failure yields no behaviour, no state, and the
    /// `MdnsUnavailable` event carrying the OS's message. It does NOT pin
    /// the call site in `start`: a `?` could come back there
    /// (`.transpose()?`) and this would still pass. An earlier version of
    /// this doc claimed otherwise (#111 re-review P2-6).
    #[test]
    fn an_mdns_construction_failure_degrades_the_provider_and_names_why() {
        let (behaviour, state, unavailable) = mdns_or_degraded::<()>(Some(Err(
            std::io::Error::other("failed to create the interface watcher"),
        )));

        assert!(
            behaviour.is_none() && state.is_none(),
            "a provider that could not be built must not be half-present"
        );
        let Some(SwarmEvent::MdnsUnavailable { detail }) = unavailable else {
            panic!(
                "a profile that asked for LAN discovery and did not get it must be told -- a \
                 silent degrade is a provider that looks configured and never announces; \
                 got {unavailable:?}"
            );
        };
        assert!(
            detail.contains("interface watcher"),
            "the operating system's own message is the only thing that distinguishes \
             this cause from the next one this arm acquires. Got: {detail}"
        );
    }

    /// The two arms that are NOT a degrade, so the one above cannot pass
    /// by the function having become a constant.
    #[test]
    fn mdns_is_built_when_asked_for_and_absent_when_not() {
        let (behaviour, state, unavailable) = mdns_or_degraded(Some(Ok(())));
        assert!(behaviour.is_some(), "a provider that built must be present");
        assert!(state.is_some(), "its driver state travels with it");
        assert!(unavailable.is_none(), "nothing to report when it worked");

        let (behaviour, state, unavailable) = mdns_or_degraded::<()>(None);
        assert!(
            behaviour.is_none() && state.is_none(),
            "a profile that did not ask for LAN discovery gets no provider"
        );
        assert!(
            unavailable.is_none(),
            "and is told nothing, because nothing failed -- reporting here would \
             make every profile look like a degraded one"
        );
    }

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
            polling_room(buffered, event_capacity, listens, 0, 0, 0, 0),
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
            !polling_room(buffered + 1, event_capacity, listens, 0, 0, 0, 0),
            "which is precisely the state where the listener can never resolve"
        );
    }

    /// The runtime's half of the backlog bound (#117's blind re-review,
    /// round 3, F1): the flag goes up at a full call's worth of undelivered
    /// transactions, and down again below it.
    #[test]
    fn the_backlog_flag_follows_the_outbox() {
        use super::kademlia_driver::{
            KademliaSettings, KademliaState, MAX_QUERY_TRANSACTION_EVENTS,
        };
        use interweave_kademlia_control_api::KademliaMode;
        use std::num::NonZeroUsize;
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: std::time::Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let settlement = |n: u64| SwarmEvent::Kademlia {
            event: interweave_kademlia_control_api::KademliaEvent::QueryFailed {
                handle: interweave_kademlia_control_api::QueryHandle::commanded(n),
                class: interweave_kademlia_control_api::QueryClass::Targeted,
                reason: interweave_kademlia_control_api::QueryFailure::NoRoutingPeers,
            },
        };
        let mut outbox = VecDeque::new();
        for n in 0..(MAX_QUERY_TRANSACTION_EVENTS - 1) {
            outbox.push_back(settlement(n as u64));
        }
        assert_eq!(
            super::mark_query_backlog(Some(&mut state), &outbox),
            MAX_QUERY_TRANSACTION_EVENTS - 1
        );
        assert!(!state.is_backlogged(), "one short: still tracking");
        outbox.push_back(settlement(999));
        let _ = super::mark_query_backlog(Some(&mut state), &outbox);
        assert!(state.is_backlogged(), "a full call's worth: backlogged");
        let _ = outbox.pop_front();
        let _ = super::mark_query_backlog(Some(&mut state), &outbox);
        assert!(!state.is_backlogged(), "and cleared as the consumer drains");
    }

    /// Review R1 on fa3eab8: a query settlement buffered for a query that
    /// is no longer outstanding -- an immediate refusal -- took the slot
    /// a direct exchange had earned, and at capacity 1 the Swarm stopped
    /// being polled (`2 < 1 + 1`). A buffered transaction is its own
    /// allowance. THE CONTROL is the same state without it.
    #[test]
    fn a_buffered_query_settlement_does_not_spend_an_exchanges_slot() {
        assert!(
            !polling_room(2, 1, 0, 1, 0, 0, 0),
            "two notifications and one exchange: the exchange's slot is spent"
        );
        assert!(
            polling_room(2, 1, 0, 1, 0, 0, 1),
            "one notification, one settlement and one exchange: still polled"
        );
        // AND NOT SELF-FINANCING (#117's blind review, F1): a buffered
        // transaction buys no room -- the notifications alone are judged
        // -- and NEVER A STOP (its re-review, F1): however many are
        // buffered, a caller waiting on Swarm progress keeps it polled.
        // Their bound is the driver's (`set_backlogged`).
        let call = super::kademlia_driver::MAX_QUERY_TRANSACTION_EVENTS;
        assert!(
            !polling_room(call + 2, 1, 0, 0, 0, 0, call),
            "two notifications at capacity one, and nobody waiting: stopped"
        );
        assert!(
            polling_room(call + 1, 1, 0, 1, 0, 0, call),
            "a full call's worth of transactions does not stop a pending exchange"
        );
        assert!(
            polling_room(4 * call + 1, 1, 0, 1, 0, 0, 4 * call),
            "nor any number of them"
        );
        let mut outbox = VecDeque::new();
        outbox.push_back(SwarmEvent::MdnsUnavailable {
            detail: "a notification".to_owned(),
        });
        outbox.push_back(SwarmEvent::Kademlia {
            event: interweave_kademlia_control_api::KademliaEvent::QueryFailed {
                handle: interweave_kademlia_control_api::QueryHandle::commanded(1),
                class: interweave_kademlia_control_api::QueryClass::Targeted,
                reason: interweave_kademlia_control_api::QueryFailure::NoRoutingPeers,
            },
        });
        assert_eq!(
            super::buffered_query_transactions(&outbox),
            1,
            "the settlement is counted, the notification is not"
        );
    }

    /// The bound still bounds: with nothing in flight, the base capacity
    /// is the whole allowance.
    #[test]
    fn a_stalled_consumer_with_nothing_in_flight_stops_polling() {
        assert!(polling_room(0, 1, 0, 0, 0, 0, 0));
        assert!(!polling_room(1, 1, 0, 0, 0, 0, 0));
    }

    /// In-flight exchanges buy room, because polling is what settles
    /// them. Without this `send_direct` waits past its own deadline for
    /// a response nothing will ever process.
    #[test]
    fn in_flight_exchanges_keep_polling_alive() {
        assert!(
            polling_room(1, 1, 0, 1, 0, 0, 0),
            "one exchange in flight, one event buffered: still polling"
        );
        assert!(
            !polling_room(2, 1, 0, 1, 0, 0, 0),
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
        assert!(polling_room(1, 1, 0, 1, 0, 0, 0));
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
            polling_room(1, 1, 0, 0, 1, 0, 0),
            "nothing else in flight, but an answer is waiting to be written"
        );
        assert!(
            !polling_room(2, 1, 0, 0, 1, 0, 0),
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
        assert!(polling_room(1, 1, 1, 0, 0, 0, 0));
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
