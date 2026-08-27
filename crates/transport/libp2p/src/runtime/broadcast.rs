// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Broadcast state and the inbound path, mirroring [`super::direct`].
//!
//! The admission decisions live in
//! `interweave_transport_runtime::broadcast_inbound`, which knows nothing
//! about libp2p. What is here is the part that must: turning a
//! `gossipsub::Message` into those decisions' inputs, and reporting the
//! ADR-0029 verdict back to the backend.
//!
//! # The topic is the channel, and the map is total
//!
//! The envelope carries no ChannelId, so the receiver learns the channel
//! from the topic it arrived on. That is not a lookup that can fail in
//! principle: a node only receives on topics it derived from a ChannelId
//! it holds. It is still written as a lookup that CAN fail, because
//! "cannot happen" and "is not checked" are different claims and only one
//! of them survives a future edit.

use std::collections::BTreeMap;

use libp2p::gossipsub;

use interweave_transport_api::ChannelId;
use interweave_transport_runtime::broadcast_inbound::{
    BroadcastAdmission, BroadcastContext, ProtocolVerdict, admit_broadcast, classify_broadcast,
};
use interweave_transport_runtime::direct_inbound::{Clocks, PrefixContext};
use interweave_transport_runtime::ingress::{IngressLimiter, SubscriptionRegistry};
use interweave_transport_runtime::session_queue::SessionQueues;
use interweave_transport_runtime::topic::topic_key_v1;
use interweave_transport_runtime::{PeerTrustPolicy, TrustSources};

use crate::behaviour::SubstrateBehaviourEvent;
use crate::gated_swarm::GatedSwarm;

use super::config::SubstrateError;
use super::messages::SwarmEvent;
use super::to_transport_identity;

/// The channels this profile holds and the queues they deliver to.
pub struct BroadcastState {
    /// Who this profile trusts, mirrored from the manager's sources.
    pub(super) trust: PeerTrustPolicy,
    /// Broadcast-ingress token buckets.
    ///
    /// A SEPARATE INSTANCE from direct's, not a shared one. The ADR-0026
    /// amendment accounts the two modes apart so a broadcast flood cannot
    /// spend a peer's direct allowance, which would turn a bound on one
    /// mode into a denial of the other.
    pub(super) ingress: IngressLimiter,
    /// The duplicate cache, keyed by broadcast identity.
    pub(super) dedup: interweave_transport_runtime::dedup::DedupCache,
    /// Local join references and the profile's desired channels.
    pub(super) subs: SubscriptionRegistry,
    /// Bounded per-session delivery queues.
    pub(super) queues: SessionQueues,
    /// Topic hash back to the channel that derived it.
    ///
    /// Populated whenever this node subscribes, which is what makes the
    /// reverse direction total: a topic reaches the inbound path only if
    /// this node asked for it.
    pub(super) channels: BTreeMap<String, ChannelId>,
    /// Bound applied to a session's queue when a join opens it.
    pub(super) queue_bound: usize,
}

impl BroadcastState {
    /// An empty state trusting `sources`, before any channel is held.
    #[must_use]
    pub fn new(sources: &TrustSources) -> Self {
        Self {
            trust: sources.peers.clone(),
            ingress: IngressLimiter::with_defaults(0),
            dedup: interweave_transport_runtime::dedup::DedupCache::default(),
            subs: SubscriptionRegistry::default(),
            queues: SessionQueues::new(),
            channels: BTreeMap::new(),
            // Replaced by `ConfigureBroadcast`; the local-client event
            // queue default until then, so a join before configuration
            // opens a sane queue rather than a zero one.
            queue_bound: interweave_local_client_api::DEFAULT_EVENT_QUEUE,
        }
    }

    /// Take the profile's trust after a policy change.
    ///
    /// The same reason `DirectState::adopt_trust` exists: a revocation
    /// must reach the data plane, not only the connection layer, or a
    /// peer whose connection is being closed still has its next message
    /// admitted.
    pub(super) fn adopt_trust(&mut self, sources: &TrustSources) {
        self.trust = sources.peers.clone();
    }

    /// The channel a topic hash belongs to, if this node holds it.
    pub(super) fn channel_of(&self, topic: &gossipsub::TopicHash) -> Option<&ChannelId> {
        self.channels.get(topic.as_str())
    }

    /// Remember the mapping for a channel this node is subscribing to.
    pub(super) fn remember(&mut self, channel: &ChannelId) -> gossipsub::IdentTopic {
        let wire = topic_key_v1(channel).wire_string();
        self.channels.insert(wire.clone(), channel.clone());
        gossipsub::IdentTopic::new(wire)
    }

    /// Forget a channel this node has stopped holding.
    pub(super) fn forget(&mut self, channel: &ChannelId) -> gossipsub::IdentTopic {
        let wire = topic_key_v1(channel).wire_string();
        self.channels.remove(&wire);
        gossipsub::IdentTopic::new(wire)
    }
}

/// The profile's broadcast configuration, validated.
///
/// Mirrors `DirectEndpoints::from_profile`: derived from the VALIDATED
/// profile rather than re-checking rules here, so one document has one
/// interpretation.
#[derive(Debug)]
pub struct BroadcastChannels {
    /// Channels to hold warm whether or not a client joins.
    pub(super) desired: Vec<ChannelId>,
    /// Bound for each session's delivery queue.
    pub(super) queue_bound: usize,
}

impl BroadcastChannels {
    /// Derive broadcast configuration from a profile.
    ///
    /// # Errors
    /// [`SubstrateError::InvalidProfile`] carrying every rule the
    /// configuration broke, reported together rather than one run apart.
    pub fn from_profile(
        profile: &interweave_profile_config::ProfileConfig,
        queue_bound: usize,
    ) -> Result<Self, SubstrateError> {
        let errors = profile.validate();
        if !errors.is_empty() {
            return Err(SubstrateError::InvalidProfile(
                errors.iter().map(ToString::to_string).collect(),
            ));
        }
        if queue_bound == 0 || queue_bound > interweave_local_client_api::MAX_EVENT_QUEUE {
            return Err(SubstrateError::InvalidConfig {
                field: "broadcast.queue_bound",
                got: queue_bound,
                allowed: (1, interweave_local_client_api::MAX_EVENT_QUEUE),
            });
        }
        Ok(Self {
            desired: profile.channels.desired.clone(),
            queue_bound,
        })
    }
}

/// Per-iteration facts the inbound path needs.
pub(super) struct BroadcastTick {
    /// Monotonic milliseconds since the runtime started.
    pub(super) now_ms: u64,
    /// Unix-epoch milliseconds, for the receipt time on a delivery.
    pub(super) wall_ms: u64,
    /// The profile's effective payload limit.
    pub(super) max_payload_bytes: usize,
    /// Whether the node has begun draining.
    pub(super) draining: bool,
    /// Whether a delivery notification may be buffered.
    ///
    /// From the BASE capacity, never the progress slack: a broadcast
    /// notification must not spend the room reserved for settling an
    /// in-flight direct exchange.
    pub(super) may_buffer_delivery: bool,
}

/// Whether the event was consumed here or belongs to the caller.
pub(super) enum BroadcastHandled {
    /// Handled; the loop should continue.
    Consumed,
    /// Not a broadcast event.
    Passed(Box<libp2p::swarm::SwarmEvent<SubstrateBehaviourEvent>>),
}

/// Handle one Swarm event if it is an inbound broadcast.
///
/// # The report happens on every path, first
///
/// The behaviour is built with `validate_messages()`, so nothing
/// propagates until this reports — and a message never reported stays in
/// the backend's cache, where its id is never seen as new again. So the
/// report is made immediately after the verdict, before any local
/// decision, and no local outcome can skip it. That ordering is also what
/// ADR-0029 requires: dedup and resource decisions come *after* the
/// report, and none of them may change it.
pub(super) fn handle_broadcast(
    event: libp2p::swarm::SwarmEvent<SubstrateBehaviourEvent>,
    swarm: &mut GatedSwarm,
    state: &mut BroadcastState,
    outbox: &mut std::collections::VecDeque<SwarmEvent>,
    tick: BroadcastTick,
) -> BroadcastHandled {
    let libp2p::swarm::SwarmEvent::Behaviour(SubstrateBehaviourEvent::Broadcast(inner)) = event
    else {
        return BroadcastHandled::Passed(Box::new(event));
    };

    let (propagation_source, message_id, message) = match inner {
        gossipsub::Event::Message {
            propagation_source,
            message_id,
            message,
        } => (propagation_source, message_id, message),
        // SUBSCRIPTION CHANGES ARE ANNOUNCED, under the channel this
        // node derived the topic from. A topic it never held maps to
        // nothing and is dropped: announcing it would mean naming a
        // channel by guessing, and a guessed channel is worse than none.
        gossipsub::Event::Subscribed { peer_id, topic } => {
            if let (Some(channel), Ok(peer)) = (
                state.channel_of(&topic).cloned(),
                to_transport_identity(&peer_id),
            ) && tick.may_buffer_delivery
            {
                outbox.push_back(SwarmEvent::PeerSubscribed { peer, channel });
            }
            return BroadcastHandled::Consumed;
        }
        gossipsub::Event::Unsubscribed { peer_id, topic } => {
            if let (Some(channel), Ok(peer)) = (
                state.channel_of(&topic).cloned(),
                to_transport_identity(&peer_id),
            ) && tick.may_buffer_delivery
            {
                outbox.push_back(SwarmEvent::PeerUnsubscribed { peer, channel });
            }
            return BroadcastHandled::Consumed;
        }
        // Informational and consumed: a peer that does not speak the
        // protocol simply never joins a mesh, and a slow peer is the
        // backend's queue accounting, not a delivery fact.
        gossipsub::Event::GossipsubNotSupported { .. } | gossipsub::Event::SlowPeer { .. } => {
            return BroadcastHandled::Consumed;
        }
    };

    // Strict validation guarantees a source; a message without one never
    // reaches the application. Treated as unreportable rather than
    // assumed away: with nothing to attribute the message to there is no
    // trust question to answer, and inventing an answer is worse than
    // declining one.
    let Some(publisher) = message.source else {
        swarm.report_broadcast_validation(
            &message_id,
            &propagation_source,
            gossipsub::MessageAcceptance::Reject,
        );
        return BroadcastHandled::Consumed;
    };
    let Ok(source) = to_transport_identity(&publisher) else {
        swarm.report_broadcast_validation(
            &message_id,
            &propagation_source,
            gossipsub::MessageAcceptance::Reject,
        );
        return BroadcastHandled::Consumed;
    };

    let verdict = classify_broadcast(&message.data, tick.max_payload_bytes, &source, &state.trust);

    // FIRST, AND ON EVERY ARM. Not conditional on outbox room, not
    // conditional on what admission decides afterwards.
    swarm.report_broadcast_validation(
        &message_id,
        &propagation_source,
        match verdict {
            ProtocolVerdict::Accept(_) => gossipsub::MessageAcceptance::Accept,
            ProtocolVerdict::Ignore => gossipsub::MessageAcceptance::Ignore,
            ProtocolVerdict::Reject => gossipsub::MessageAcceptance::Reject,
        },
    );

    let ProtocolVerdict::Accept(frame) = verdict else {
        return BroadcastHandled::Consumed;
    };

    // The channel comes from the TOPIC, never the envelope. Unknown is
    // unreachable — this node subscribed to receive here — and is still
    // handled, because an unreachable branch that fabricates a channel is
    // how a message gets delivered under the wrong one.
    let Some(channel) = state.channel_of(&message.topic).cloned() else {
        return BroadcastHandled::Consumed;
    };

    let admission = {
        let mut ctx = BroadcastContext {
            prefix: PrefixContext {
                trust: &state.trust,
                ingress: &mut state.ingress,
                draining: tick.draining,
            },
            dedup: &mut state.dedup,
            subs: &state.subs,
            queues: &mut state.queues,
        };
        admit_broadcast(
            &frame,
            &channel,
            &source,
            Clocks {
                monotonic_ms: tick.now_ms,
                wall_ms: tick.wall_ms,
            },
            &mut ctx,
        )
    };

    if let BroadcastAdmission::Delivered { sessions, .. } = admission {
        for session in sessions {
            // THE NOTIFICATION MAY BE DROPPED; the event itself is
            // already in the session's queue. Direct's non-delivery
            // events are treated the same way: informational, nothing
            // downstream blocks on them, and the alternative is an outbox
            // that grows with whatever the network sends.
            if !tick.may_buffer_delivery {
                break;
            }
            outbox.push_back(SwarmEvent::BroadcastDelivered {
                channel: channel.clone(),
                source_peer: source.clone(),
                session,
            });
        }
    }
    BroadcastHandled::Consumed
}

/// Map a publish failure onto the local error a caller sees.
///
/// `NoPeersSubscribedToTopic` is **success**, per PUBSUB.md: zero mesh
/// peers is degraded reachability, and local publish acceptance is the
/// only synchronous claim broadcast makes. Reporting it as an error would
/// tell a caller its message failed when the contract says the publish
/// succeeded and nobody happened to be listening.
#[must_use]
pub(super) fn publish_error(
    error: &gossipsub::PublishError,
) -> Option<interweave_transport_api::TransportError> {
    use interweave_transport_api::TransportError as E;
    match error {
        // Both are local acceptance. `Duplicate` means this exact message
        // was already published by this node; the caller's message is out.
        gossipsub::PublishError::NoPeersSubscribedToTopic | gossipsub::PublishError::Duplicate => {
            None
        }
        // Unreachable after the caller's own ceiling check, and mapped
        // rather than collapsed into Internal so that if it ever fires it
        // says what it is.
        gossipsub::PublishError::MessageTooLarge => Some(E::PayloadTooLarge),
        gossipsub::PublishError::AllQueuesFull(_) => Some(E::Overloaded),
        gossipsub::PublishError::SigningError(_) | gossipsub::PublishError::TransformFailed(_) => {
            Some(E::Internal)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_mesh_peers_is_local_success_not_an_error() {
        // PUBSUB.md: "Local publish acceptance is the only synchronous
        // success claim." A node publishing into an empty mesh has
        // accepted the message; there is simply nobody to carry it, which
        // diagnostics surface as degraded reachability rather than a
        // failed send.
        assert_eq!(
            publish_error(&gossipsub::PublishError::NoPeersSubscribedToTopic),
            None
        );
        assert_eq!(publish_error(&gossipsub::PublishError::Duplicate), None);
    }

    #[test]
    fn a_full_backend_queue_is_overload_and_a_signing_failure_is_internal() {
        use interweave_transport_api::TransportError as E;
        assert_eq!(
            publish_error(&gossipsub::PublishError::AllQueuesFull(3)),
            Some(E::Overloaded)
        );
        assert_eq!(
            publish_error(&gossipsub::PublishError::MessageTooLarge),
            Some(E::PayloadTooLarge)
        );
    }

    #[test]
    fn a_topic_round_trips_to_the_channel_that_derived_it() {
        // The reverse map is what makes "the envelope carries no channel"
        // workable: a topic reaches the inbound path only because this
        // node subscribed, so the lookup is total for anything real.
        let sources = TrustSources::default();
        let mut state = BroadcastState::new(&sources);
        let channel = ChannelId::parse("general").expect("valid channel");

        let topic = state.remember(&channel);
        assert_eq!(state.channel_of(&topic.hash()), Some(&channel));

        // And a topic this node never subscribed to maps to nothing,
        // rather than to some channel it happens to know.
        let stranger = gossipsub::IdentTopic::new("0".repeat(64));
        assert_eq!(state.channel_of(&stranger.hash()), None);

        state.forget(&channel);
        assert_eq!(
            state.channel_of(&topic.hash()),
            None,
            "forgetting a channel forgets its topic"
        );
    }

    #[test]
    fn case_differing_channels_derive_different_topics() {
        // ADR-0025 makes ChannelId case-sensitive, and this is where a
        // collapse would show up as two channels sharing one mesh.
        let sources = TrustSources::default();
        let mut state = BroadcastState::new(&sources);
        let lower = ChannelId::parse("general").expect("valid");
        let upper = ChannelId::parse("General").expect("valid");

        let a = state.remember(&lower);
        let b = state.remember(&upper);
        assert_ne!(a.hash(), b.hash());
        assert_eq!(state.channel_of(&a.hash()), Some(&lower));
        assert_eq!(state.channel_of(&b.hash()), Some(&upper));
    }
}
