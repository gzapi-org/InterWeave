// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The network behaviour: pre-auth admission, Identify, direct v2
//! and signed GossipSub.
//!
//! One behaviour, deliberately. Every additional protocol here is a
//! protocol that starts doing things on its own — Kademlia dials to fill
//! buckets, AutoNAT probes, Relay renews reservations — and each of
//! those is an outbound dial that must already be passing the root
//! admission gate before it exists (CLAUDE.md §3). Identify does not
//! originate dials; it answers on connections that already exist, which
//! is why it is the one that can be here now.

// The `NetworkBehaviour` derive generates `SubstrateBehaviourEvent` as a
// sibling item, and its variants carry no documentation the derive could
// have written. The allowance is scoped to THIS module — which holds
// nothing but the behaviour and its constructor, both documented — rather
// than to the crate, so every hand-written type elsewhere still has to
// document itself.
#![allow(missing_docs, reason = "variants of the derive-generated event enum")]

use std::time::Duration;

use libp2p::gossipsub;
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::NetworkBehaviour;
use libp2p::{identify, identity};

use interweave_transport_api::{MAX_PAYLOAD_BYTES, broadcast_v1};
use interweave_transport_runtime::mesh_id::gossipsub_message_id_v1;
use interweave_transport_runtime::preauth::PreAuthLimits;

use crate::direct_codec::{DIRECT_PROTOCOL, DirectCodec};
use crate::outbound_gate::OutboundAdmission;
use crate::preauth_gate::PreAuthAdmission;

/// The total deadline for one direct exchange (`DIRECT.md`).
///
/// Ten seconds, and it is the REQUESTER's patience rather than a promise
/// about the responder: SPIKE-002 finding 1 showed that when both sides
/// time out the attribution is a race, so this bounds how long a caller
/// waits and nothing more.
pub const DIRECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The Identify protocol name this profile advertises.
///
/// Namespaced under `interweave` per ADR-0047, and versioned so a future
/// change is a new string rather than a silent reinterpretation.
pub const IDENTIFY_PROTOCOL: &str = "/interweave/id/1.0.0";

/// What the signed GossipSub RPC adds around one application envelope.
///
/// `max_transmit_size` bounds the ENCODED RPC, not `message.data`. A
/// ceiling sized for the envelope alone therefore refuses the largest
/// LEGAL broadcast, because the signed RPC also carries the publisher's
/// PeerId, the sequence number, the topic string, an Ed25519 signature,
/// the publisher's public key, and protobuf tags and length prefixes for
/// all of it.
///
/// Sized generously rather than exactly, because every term is a foreign
/// encoding this crate does not control: a multihash whose length depends
/// on the key type, a protobuf varint whose width depends on the value,
/// and a topic string this node derives but the backend frames. An exact
/// figure would be a re-derivation of someone else's format that goes
/// silently wrong when it changes; 512 bytes is far above the ~250 the
/// current terms occupy and far below anything that would let an
/// oversized envelope through, since the envelope's own limit is
/// enforced separately by the decoder.
const GOSSIPSUB_RPC_OVERHEAD: usize = 512;

/// The largest GossipSub RPC this node will send or accept.
///
/// The payload ceiling, plus the envelope's fixed maximum overhead, plus
/// the RPC framing above — sized deliberately rather than left at the
/// backend's default. Too LOW and the largest legal broadcast cannot be
/// sent at all; too HIGH and a peer can make this node buffer a frame in
/// full that the envelope decoder must then refuse.
///
/// The envelope limit is still enforced on its own by `decode`, so this
/// ceiling being generous does not widen what the application accepts.
/// PUBSUB.md states the same arithmetic.
pub const MAX_BROADCAST_TRANSMIT: usize =
    MAX_PAYLOAD_BYTES + broadcast_v1::MAX_FRAME_OVERHEAD + GOSSIPSUB_RPC_OVERHEAD;

/// The frozen mesh duplicate identity of one GossipSub message.
///
/// A named function rather than a closure so it can be tested against
/// `fixtures/gossipsub/gossipsub-message-id-v1.json` without a Swarm.
/// The adapter is where the composition can go wrong — reading the wrong
/// fields — and the closure form put it somewhere no test could reach.
///
/// **It reads only transport metadata.** `message.data` carries the
/// InterWeave envelope and is deliberately not an input: PUBSUB.md makes
/// it a MUST that the mesh key does not depend on the application
/// envelope's `message_id`, because two publishers may legitimately
/// choose the same 128 bits and a mesh that collapsed them would drop a
/// message nobody sent twice.
fn mesh_message_id(message: &gossipsub::Message) -> gossipsub::MessageId {
    // Strict validation guarantees both are present for any message that
    // reaches the application; see `validation_mode` where this is
    // installed. The fallbacks are unreachable rather than meaningful,
    // and are chosen so an impossible message hashes to something rather
    // than panicking inside the backend's own poll.
    let source = message.source.map(|p| p.to_bytes()).unwrap_or_default();
    let id = gossipsub_message_id_v1(&source, message.sequence_number.unwrap_or(0));
    gossipsub::MessageId::new(id.as_bytes())
}

/// The Stage 4 behaviour, plus the gate that decides who may begin.
#[derive(NetworkBehaviour)]
pub struct SubstrateBehaviour {
    /// Pre-Noise admission for inbound connections.
    ///
    /// FIRST, and the order is not cosmetic: the derive calls each
    /// field's `handle_pending_inbound_connection` in declaration
    /// order and stops at the first `Err`, so a denial here costs
    /// nothing further. It is also the field that must exist before
    /// any behaviour that dials, which is why it lands with Stage 5
    /// rather than with the first behaviour that needs it.
    pub preauth: PreAuthAdmission,
    /// The gate every outbound dial passes, including a behaviour's.
    ///
    /// Present before any behaviour that dials exists, which is the
    /// order CLAUDE.md §3 requires: the funnel is green first, and
    /// Kademlia is added to a Swarm that already refuses an
    /// unadmitted dial.
    pub outbound: OutboundAdmission,
    /// Peer metadata exchange on an already-established connection.
    pub identify: identify::Behaviour,
    /// Directed messaging, `/interweave/direct/2.0.0`.
    ///
    /// LAST, and after both gates, because the derive calls each field's
    /// handlers in declaration order. This behaviour originates outbound
    /// dials when a caller sends to a peer it is not connected to, so it
    /// is added to a Swarm where `outbound` already refuses an unadmitted
    /// dial and `preauth` already answers before Noise — the ordering
    /// CLAUDE.md §3 requires, and the reason Stage 5 had to be green
    /// before this field could exist at all.
    pub direct: request_response::Behaviour<DirectCodec>,
    /// Signed broadcast, GossipSub over hashed topics.
    ///
    /// LAST for the same reason `direct` is late, though for a weaker
    /// reason than `direct` has: this behaviour originates NO dial of its
    /// own. It acts only on connections the swarm already established,
    /// and nothing here calls `add_explicit_peer`, which is the one API
    /// that would make it dial. It is placed after both gates anyway
    /// because the ordering rule is about where a behaviour sits relative
    /// to the funnel, not about whether today's configuration happens to
    /// exercise it.
    ///
    /// What it DOES need is the trust class kept in sync: it performs no
    /// connection admission at all, so an untrusted peer never reaches it
    /// only because the gated swarm refused the connection first.
    pub broadcast: gossipsub::Behaviour,
}

impl SubstrateBehaviour {
    /// Build the behaviour for `keypair`.
    ///
    /// Takes the whole keypair rather than the public key alone because
    /// GossipSub signs every message this node publishes: PUBSUB.md
    /// requires signed messages and strict validation, and
    /// `MessageAuthenticity::Signed` is what binds the author and
    /// sequence number the frozen mesh id is computed from.
    ///
    /// # Errors
    /// Returns the backend's own message if the GossipSub configuration
    /// is rejected — which it is, at construction, when authenticity and
    /// validation mode disagree. That is a build-time contradiction
    /// rather than a runtime condition, and it is propagated rather than
    /// unwrapped so a future edit that introduced one fails to start
    /// instead of panicking in a task.
    pub fn new(keypair: &identity::Keypair, preauth: PreAuthLimits) -> Result<Self, &'static str> {
        let broadcast_config = gossipsub::ConfigBuilder::default()
            // STRICT, which is what makes the mesh id computable at all:
            // it guarantees every message reaching the application has an
            // authenticated `source` and a `sequence_number`, the two
            // inputs GossipSubMessageIdV1 binds. Anything weaker admits a
            // message with neither.
            .validation_mode(gossipsub::ValidationMode::Strict)
            // MANUAL REPORTING. Without this the backend forwards on its
            // own and ADR-0029's Accept/Ignore/Reject mapping has nowhere
            // to happen. With it, every message MUST be reported exactly
            // once or it stays in the backend's cache forever.
            .validate_messages()
            .message_id_fn(mesh_message_id)
            .max_transmit_size(MAX_BROADCAST_TRANSMIT)
            .build()
            .map_err(|_| "the GossipSub configuration is not buildable")?;

        Ok(Self {
            preauth: PreAuthAdmission::new(preauth),
            outbound: OutboundAdmission::default(),
            identify: identify::Behaviour::new(identify::Config::new(
                IDENTIFY_PROTOCOL.to_owned(),
                keypair.public(),
            )),
            direct: request_response::Behaviour::with_codec(
                DirectCodec,
                // FULL, because a profile both sends and receives directed
                // messages. Inbound-only would make this peer unable to
                // initiate, which is not a security posture — an
                // unauthorized peer is refused by trust, not by declining
                // to speak.
                [(DIRECT_PROTOCOL, ProtocolSupport::Full)],
                request_response::Config::default().with_request_timeout(DIRECT_TIMEOUT),
            ),
            broadcast: gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(keypair.clone()),
                broadcast_config,
            )?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::str::FromStr;

    use libp2p::PeerId;

    /// The repository zero-seed publisher, from the frozen vectors.
    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    fn message(peer: &str, sequence: u64, data: &[u8]) -> gossipsub::Message {
        gossipsub::Message {
            source: Some(PeerId::from_str(peer).expect("valid peer id")),
            data: data.to_vec(),
            sequence_number: Some(sequence),
            topic: gossipsub::TopicHash::from_raw("t"),
        }
    }

    #[test]
    fn the_mesh_id_is_the_frozen_golden_for_the_zero_seed_publisher() {
        // PUBSUB.md's golden, reproduced through the composition rather
        // than through the derivation alone: this is what proves the
        // adapter reads the fields the algorithm is defined over.
        let id = mesh_message_id(&message(P1, 0, b"anything"));
        let hex: String = id.0.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "7f037dd538d9cccfb1949ca26b875c469173e6b248f1b68553ccaeb16bf9cf89",
            "the composed message_id_fn must reproduce the frozen golden"
        );
    }

    #[test]
    fn the_envelope_bytes_are_not_an_input() {
        // The MUST. Two messages differing only in payload -- which is
        // where the application envelope and its own message_id live --
        // must share a mesh id, or the mesh key depends on application
        // serialization.
        assert_eq!(
            mesh_message_id(&message(P1, 4, b"one body")),
            mesh_message_id(&message(P1, 4, b"a completely different body")),
        );
    }

    #[test]
    fn two_publishers_at_one_sequence_number_do_not_collide() {
        assert_ne!(
            mesh_message_id(&message(P1, 0, b"same")),
            mesh_message_id(&message(P2, 0, b"same")),
        );
    }

    #[test]
    fn one_publisher_at_two_sequence_numbers_does_not_collide() {
        assert_ne!(
            mesh_message_id(&message(P1, 0, b"same")),
            mesh_message_id(&message(P1, 1, b"same")),
        );
    }

    #[test]
    fn the_transmit_ceiling_leaves_rpc_room_above_a_maximum_envelope() {
        // Sized from the envelope rather than left at the backend's
        // default: a larger ceiling buffers frames the decoder must then
        // refuse, and a smaller one refuses legal maximum-size messages
        // as though the network had failed.
        //
        // Asserted by ENCODING one rather than by restating the
        // arithmetic. A test that compared the constant to its own
        // definition would agree with any miscalculation of the overhead,
        // which is the only thing here that can be wrong.
        let widest = interweave_transport_api::BroadcastMessageV1 {
            message_id: interweave_transport_api::MessageId::from_bytes([0xab; 16]),
            sent_at_ms: u64::MAX,
            payload: interweave_transport_api::Payload::at_ceiling(
                Some(
                    interweave_transport_api::MediaType::parse(
                        "a".repeat(interweave_transport_api::MAX_MEDIA_TYPE_BYTES),
                    )
                    .expect("a maximum-length media type"),
                ),
                vec![0u8; MAX_PAYLOAD_BYTES],
            )
            .expect("a maximum payload"),
        };

        let encoded = widest.encode();
        // The ceiling is NOT the envelope maximum: it is that plus room
        // for the RPC the backend wraps around it. Asserting equality
        // here is what the ceiling looked like when it was wrong, and
        // the assertion passed for exactly as long as the bug existed.
        assert_eq!(
            encoded.len(),
            MAX_PAYLOAD_BYTES + broadcast_v1::MAX_FRAME_OVERHEAD,
            "the envelope's own maximum is its declared fixed overhead"
        );
        assert!(
            encoded.len() + GOSSIPSUB_RPC_OVERHEAD <= MAX_BROADCAST_TRANSMIT,
            "and the ceiling leaves the whole RPC allowance above it"
        );
    }
}
