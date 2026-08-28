// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `/interweave/endpoints/1.0.0`: answering directory queries, asking
//! them, and the requester's advisory cache.
//!
//! The libp2p-shaped half. The decisions are pure and live in
//! `transport-runtime`: the responder's snapshot is
//! `EndpointRegistry::advertised_for`, the requester's validation and
//! cache are `transport_runtime::directory`, and the budget is
//! `DirectoryBudget`. What is here is the request-response plumbing and
//! the trust class check that gates a query before any list is built.

use std::collections::HashMap;

use libp2p::request_response::{Event as RrEvent, Message as RrMessage, OutboundRequestId};
use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;

use interweave_transport_api::{
    DirectoryRefusal, EndpointDirectoryV1, EndpointId, TransportError as DirectError,
    TransportIdentity,
};
use interweave_transport_runtime::ConnectionClass;
use interweave_transport_runtime::directory::{
    DirectoryBudget, DirectoryCache, DirectoryViolation, validate_response,
};
use tokio::sync::oneshot;

use crate::behaviour::SubstrateBehaviourEvent;
use crate::endpoints_codec::DirectoryResponse;
use crate::gated_swarm::GatedSwarm;

use super::direct::DirectState;
use super::to_transport_identity;

/// What a caller learns from a directory query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryResult {
    /// The advertised endpoints, validated and sorted.
    pub endpoints: Vec<EndpointId>,
    /// Freshness deadline in this node's monotonic-ms frame; the entry is
    /// good until then.
    pub fresh_until_ms: u64,
    /// Whether the answer came from cache rather than the wire.
    pub cached: bool,
    /// Whether the remote sent an unsorted list that was sorted locally.
    pub noncanonical: bool,
}

/// One directory exchange awaiting its answer.
pub(super) struct PendingQuery {
    /// The peer asked, so a cache entry can be keyed on the answer.
    pub(super) peer: TransportIdentity,
    /// The caller waiting.
    pub(super) reply: oneshot::Sender<Result<DirectoryResult, DirectError>>,
}

/// Everything the directory owns on the Swarm task.
pub(super) struct DirectoryState {
    /// The requester's advisory cache.
    pub(super) cache: DirectoryCache,
    /// The responder's per-peer / in-flight budget.
    pub(super) budget: DirectoryBudget,
    /// The local cache TTL term of the clamp, in ms.
    pub(super) local_cache_ttl_ms: u32,
    /// Inbound queries whose answer this node is still writing.
    ///
    /// A set, not a count, for the same reason `DirectState::answering`
    /// is: an `InboundFailure` for a query never admitted must release
    /// nothing that belongs to another. The budget's in-flight slot is
    /// released when the id leaves this set.
    pub(super) answering: std::collections::BTreeSet<libp2p::request_response::InboundRequestId>,
}

impl DirectoryState {
    /// Build the directory's task state from the runtime configuration.
    #[must_use]
    pub(super) fn new(now_ms: u64, cache_peers: usize, local_cache_ttl_ms: u32) -> Self {
        Self {
            cache: DirectoryCache::new(cache_peers, local_cache_ttl_ms),
            budget: DirectoryBudget::with_defaults(now_ms),
            local_cache_ttl_ms,
            answering: std::collections::BTreeSet::new(),
        }
    }

    /// Queries whose answer is queued but not yet written — read by
    /// `polling_room` so an answer cannot be starved out of its slot.
    pub(super) fn answering(&self) -> usize {
        self.answering.len()
    }
}

/// The outcome of trying to build a directory for a querying peer.
enum Answer {
    Directory(EndpointDirectoryV1),
    Refused(DirectoryRefusal),
}

/// One directory-protocol event, or the event handed back untouched.
pub(super) fn handle_endpoints(
    event: Libp2pSwarmEvent<SubstrateBehaviourEvent>,
    swarm: &mut GatedSwarm,
    direct_state: &mut DirectState,
    directory: &mut DirectoryState,
    manager: &interweave_transport_runtime::ConnectionManager,
    pending: &mut HashMap<OutboundRequestId, PendingQuery>,
    now_ms: u64,
) -> Handled {
    let Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Endpoints(event)) = event else {
        return Handled::Passed(Box::new(event));
    };

    match event {
        RrEvent::Message {
            peer,
            message:
                RrMessage::Request {
                    channel,
                    request_id,
                    ..
                },
            ..
        } => {
            let Ok(querier) = to_transport_identity(&peer) else {
                return Handled::Consumed;
            };
            let answer = build_answer(&querier, direct_state, directory, manager, now_ms);
            let response = match answer {
                Answer::Directory(directory) => DirectoryResponse::Directory(directory),
                Answer::Refused(reason) => DirectoryResponse::Refused(reason),
            };
            // ONLY AN ADMITTED QUERY HOLDS A SLOT. `build_answer` reserved
            // one exactly when it returned a Directory, so the release on
            // failure below matches it.
            let admitted = matches!(response, DirectoryResponse::Directory(_));
            if swarm.answer_endpoints(channel, response).is_ok() {
                if admitted {
                    directory.answering.insert(request_id);
                }
            } else if admitted {
                // The answer never left; give the slot straight back.
                directory.budget.end_exchange();
            }
            Handled::Consumed
        }

        RrEvent::Message {
            message:
                RrMessage::Response {
                    request_id,
                    response,
                },
            ..
        } => {
            let Some(PendingQuery { peer, reply }) = pending.remove(&request_id) else {
                return Handled::Consumed;
            };
            let _ = reply.send(receive(response, &peer, directory, now_ms));
            Handled::Consumed
        }

        RrEvent::OutboundFailure {
            request_id, error, ..
        } => {
            if let Some(PendingQuery { reply, .. }) = pending.remove(&request_id) {
                let _ = reply.send(Err(outbound_error(&error)));
            }
            Handled::Consumed
        }

        RrEvent::InboundFailure { request_id, .. } | RrEvent::ResponseSent { request_id, .. } => {
            // The answer is written or its stream died; either way the
            // in-flight slot it held is freed, once, keyed by the id so a
            // query this side never admitted releases nothing.
            if directory.answering.remove(&request_id) {
                directory.budget.end_exchange();
            }
            Handled::Consumed
        }
    }
}

/// Decide what to answer a querying peer — and reserve an in-flight slot
/// exactly when the answer is a directory.
fn build_answer(
    querier: &TransportIdentity,
    direct_state: &DirectState,
    directory: &mut DirectoryState,
    manager: &interweave_transport_runtime::ConnectionManager,
    now_ms: u64,
) -> Answer {
    // DISABLED OR DRAINING FIRST: neither reveals anything about any
    // endpoint, and both are true before trust is even consulted.
    if !direct_state.directory_enabled() || manager.is_draining() {
        return Answer::Refused(DirectoryRefusal::Unavailable);
    }
    // DATA-PLANE TRUST is the admission. An infrastructure-only peer is
    // not data-plane trusted, so ADR-0036's "no endpoint directory" falls
    // out of the same check rather than needing its own.
    if manager.classify(querier) != ConnectionClass::DataPlaneTrusted {
        return Answer::Refused(DirectoryRefusal::Unauthorized);
    }
    // THE BUDGET, which reserves an in-flight slot on success. A refusal
    // holds nothing.
    if directory.budget.begin_exchange(querier, now_ms).is_err() {
        return Answer::Refused(DirectoryRefusal::Overloaded);
    }
    let endpoints = direct_state.advertised_for(querier);
    Answer::Directory(EndpointDirectoryV1 {
        generated_at_ms: now_ms,
        ttl_ms: directory.local_cache_ttl_ms,
        endpoints,
    })
}

/// Validate a response, cache it, and shape the caller's result.
fn receive(
    response: DirectoryResponse,
    peer: &TransportIdentity,
    directory: &mut DirectoryState,
    now_ms: u64,
) -> Result<DirectoryResult, DirectError> {
    match response {
        DirectoryResponse::Refused(reason) => Err(match reason {
            // The remote refused. Coarse in, coarse out: the caller learns
            // "not available to me", never which branch applied.
            DirectoryRefusal::Overloaded => DirectError::Overloaded,
            DirectoryRefusal::Unauthorized => DirectError::UnauthorizedPeer,
            DirectoryRefusal::Unavailable => DirectError::RemoteEndpointUnavailable,
        }),
        DirectoryResponse::Directory(raw) => match validate_response(&raw) {
            Err(DirectoryViolation::TooManyEntries { .. } | DirectoryViolation::Duplicate(_)) => {
                // A hostile list is not cached and not surfaced.
                Err(DirectError::ProtocolViolation)
            }
            Ok(validated) => {
                let noncanonical = validated.noncanonical;
                let entry = directory.cache.insert(peer.clone(), validated, now_ms);
                Ok(DirectoryResult {
                    endpoints: entry.endpoints.clone(),
                    fresh_until_ms: entry.fresh_until_ms,
                    cached: false,
                    noncanonical,
                })
            }
        },
    }
}

/// Start an outbound query, or answer it from cache.
///
/// Returns `Ok(None)` when a fresh cache entry answered and the caller
/// has already been replied to; `Ok(Some(id))` when an exchange was
/// dispatched and its answer will arrive as a `Response` event; `Err`
/// when nothing could be sent.
pub(super) fn begin_query(
    swarm: &mut GatedSwarm,
    directory: &mut DirectoryState,
    manager: &interweave_transport_runtime::ConnectionManager,
    peer: &TransportIdentity,
    now_ms: u64,
    reply: oneshot::Sender<Result<DirectoryResult, DirectError>>,
) -> Option<(OutboundRequestId, PendingQuery)> {
    // CACHE FIRST. A fresh entry needs no exchange, and answering from it
    // is what makes the directory advisory rather than a round trip per
    // read.
    if let Some(entry) = directory.cache.get(peer, now_ms) {
        let _ = reply.send(Ok(DirectoryResult {
            endpoints: entry.endpoints.clone(),
            fresh_until_ms: entry.fresh_until_ms,
            cached: true,
            noncanonical: entry.noncanonical,
        }));
        return None;
    }
    // A QUERY IS A DATA-PLANE OPERATION: an untrusted peer is refused
    // locally without a packet, the same class the responder checks.
    if manager.classify(peer) != ConnectionClass::DataPlaneTrusted {
        let _ = reply.send(Err(DirectError::UnauthorizedPeer));
        return None;
    }
    let Ok(peer_id) = super::to_peer_id(peer) else {
        // A trusted peer whose id the neutral grammar accepts but libp2p
        // cannot parse: nothing to dial and nothing to ask.
        let _ = reply.send(Err(DirectError::PeerUnknown));
        return None;
    };
    match swarm.query_endpoints(&peer_id) {
        Ok(request_id) => Some((
            request_id,
            PendingQuery {
                peer: peer.clone(),
                reply,
            },
        )),
        Err(_) => {
            // Not connected; the directory never originates a dial.
            let _ = reply.send(Err(DirectError::PeerUnreachable));
            None
        }
    }
}

/// Map an outbound failure onto a caller error, the same way direct does.
fn outbound_error(error: &libp2p::request_response::OutboundFailure) -> DirectError {
    use libp2p::request_response::OutboundFailure;
    match error {
        OutboundFailure::Io(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            // Our own decoder refused the response: a bad grammar, a bad
            // count, or trailing bytes arrive here.
            DirectError::ProtocolViolation
        }
        OutboundFailure::UnsupportedProtocols => DirectError::ProtocolUnsupported,
        OutboundFailure::Timeout
        | OutboundFailure::Io(_)
        | OutboundFailure::DialFailure
        | OutboundFailure::ConnectionClosed => DirectError::PeerUnreachable,
    }
}

/// Whether an endpoints event was consumed or should continue down the
/// event chain.
pub(super) enum Handled {
    Consumed,
    Passed(Box<Libp2pSwarmEvent<SubstrateBehaviourEvent>>),
}
