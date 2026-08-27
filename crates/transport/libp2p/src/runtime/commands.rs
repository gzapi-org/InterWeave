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

use libp2p::core::transport::ListenerId;
use libp2p::identify;
use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;

use interweave_transport_api::TransportError as DirectError;
use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{ConnectionClass, ConnectionManager, DialOrigin, DialTicket};

use crate::behaviour::SubstrateBehaviourEvent;
use crate::gated_swarm::{GatedSwarm, NotConnected};

use super::dialing::{OpenConnection, PendingListens, attempt_dial, connections_to_close};
use super::messages::{DialRefusal, SwarmCommand, SwarmEvent};

// Still beside the loop that owns them; step 5 moves these out.
use super::{DirectState, PendingDirect, admit_outbound, to_peer_id, to_transport_identity};

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_command(
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
pub(super) fn translate(
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
