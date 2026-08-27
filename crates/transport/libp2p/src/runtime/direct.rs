// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! `/interweave/direct/2.0.0`: inbound admission, outbound exchanges,
//! and the endpoint configuration both sides resolve against.
//!
//! Split out of `runtime.rs` unchanged. The admission decisions
//! themselves live in `interweave-transport-runtime` as pure state
//! machines; what is here is the libp2p-shaped half — the request /
//! response plumbing, the response validation a sender performs before
//! anything is cached or surfaced, and the failure mapping that turns an
//! `OutboundFailure` into the error a caller is told.

use std::collections::HashMap;

use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;

use interweave_transport_api::TransportError as DirectError;
use interweave_transport_api::{
    DirectMessageV2, DirectRejectReason, EndpointId, TransportIdentity,
};
use interweave_transport_runtime::TrustSources;
use interweave_transport_runtime::dedup::RecordedRoute;
use interweave_transport_runtime::direct_inbound::{
    AdmissionContext, Outcome as AdmissionOutcome, PrefixContext, admit_prefix, admit_structured,
};
use interweave_transport_runtime::endpoint_queue::EndpointQueues;
use interweave_transport_runtime::endpoint_registry::{EndpointRegistry, RegisteredEndpoint};

use crate::behaviour::SubstrateBehaviourEvent;
use crate::gated_swarm::GatedSwarm;

use super::config::SubstrateError;
use super::messages::SwarmEvent;
use super::{PendingDirect, to_transport_identity};

/// Everything inbound direct admission owns, held by the Swarm task.
///
/// Constructed empty: a profile with no endpoint leases admits nothing,
/// which is the correct posture for a daemon that has just started and
/// has no local client attached yet (`testing.md` scenario 27).
pub struct DirectState {
    /// Who this profile trusts, mirrored from the manager's sources.
    pub(super) trust: interweave_transport_runtime::PeerTrustPolicy,
    /// Direct-ingress token buckets.
    pub(super) ingress: interweave_transport_runtime::ingress::IngressLimiter,
    /// The duplicate cache.
    pub(super) dedup: interweave_transport_runtime::dedup::DedupCache,
    /// In-flight reservations.
    pub(super) reservations: interweave_transport_runtime::dedup::ReservationMap,
    /// Configured endpoints and their leases.
    pub(super) registry: EndpointRegistry,
    /// Open delivery queues.
    pub(super) queues: EndpointQueues,
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
    pub(super) answering: std::collections::BTreeSet<libp2p::request_response::InboundRequestId>,
}

/// One synthetic in-process session's lease epoch, distinct per endpoint.
///
/// `LOCAL-CLIENT.md` requires a "fresh 128-bit lease epoch for every
/// grant". Stage 8 satisfies that directly: every real session mints its
/// own at establishment. Until then `configure_direct` stands in for N
/// sessions while being handed ONE value, and passing that value to every
/// lease is not a harmless simplification — the epoch is how `revoke`
/// tells a client WHICH routes to discard, so a shared epoch names routes
/// that are still live on the endpoints that were not revoked.
///
/// The derivation keeps the supplied value as a prefix and appends the
/// endpoint's index. The endpoint NAME is not used: `EndpointId` admits
/// `.`, which is outside `Generation`'s `[A-Za-z0-9_-]`, so splicing a
/// name in would make the epoch unparseable for a legal configuration.
///
/// Slicing by byte is safe because that same grammar is ASCII-only.
fn derived_epoch(
    base: &interweave_transport_runtime::Generation,
    index: usize,
) -> Result<interweave_transport_runtime::Generation, SubstrateError> {
    use interweave_transport_runtime::Generation;

    let suffix = format!("-{index}");
    let room = Generation::MAX_BYTES.saturating_sub(suffix.len());
    let base = base.as_str();
    let keep = base.len().min(room);
    Generation::parse(format!("{}{suffix}", &base[..keep])).map_err(|_| {
        SubstrateError::InvalidProfile(vec![format!(
            "endpoint {index} could not be given a distinct lease epoch"
        )])
    })
}

impl DirectState {
    /// Adopt the profile's data-plane trust.
    ///
    /// THE SAME `PeerTrustPolicy` THE MANAGER USES, taken from the one
    /// `TrustSources` a caller supplied rather than kept separately.
    /// Two copies of "who may talk to us" is two answers, and the one a
    /// directed message met would eventually differ from the one that
    /// admitted its connection.
    pub(super) fn adopt_trust(&mut self, trust: &TrustSources) {
        self.trust = trust.peers.clone();
    }

    /// Install endpoint configuration and open a queue for each lease.
    pub(super) fn configure(&mut self, config: DirectEndpoints) -> Result<(), SubstrateError> {
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
        for (index, (name, configured)) in config.endpoints.iter().enumerate() {
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
                    // A DISTINCT EPOCH PER LEASE, not one shared value.
                    // See `derived_epoch`.
                    derived_epoch(&config.epoch, index)?,
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
    pub(super) fn answering(&self) -> usize {
        self.answering.len()
    }

    /// Take everything waiting for `endpoint`.
    pub(super) fn drain(
        &mut self,
        endpoint: &EndpointId,
    ) -> Vec<interweave_transport_runtime::DirectEvent> {
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
    pub(super) fn revoke(&mut self, endpoint: &EndpointId) -> usize {
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
pub(super) struct DirectTick {
    /// Monotonic milliseconds since the runtime started.
    pub(super) now_ms: u64,
    /// The profile's effective payload limit, for the deferred parse.
    pub(super) max_payload_bytes: usize,
    /// Unix-epoch milliseconds, for the receipt time on a delivery.
    ///
    /// Separate from `now_ms`, which is monotonic-since-startup and
    /// governs rate limits and deadlines. A receipt time taken from that
    /// clock restarts near zero with the process.
    pub(super) wall_ms: u64,
    /// Whether the node has begun draining.
    pub(super) draining: bool,
    /// Whether an accepted delivery may be buffered for the consumer.
    ///
    /// Decided by [`may_buffer_delivery`] from the BASE capacity, so a
    /// delivery cannot spend the slack reserved for in-flight work.
    pub(super) may_buffer_delivery: bool,
}

/// Whether a swarm event was a direct-protocol one.
pub(super) enum DirectHandled {
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
pub(super) fn handle_direct(
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
                // THE NARROW CONTEXT, and that is the point: these three
                // gates are all this call reads, so it no longer has to
                // borrow the registry and every open queue to ask whether
                // a peer is trusted, draining, or over its rate.
                let mut ctx = PrefixContext {
                    trust: &state.trust,
                    ingress: &mut state.ingress,
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
                    prefix: PrefixContext {
                        trust: &state.trust,
                        ingress: &mut state.ingress,
                        draining,
                    },
                    dedup: &mut state.dedup,
                    reservations: &mut state.reservations,
                    registry: &state.registry,
                    queues: &mut state.queues,
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
                // A SETTLED WAITER IS ANSWERED, not dropped. When the
                // owner has finished, its outcome is in the positive
                // cache and that is what the waiter receives; letting the
                // `ResponseChannel` fall out of scope there would send
                // nothing at all, and the remote would wait out its own
                // deadline for a message this profile had already decided
                // about.
                //
                // An UNSETTLED waiter is the opposite case and is
                // deliberately not answered — see the `None` arm below.
                // The two must not be conflated: one has an answer and
                // must deliver it, the other has none and must not invent
                // one.
                //
                // Both read as academic today because this arm is
                // UNREACHABLE: `admit_inbound` acquires and releases the
                // reservation inside one synchronous call, so nothing is
                // ever in flight when the next request arrives: a
                // duplicate that follows is a positive-cache hit, and
                // this arm is UNREACHABLE. That is a property of today's
                // admission, not of the protocol.
                //
                // It stops being unreachable at the first stage whose
                // admission yields while holding a reservation — the
                // local-client IPC boundary — and that is when the
                // retention ADR-0019 requires must be built: hold the
                // channel until the owner settles, then answer every
                // waiter with the owner's outcome.
                //
                // Until then this must not ANSWER the branch. It
                // previously replied `overloaded`, and the ADR-0019
                // amendment of 2026-08-27 names that non-conforming:
                // exhaustion is a refusal and a waiter was admitted, so
                // the reply reported a limit the request never hit. Not
                // answering is recoverable — the peer retries, the owner
                // has settled by then, and dedup answers correctly —
                // whereas a wrong answer is final.
                AdmissionOutcome::AttachedAsWaiter => {
                    match waiter_response(&state.dedup, &request, &source) {
                        Some(response) => response,
                        None => {
                            debug_assert!(
                                false,
                                "a waiter attached with no settled owner: admission \
                                 yields now, so ADR-0019 waiter retention is owed"
                            );
                            return DirectHandled::Consumed;
                        }
                    }
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
pub(super) fn waiter_response(
    dedup: &interweave_transport_runtime::dedup::DedupCache,
    request: &DirectMessageV2,
    source: &TransportIdentity,
) -> Option<crate::direct_codec::DirectResponse> {
    use crate::direct_codec::DirectResponse;
    let key = interweave_transport_runtime::direct_inbound::dedup_key(request, source);
    // `None` means the owner has NOT settled, and this function has no
    // answer to give. It used to return `overloaded` there, which the
    // ADR-0019 amendment of 2026-08-27 names non-conforming: exhaustion
    // is a refusal, and a waiter was ADMITTED. Reporting a limit for a
    // request that passed one is a different answer to a different
    // question, and the caller cannot tell the two apart if this
    // function invents one.
    //
    // A BROADCAST ROUTE HERE IS ALSO `None`, and for the same reason: the
    // key built above is a direct key, so a broadcast record cannot be
    // stored under it — and if one somehow were, this function still has
    // no endpoint to name. Answering with an invented one is exactly the
    // fabrication the paragraph above refuses.
    dedup.get(&key).and_then(|record| match &record.route {
        RecordedRoute::Direct { resolved_endpoint } => Some(DirectResponse::Accepted {
            message_id: request.message_id,
            resolved_endpoint: resolved_endpoint.clone(),
        }),
        RecordedRoute::Broadcast => None,
    })
}

/// Validate a response before a caller may believe it.
///
/// `DIRECT.md`: a sender validates every remote field before caching or
/// surfacing it. A response that does not satisfy this is a local
/// `ProtocolViolation` and creates no positive result.
pub(super) fn validate_response(
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
pub(super) fn outbound_error(error: &libp2p::request_response::OutboundFailure) -> DirectError {
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
        DirectMessageV2, EndpointId, MediaType, MessageId, Payload, TransportIdentity,
    };
    use interweave_transport_runtime::dedup::{DEFAULT_TTL_MS, DedupCache, RecordedRoute};
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
        dedup.record_accepted(
            dedup_key(&req, &peer()),
            RecordedRoute::Direct {
                resolved_endpoint: endpoint("claude"),
            },
            fingerprint,
            0,
        );

        assert_eq!(
            waiter_response(&dedup, &req, &peer()),
            Some(DirectResponse::Accepted {
                message_id: req.message_id,
                resolved_endpoint: endpoint("claude"),
            })
        );
    }

    /// NO ANSWER IS NOT THE SAME AS A REFUSAL. A cache miss means the
    /// owner has not settled, and this function has none of the owner's
    /// outcome to relay.
    ///
    /// It used to answer `overloaded` here, and its own documentation
    /// called that "the honest answer to 'I cannot hold this open for
    /// you'". The ADR-0019 amendment of 2026-08-27 names it
    /// non-conforming instead: exhaustion is a REFUSAL, and a waiter was
    /// ADMITTED, so the reply reported a limit the request never hit and
    /// a sender could not tell the two apart.
    ///
    /// Returning `None` makes the absence something the caller must
    /// handle rather than something this function papers over.
    #[test]
    fn a_waiter_with_no_settled_owner_gets_no_fabricated_answer() {
        let dedup = DedupCache::new(64, DEFAULT_TTL_MS);
        let req = request();
        assert_eq!(
            waiter_response(&dedup, &req, &peer()),
            None,
            "a missing owner outcome must not become a refusal"
        );
    }

    /// The id is echoed on both shapes, so a waiter's answer settles the
    /// exchange it belongs to rather than being discarded by the sender's
    /// own id check.
    #[test]
    fn a_waiters_answer_echoes_the_request_id() {
        let mut dedup = DedupCache::new(64, DEFAULT_TTL_MS);
        let req = request();
        let fingerprint = direct_content_fingerprint_v1(Some("text/plain"), b"hi").expect("hashes");
        dedup.record_accepted(
            dedup_key(&req, &peer()),
            RecordedRoute::Direct {
                resolved_endpoint: endpoint("claude"),
            },
            fingerprint,
            0,
        );
        match waiter_response(&dedup, &req, &peer()) {
            Some(
                DirectResponse::Accepted { message_id, .. }
                | DirectResponse::Rejected { message_id, .. },
            ) => assert_eq!(message_id, req.message_id),
            None => panic!("a settled owner must produce an answer"),
        }
    }
}

#[cfg(test)]
mod derived_epoch_tests {
    use super::derived_epoch;
    use interweave_transport_runtime::Generation;

    fn base(seed: &str) -> Generation {
        Generation::parse(format!("{seed:_<16}")).expect("valid generation")
    }

    #[test]
    fn every_endpoint_index_gets_a_different_epoch() {
        // The whole point. One shared epoch across N leases makes
        // `revoke`'s answer name routes that are still live.
        let b = base("cfg");
        let a = derived_epoch(&b, 0).expect("index 0");
        let c = derived_epoch(&b, 1).expect("index 1");
        assert_ne!(a, c);
        assert_ne!(a.as_str(), b.as_str(), "and none of them is the base");
    }

    #[test]
    fn a_maximum_length_base_still_yields_a_legal_epoch() {
        // `Generation` is 16..=64 bytes. Appending without making room
        // would push a 64-byte base over the ceiling and fail to parse,
        // turning a legal profile into a configuration error.
        let long = Generation::parse("x".repeat(Generation::MAX_BYTES)).expect("64 bytes is legal");
        for index in [0_usize, 9, 10, 99] {
            let derived = derived_epoch(&long, index).expect("still parses");
            assert!(
                (Generation::MIN_BYTES..=Generation::MAX_BYTES).contains(&derived.as_str().len()),
                "index {index} produced {} bytes",
                derived.as_str().len()
            );
        }
    }
}
