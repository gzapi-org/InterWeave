// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Turning a `SwarmCommand` into Swarm work, and a Swarm event into a
//! `SwarmEvent`.
//!
//! Split out of `runtime.rs` unchanged. Both directions are here on
//! purpose: the command side and the translation side are the two halves
//! of one boundary, and a change to what a command means almost always
//! implies a change to what the caller is told happened.

use std::collections::{BTreeSet, HashMap};

use libp2p::Multiaddr;
use libp2p::core::transport::ListenerId;
use libp2p::identify;
use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;

use interweave_transport_api::TransportError as DirectError;
use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::endpoint_registry::LocalSessionId;
use interweave_transport_runtime::{ConnectionClass, ConnectionManager, DialOrigin, DialTicket};

use crate::behaviour::SubstrateBehaviourEvent;
use crate::gated_swarm::{GatedSwarm, NotConnected, mesh_admits};

use super::dialing::{
    ActiveListeners, OpenConnection, PendingListens, attempt_dial, connections_to_close,
};
use super::messages::{DialRefusal, SwarmCommand, SwarmEvent};

// Still beside the loop that owns them; step 5 moves these out.
use super::direct::DirectState;
use super::{PendingDirect, admit_outbound, to_peer_id, to_transport_identity};

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_command(
    swarm: &mut GatedSwarm,
    manager: &mut ConnectionManager,
    open: &HashMap<libp2p::swarm::ConnectionId, OpenConnection>,
    refuse: &mut Vec<libp2p::swarm::ConnectionId>,
    listens: &mut PendingListens,
    active: &mut ActiveListeners,
    pending_direct: &mut HashMap<libp2p::request_response::OutboundRequestId, PendingDirect>,
    direct_state: &mut DirectState,
    broadcast_state: &mut super::broadcast::BroadcastState,
    in_flight: &mut HashMap<libp2p::swarm::ConnectionId, DialTicket>,
    max_pending_listens: usize,
    max_active_listeners: usize,
    effective_payload: usize,
    now_ms: u64,
    wall_ms: u64,
    outbox: &mut std::collections::VecDeque<SwarmEvent>,
    event_capacity: usize,
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
            // AND THE BOUND THAT SURVIVES BINDING. The check above counts
            // only listeners still awaiting an address; a resolved one
            // leaves that table, so binding four in sequence under a
            // pending bound of two used to succeed and leave four sockets
            // open. Pending and active are counted together because both
            // hold an OS listener.
            if listens.len().saturating_add(active.len()) >= max_active_listeners {
                let _ = reply.send(Err(format!(
                    "at most {max_active_listeners} listeners may be bound at once"
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
        SwarmCommand::StopListening { address, reply } => {
            // Named by an address rather than an id: `listen` hands back
            // an address and nothing else, so that is the only handle a
            // caller holds.
            let found = active
                .iter()
                .find(|(_, addrs)| addrs.contains(&address))
                .map(|(id, _)| *id);
            match found {
                Some(id) => {
                    // The table entry is left for `ListenerClosed` to
                    // remove, so the withdrawal is reported by the same
                    // path whether the close was asked for or not.
                    let removed = swarm.remove_listener(id);
                    let _ = reply.send(removed);
                }
                None => {
                    let _ = reply.send(false);
                }
            }
        }
        SwarmCommand::ConfigureBroadcast { config, reply } => {
            // The desired set is replaced; live joins are kept. See the
            // command's own doc for why this differs from ConfigureDirect.
            let desired: std::collections::BTreeSet<_> = config.desired.iter().cloned().collect();
            match broadcast_state.subs.set_desired(desired) {
                Ok(()) => {
                    // THE BOUND MOVES ONLY ON SUCCESS. Assigning it above
                    // the match left a refused configuration partly
                    // applied: the caller was told the configuration
                    // failed while every later session silently opened at
                    // the bound from the rejected request.
                    broadcast_state.queue_bound = config.queue_bound;
                    // AND FOR THE SESSIONS ALREADY OPEN. Setting it for
                    // future queues alone left live sessions on whatever
                    // was configured when they joined, so one bound meant
                    // two things depending on join order.
                    broadcast_state.queues.set_bound(config.queue_bound);
                    // SUBSCRIBE AFTER the registry accepted the set, so a
                    // refused configuration leaves the mesh untouched
                    // rather than half-applied.
                    for channel in &config.desired {
                        let topic = broadcast_state.remember(channel);
                        let _ = swarm.subscribe_topic(&topic);
                    }

                    // AND DROP WHAT IS NO LONGER HELD. Subscribing to the
                    // new set is only half of applying it: a channel the
                    // previous set desired, that no session still joins,
                    // is held by nobody once the registry has answered --
                    // and would otherwise stay subscribed, receiving and
                    // relaying traffic this node no longer wants, with
                    // each reconfiguration adding another.
                    //
                    // Asked of the REGISTRY, not of the old desired set:
                    // the registry is what knows whether a live join still
                    // holds the channel, which is the case that must not
                    // be unsubscribed.
                    let held: Vec<interweave_transport_api::ChannelId> =
                        broadcast_state.channels.values().cloned().collect();
                    for channel in held {
                        if !broadcast_state.subs.backend_should_subscribe(&channel) {
                            let topic = broadcast_state.forget(&channel);
                            let _ = swarm.unsubscribe_topic(&topic);
                        }
                    }
                    let _ = reply.send(Ok(()));
                }
                Err(denial) => {
                    let _ = reply.send(Err(format!("{denial:?}")));
                }
            }
        }
        SwarmCommand::Join {
            channel,
            session,
            reply,
        } => {
            match broadcast_state.subs.join(channel.clone(), session.clone()) {
                Ok(()) => {
                    // The queue is opened by the JOIN, which is what
                    // bounds the key set by local state rather than by
                    // anything a remote peer names.
                    if !broadcast_state.queues.is_open(&session) {
                        broadcast_state
                            .queues
                            .open(session, broadcast_state.queue_bound);
                    }
                    let topic = broadcast_state.remember(&channel);
                    let _ = swarm.subscribe_topic(&topic);
                    let _ = reply.send(Ok(()));
                }
                Err(_) => {
                    // A ceiling, not a policy: the session asked for more
                    // than the profile may hold.
                    let _ = reply.send(Err(DirectError::Overloaded));
                }
            }
        }
        SwarmCommand::Leave {
            channel,
            session,
            reply,
        } => {
            broadcast_state.subs.leave(&channel, &session);

            // A SESSION THAT HOLDS NOTHING GETS ITS QUEUE BACK. Without
            // this the map keeps an entry per session id ever seen, and a
            // local client that joins and leaves under a fresh id each
            // time grows it without bound -- along with whatever those
            // queues still hold. `SubscriptionRegistry` bounds sessions
            // per channel; nothing bounded the queue map itself.
            //
            // Asked AFTER the leave and about ALL channels, because a
            // session that left one of several is still live and still
            // owed the deliveries on the others.
            if !broadcast_state.subs.holds_any(&session) {
                broadcast_state.queues.close(&session);
            }

            // UNSUBSCRIBE ONLY WHEN NOBODY HOLDS IT, joined or desired.
            // A profile that desires the channel keeps the mesh warm with
            // no local consumer, which is the whole point of `desired`.
            if !broadcast_state.subs.backend_should_subscribe(&channel) {
                let topic = broadcast_state.forget(&channel);
                let _ = swarm.unsubscribe_topic(&topic);
            }
            let _ = reply.send(());
        }
        SwarmCommand::Publish {
            channel,
            session,
            frame,
            reply,
        } => {
            // 1. THE CALLER'S OWN JOIN. PUBSUB.md: the runtime does not
            //    implicitly subscribe and does not borrow another local
            //    client's reference. Checked before any byte reaches the
            //    backend, so a refusal is invisible on the wire.
            if !broadcast_state.subs.may_publish(&channel, &session) {
                let _ = reply.send(Err(DirectError::ChannelNotJoined));
                return;
            }
            if frame.payload.len() > effective_payload {
                let _ = reply.send(Err(DirectError::PayloadTooLarge));
                return;
            }
            if manager.is_draining() {
                let _ = reply.send(Err(DirectError::ShuttingDown));
                return;
            }
            let topic = broadcast_state.remember(&channel);
            // LOCAL ACCEPTANCE, not the backend's Ok. A lone node's
            // publish comes back `NoPeersSubscribedToTopic`, which
            // `publish_error` reads as success with degraded reachability
            // -- and a fan-out hung off the Ok arm alone therefore missed
            // the one case Model B cares most about: two local clients on
            // a node with no peers at all.
            let mut unreachable = false;
            let answer = match swarm.publish_broadcast(topic.hash(), frame.encode()) {
                Ok(_) => Ok(()),
                Err(error) => {
                    unreachable = matches!(
                        error,
                        libp2p::gossipsub::PublishError::NoPeersSubscribedToTopic
                    );
                    super::broadcast::publish_error(&error).map_or(Ok(()), Err)
                }
            };
            // ONE PRIORITY ORDER FOR THE TURN: the drop report (emitted
            // inside `deliver_locally`, because data was actually lost),
            // then this, then the wake-ups.
            //
            // PUBSUB.md requires `mesh_peer_count=0` to surface as
            // degraded rather than as delivery, and the caller's `Ok` is
            // the wrong place for it -- broadcast promises the caller
            // nothing about reach. The operator gets the other half.
            //
            // It sits BELOW the drop report deliberately: a review round
            // found each of the two orderings against the wake-ups, and
            // the resolution is not to give diagnostics extra room --
            // `polling_room` stops the Swarm the moment the outbox passes
            // the base capacity -- but to rank them. Actual loss outranks
            // degraded reachability, which outranks a notification for a
            // message the session already holds.
            let outcome = if answer.is_ok() {
                deliver_locally(
                    broadcast_state,
                    manager,
                    outbox,
                    &channel,
                    &session,
                    &frame,
                    LocalPublishTick {
                        now_ms,
                        wall_ms,
                        event_capacity,
                    },
                )
            } else {
                None
            };
            if unreachable && super::may_buffer_delivery(outbox.len(), event_capacity) {
                outbox.push_back(SwarmEvent::BroadcastUnreachable {
                    channel: channel.clone(),
                });
            }
            if let Some(outcome) = outcome {
                for delivered in outcome.sessions {
                    if !super::may_buffer_delivery(outbox.len(), event_capacity) {
                        break;
                    }
                    outbox.push_back(SwarmEvent::BroadcastDelivered {
                        channel: channel.clone(),
                        source_peer: outcome.source_peer.clone(),
                        session: delivered,
                    });
                }
            }
            let _ = reply.send(answer);
        }
        SwarmCommand::DrainSession { session, reply } => {
            let _ = reply.send(broadcast_state.queues.drain(&session));
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
            // AND BROADCAST'S, which is a separate copy and so a separate
            // way to be stale. Without this a revoked peer's connection
            // closes while its next broadcast is still admitted by the
            // policy that trusted it -- the same divergence the direct
            // line above exists to prevent, in the mode that fans out.
            broadcast_state.adopt_trust(&trust);
            let revoked = manager.set_trust(*trust, &live);

            // AND THE MESH MOVES WITH IT, for every live peer rather than
            // only the revoked ones. A peer DEMOTED to infrastructure-only
            // is not revoked -- ADR-0036 keeps its connection so it can
            // carry AutoNAT and relay traffic -- so it survives the
            // closing below and would otherwise keep receiving broadcast
            // on a connection the data plane no longer authorizes. The
            // promotion direction matters equally: a peer that becomes
            // trusted must stop being blacklisted, or a revocation
            // followed by a restoration would leave it permanently
            // excluded from the mesh with nothing reporting why.
            for peer in &live {
                if let Ok(id) = to_peer_id(peer) {
                    let trusted = mesh_admits(manager.classify(peer));
                    swarm.sync_broadcast_admission(&id, trusted);
                }
            }

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
        SwarmCommand::SendDirect {
            session,
            peer,
            mut frame,
            reply,
        } => {
            // THE SOURCE ENDPOINT IS THE CALLER'S LEASE, and the frame's
            // own field is REPLACED rather than checked. CLAUDE.md §5:
            // source EndpointId is derived from the local lease, never
            // trusted from the caller — and a comparison would still
            // make the field caller-meaningful, since the caller would
            // learn which values pass. So the session is the only input.
            //
            // Stage 6 accepted any lease this node held, because the
            // handle holder was the only caller and owned every endpoint.
            // That gap was carried here by explicit decision (PR #38),
            // and closes with sessions: a session holds ONE lease and
            // sends as that endpoint or not at all —
            // `a_session_sends_only_as_its_own_endpoint`.
            let Some(source) = direct_state.source_of(&LocalSessionId(session)) else {
                let _ = reply.send(Err(DirectError::EndpointNotRegistered));
                return;
            };
            frame.source_endpoint = source;
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
        SwarmCommand::ClaimEndpoint {
            session,
            endpoint,
            client_kind,
            reply,
        } => {
            let _ =
                reply.send(direct_state.claim(LocalSessionId(session), &endpoint, &client_kind));
        }
        SwarmCommand::ReleaseSession { session, reply } => {
            let _ = reply.send(direct_state.release(&LocalSessionId(session)));
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
/// Forget one address a listener is no longer serving.
///
/// Returns whether it was being tracked, so the caller reports a
/// withdrawal only for an address a consumer was actually told about.
///
/// The ENTRY SURVIVES an emptied address list. The listener itself is
/// still open — only one of its addresses went away — so it still holds
/// a socket and must still count against `max_active_listeners`. Removing
/// the entry here would hand its slot back while it was still serving.
///
/// A free function because `SwarmEvent` is `#[non_exhaustive]`: an
/// `ExpiredListenAddr` cannot be constructed outside libp2p, so the arm
/// that handles it cannot be reached from a test. The decision therefore
/// lives where a test can reach it, the same reason `admit_outbound` was
/// extracted.
/// The clocks and the bound one local publish is admitted against.
///
/// Named rather than passed as three more positional scalars: two of them
/// are `u64` milliseconds that mean different things, and a call site that
/// swapped them would compile.
struct LocalPublishTick {
    /// Monotonic milliseconds, for dedup TTLs.
    now_ms: u64,
    /// Wall-clock milliseconds, for the receipt stamp.
    wall_ms: u64,
    /// The BASE outbox capacity, re-read before every event.
    event_capacity: usize,
}

/// Admit a locally published broadcast for the OTHER sessions that joined.
///
/// Through the SAME admission as an inbound message, not beside it.
/// GossipSub does not loop a publish back to its own node, so without
/// this two local clients on one profile never see each other's messages
/// — the case `human-client-model-b.md` is built around.
///
/// The first version of this pushed straight into the queues, and every
/// difference from inbound admission was a defect: retries of one
/// envelope were delivered once remotely and once PER ATTEMPT locally,
/// because only dedup collapses the republishing a publisher does while
/// the mesh forms; same-key conflicting bodies that inbound refuses were
/// delivered; and neither the delivery wake-up nor the overload drop was
/// reported. Sharing the path is what stops the two drifting again.
fn deliver_locally(
    broadcast_state: &mut super::broadcast::BroadcastState,
    manager: &ConnectionManager,
    outbox: &mut std::collections::VecDeque<SwarmEvent>,
    channel: &interweave_transport_api::ChannelId,
    publisher_session: &str,
    frame: &interweave_transport_api::BroadcastMessageV1,
    tick: LocalPublishTick,
) -> Option<LocalOutcome> {
    let LocalPublishTick {
        now_ms,
        wall_ms,
        event_capacity,
    } = tick;
    let source_peer = manager.local_peer().cloned()?;
    let admission = interweave_transport_runtime::broadcast_inbound::admit_local_broadcast(
        frame,
        channel,
        &source_peer,
        interweave_transport_runtime::direct_inbound::Clocks {
            monotonic_ms: now_ms,
            wall_ms,
        },
        Some(publisher_session),
        &mut interweave_transport_runtime::broadcast_inbound::LocalContext {
            dedup: &mut broadcast_state.dedup,
            subs: &broadcast_state.subs,
            queues: &mut broadcast_state.queues,
        },
    );

    let interweave_transport_runtime::broadcast_inbound::BroadcastAdmission::Delivered {
        sessions,
        dropped,
    } = admission
    else {
        return None;
    };

    // THE SAME EVENTS AS INBOUND, under the same live capacity check, and
    // in one priority order across the whole command turn:
    //
    //   1. the DROP report -- data was actually lost;
    //   2. the zero-mesh report -- reachability is degraded;
    //   3. the delivery wake-ups -- a message the session already holds.
    //
    // The order is the whole mechanism, because there is no room above
    // the base capacity to give: `polling_room` stops the Swarm being
    // polled the moment the outbox passes it, so a "reserve" for
    // diagnostics buys one more event and costs the node its ability to
    // answer anything. That was tried, and the test for an earlier
    // finding caught it.
    //
    // Every push re-reads the outbox length: one publish can notify every
    // joined session, which is how a single free slot becomes N events.
    if !dropped.is_empty() && super::may_buffer_delivery(outbox.len(), event_capacity) {
        outbox.push_back(SwarmEvent::BroadcastDropped {
            channel: channel.clone(),
            source_peer: source_peer.clone(),
            sessions: dropped.len(),
        });
    }
    Some(LocalOutcome {
        sessions,
        source_peer,
    })
}

/// What a local publish delivered, for the caller to announce.
///
/// Returned rather than emitted inside `deliver_locally` so the command
/// can put the zero-mesh report BETWEEN the drop report and these: three
/// event kinds in one turn, one priority order, one capacity.
struct LocalOutcome {
    sessions: Vec<String>,
    source_peer: interweave_transport_api::TransportIdentity,
}

pub(super) fn forget_address(
    active: &mut ActiveListeners,
    listener: ListenerId,
    address: &Multiaddr,
) -> bool {
    let Some(addresses) = active.get_mut(&listener) else {
        return false;
    };
    let before = addresses.len();
    addresses.retain(|a| a != address);
    before != addresses.len()
}

pub(super) fn translate(
    event: Libp2pSwarmEvent<SubstrateBehaviourEvent>,
    listens: &mut PendingListens,
    active: &mut ActiveListeners,
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
            // REMEMBERED FROM HERE ON. A listener may report several
            // addresses, so this accumulates rather than replaces.
            active.entry(listener_id).or_default().push(address.clone());
            Some(SwarmEvent::Listening { address })
        }
        // ONE ADDRESS GONE IS NOT THE LISTENER GONE. libp2p reports an
        // address going away without closing the listener, and only
        // `ListenerClosed` used to touch this table — so the stale
        // address stayed, `stop_listening` on it would have matched and
        // killed a listener still serving its other addresses, and the
        // consumer was never told the address had gone.
        Libp2pSwarmEvent::ExpiredListenAddr {
            listener_id,
            address,
        } => forget_address(active, listener_id, &address).then(|| {
            SwarmEvent::ListeningStopped {
                addresses: vec![address],
                // Expiry is orderly: the address went away, nothing
                // failed.
                reason: None,
            }
        }),
        // A listener that dies before binding must not leave `listen`
        // waiting for an address that will never arrive.
        //
        // AND ONE THAT DIES AFTER BINDING IS NOT SILENT. This arm used to
        // return `None` whenever there was no pending reply, which is
        // exactly the case where the listener was already serving: the
        // node stopped accepting connections on those addresses and
        // nothing said so. A caller that reported `Listening` to an
        // operator had no way to withdraw it.
        Libp2pSwarmEvent::ListenerClosed {
            listener_id,
            addresses,
            reason,
        } => {
            if let Some(reply) = listens.remove(&listener_id) {
                let _ = reply.send(Err("the listener closed before binding".to_owned()));
                // It never bound, so there is no `Listening` to withdraw
                // and the caller has already been told directly.
                return None;
            }
            active.remove(&listener_id);
            Some(SwarmEvent::ListeningStopped {
                addresses,
                reason: reason.err().map(|e| e.to_string()),
            })
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
mod expired_address_tests {
    use super::{ActiveListeners, forget_address};
    use libp2p::Multiaddr;
    use libp2p::core::transport::ListenerId;

    fn addr(port: u16) -> Multiaddr {
        format!("/ip4/127.0.0.1/tcp/{port}")
            .parse()
            .expect("valid multiaddr")
    }

    #[test]
    fn an_expired_address_stops_naming_its_listener() {
        // The defect this closes: the stale address stayed in the table,
        // so `stop_listening` on it matched and would have removed a
        // listener that was still serving its other address.
        let id = ListenerId::next();
        let mut active: ActiveListeners = ActiveListeners::new();
        active.insert(id, vec![addr(1), addr(2)]);

        assert!(forget_address(&mut active, id, &addr(1)));
        assert_eq!(
            active.get(&id).map(Vec::as_slice),
            Some([addr(2)].as_slice()),
            "only the expired address goes"
        );
        assert!(
            !active.values().any(|a| a.contains(&addr(1))),
            "nothing can still resolve the expired address to this listener"
        );
    }

    #[test]
    fn a_listener_that_lost_every_address_still_holds_its_slot() {
        // It is still open — only its addresses went away — so it still
        // holds a socket and must still count against
        // `max_active_listeners`. Dropping the entry would hand the slot
        // back while the listener was still serving.
        let id = ListenerId::next();
        let mut active: ActiveListeners = ActiveListeners::new();
        active.insert(id, vec![addr(3)]);

        assert!(forget_address(&mut active, id, &addr(3)));
        assert!(
            active.contains_key(&id),
            "the listener is still open and still counted"
        );
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn an_address_that_was_never_tracked_reports_no_withdrawal() {
        // A consumer is told an address stopped serving only if it was
        // told it started, so the caller reports on this answer rather
        // than announcing a withdrawal nobody could match.
        let id = ListenerId::next();
        let mut active: ActiveListeners = ActiveListeners::new();
        active.insert(id, vec![addr(4)]);

        assert!(!forget_address(&mut active, id, &addr(5)));
        assert!(!forget_address(&mut active, ListenerId::next(), &addr(4)));
    }
}
