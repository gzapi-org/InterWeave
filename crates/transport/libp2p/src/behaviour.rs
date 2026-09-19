// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The network behaviour: pre-auth admission, Identify, direct v2
//! and signed GossipSub.
//!
//! One behaviour, deliberately. Every additional protocol here is a
//! protocol that starts doing things on its own — Kademlia dials to fill
//! buckets, the AutoNAT SERVER dials back, Relay renews reservations —
//! and each of those is an outbound dial that must already be passing
//! the root admission gate before it exists (CLAUDE.md §3). (The AutoNAT
//! CLIENT is not on that list: a probe is a request over a connection
//! already open, and it emits no dial — CLAUDE.md §1, pinned against the
//! vendored source by `tests/autonat_client_retest.rs`.) Kademlia is here NOW
//! because Stage 10 satisfied that order: the outbound gate admits
//! behaviour-originated dials by root policy, and it landed — tested —
//! before the `kad` feature entered the workspace manifest.

// The `NetworkBehaviour` derive generates `SubstrateBehaviourEvent` as a
// sibling item, and its variants carry no documentation the derive could
// have written. The allowance is scoped to THIS module — which holds
// nothing but the behaviour and its constructor, both documented — rather
// than to the crate, so every hand-written type elsewhere still has to
// document itself.
#![allow(missing_docs, reason = "variants of the derive-generated event enum")]

use std::time::Duration;

use libp2p::autonat;
use libp2p::gossipsub;
use libp2p::kad;
use libp2p::kad::store::MemoryStore;
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::NetworkBehaviour;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::{identify, identity};

use interweave_transport_api::{MAX_PAYLOAD_BYTES, broadcast_v1};
use interweave_transport_runtime::SnapshotHandle;
use interweave_transport_runtime::mesh_id::gossipsub_message_id_v1;
use interweave_transport_runtime::preauth::PreAuthLimits;

use crate::attribution::Attributing;
use crate::candidate_scope::ScopedCandidates;
use crate::class_gate::ClassGated;
use crate::direct_codec::{DIRECT_PROTOCOL, DirectCodec};
use crate::endpoints_codec::{ENDPOINTS_PROTOCOL, EndpointsCodec};
use crate::outbound_gate::OutboundAdmission;
use crate::preauth_gate::PreAuthAdmission;

/// The total deadline for one direct exchange (`DIRECT.md`).
///
/// Ten seconds, and it is the REQUESTER's patience rather than a promise
/// about the responder: SPIKE-002 finding 1 showed that when both sides
/// time out the attribution is a race, so this bounds how long a caller
/// waits and nothing more.
pub const DIRECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one directory exchange may take.
///
/// Shorter than direct: the answer is a snapshot the responder already
/// holds, and a slow one is a slow peer rather than a slow decision.
const ENDPOINTS_TIMEOUT: Duration = Duration::from_secs(5);

/// The `protocol_version` string this profile puts in its Identify
/// payload.
///
/// **Not a protocol name, despite looking exactly like one.**
/// `identify::Config::new` takes a `protocol_version`, which travels
/// inside the Identify payload as metadata a peer may read; the
/// protocols actually negotiated are libp2p's own hardcoded
/// `/ipfs/id/1.0.0` and `/ipfs/id/push/1.0.0`
/// (`libp2p-identify-0.48.0` `protocol.rs:35,37`). Setting this changes
/// what a peer is TOLD, never what is spoken, and this node advertises
/// no protocol under the `interweave` namespace for Identify.
///
/// This constant was called `IDENTIFY_PROTOCOL` and documented as "the
/// Identify protocol name this profile advertises" from Stage 4 until
/// Stage 11. Nothing was ever wrong with the CODE — every use passes it
/// where a `protocol_version` belongs, and `two_peers.rs` reads it back
/// off the `Identified` event's `protocol_version` field. Only the name
/// and the sentence were wrong, and they were wrong in the direction
/// that costs something: a test written from them asserted
/// `/interweave/id/1.0.0` in the advertised protocol set, where it has
/// never appeared.
///
/// Namespaced under `interweave` per ADR-0047, and versioned so a future
/// change is a new string rather than a silent reinterpretation.
pub const IDENTIFY_PROTOCOL_VERSION: &str = "/interweave/id/1.0.0";

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
    ///
    /// AND BEFORE ANY FIELD WHOSE ESTABLISHED HOOK COULD DENY. The gate
    /// tells a dial the pool failed from one the Swarm failed before
    /// dialling by whether it re-bound the ticket at its established
    /// hook (`outbound_gate.rs`, "A dial that fails between the hook
    /// and the socket"); a field before it denying there would hand
    /// the gate a pool failure with a placeholder ticket -- released
    /// once still, but labelled as the other case. `preauth` has no
    /// outbound denial, and nothing else precedes this field.
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
    pub direct: ClassGated<request_response::Behaviour<DirectCodec>>,
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
    pub broadcast: ClassGated<gossipsub::Behaviour>,
    /// The endpoint directory, `/interweave/endpoints/1.0.0` (ADR-0031).
    ///
    /// After both gates for the same reason `direct` is: `send_request`
    /// dials an unconnected peer, so this behaviour sits in a Swarm where
    /// an unadmitted dial is already refused — and `GatedSwarm::
    /// query_endpoints` refuses to call it on an unconnected peer at all,
    /// so the gate is the second line and not the first.
    pub endpoints: ClassGated<request_response::Behaviour<EndpointsCodec>>,
    /// Kademlia peer routing (ADR-0009), present only when configured.
    ///
    /// LAST, after both gates, and the strongest instance of the
    /// ordering rule: this is the first behaviour that dials
    /// AUTONOMOUSLY — an iterative query asks the Swarm to dial with no
    /// caller anywhere — so it joins a Swarm whose outbound gate
    /// already decides such dials by root policy, under the origin the
    /// wrapper announces. `Toggle` rather than
    /// an always-on field because a profile without a kademlia entry
    /// must not even advertise the protocol (§13: `enabled: false`
    /// means zero activity).
    /// Wrapped so the gate is TOLD this is a Kademlia query rather
    /// than inferring it. Stage 10 could infer it — Kademlia was the
    /// only dialling behaviour compiled — and Stage 11 adds three more,
    /// at which point the inference refuses every relay reservation and
    /// AutoNAT dial-back against the infrastructure the stack needs
    /// (SPIKE-004 F1, measured). The wrapper decides nothing; it writes
    /// `ConnectionId -> DialOrigin` before the Swarm acts on the dial.
    pub kad: ClassGated<Toggle<Attributing<kad::Behaviour<MemoryStore>>>>,
    /// The AutoNAT v2 client (ADR-0035, `AUTONAT.md`), present only
    /// when configured.
    ///
    /// NOT `Attributing`, because it never dials: every `ToSwarm` it
    /// emits is a confirmation, an event or a handler notification, and
    /// the dial in AutoNAT is the server's dial-back (CLAUDE.md §1;
    /// pinned against the vendored source by
    /// `tests/autonat_client_retest.rs`). Wrapping it would announce an
    /// origin for a dial that never happens.
    ///
    /// NOT `ClassGated`, because it is a CONTROL protocol and not the
    /// data plane: an infrastructure-only server must be offered
    /// `/libp2p/autonat/2/dial-request` on the connection this profile
    /// dialled it on, and must be able to open `/libp2p/autonat/2/
    /// dial-back` on the inbound it answers with. `transport/libp2p/
    /// CONNECTIVITY.md`'s matrix grants that class exactly those.
    ///
    /// Wrapped in `ScopedCandidates` instead, which is the boundary this
    /// behaviour actually needs: what it may probe (§6) and who may say
    /// an address is confirmed (§5). `Toggle`, like `kad`, because a
    /// profile without a client must not advertise the protocol.
    pub autonat_client: Toggle<ScopedCandidates<autonat::v2::client::Behaviour>>,
    /// The AutoNAT v2 SERVER (`AUTONAT.md` §7), present only when
    /// configured -- the owner's 2026-09-07 ruling, gated off.
    ///
    /// `Attributing`, unlike the client, because the server DIALS: the
    /// dial-back at `v2/server/behaviour.rs:124` is the one dial in
    /// AutoNAT v2, and it reaches the outbound gate announced as
    /// `AutonatProbe` -- CLAUDE.md §1's route 1, reached here for the
    /// first time -- so the root policy admits or refuses it like any
    /// other behaviour dial (SPIKE-004 R4.4/R4.8). Beneath that,
    /// `ProbeServer` carries §7's dial-back target rule and budgets,
    /// which the crate does not (F2); its target refusal is a pending-
    /// hook denial AFTER the gate's, and the gate takes its ticket back
    /// on that (`outbound_gate.rs`, "A dial that fails between the hook
    /// and the socket"), which is what lets this field sit where every
    /// dialling behaviour sits: after the gates.
    ///
    /// `ClassGated` for the INFRASTRUCTURE service, not the data-plane
    /// one: §7 serves probes to both authorized classes, so an
    /// infrastructure-only peer is offered the dial-request protocol
    /// here while it is still offered nothing above.
    pub autonat_server: crate::runtime::autonat_server_driver::ServerField,
    /// The Circuit Relay v2 CLIENT (`RELAY.md`), present only when
    /// configured -- the owner's 2026-09-07 ruling, gated off -- and
    /// then only together with the relay TRANSPORT the Swarm builder
    /// composes beside it (`with_relay_client`): the behaviour half
    /// cannot exist without the transport half, and neither exists
    /// for a profile that reserves on no relay.
    ///
    /// `Attributing`, because the client DIALS: the control connection
    /// to a relay it holds none to (`priv_client.rs`, the `ListenReq`
    /// arm) reaches the outbound gate announced as `RelayReservation`
    /// -- CLAUDE.md §1's route 1 -- and the root policy admits or
    /// refuses it like any other behaviour dial. Beneath that,
    /// `ReservationScope` swallows the crate's own
    /// `ExternalAddrConfirmed`, so what the Swarm advertises is the
    /// reservation manager's set and nothing the crate decided (§5).
    ///
    /// `ClassGated` for the INFRASTRUCTURE service: the stop protocol
    /// -- a relay handing this profile an inbound circuit -- is offered
    /// to both authorized classes and to nobody else, so a peer in no
    /// trust set cannot open one.
    pub relay_client: crate::runtime::relay_driver::ClientField,
    /// The Circuit Relay v2 SERVER (`RELAY.md` §8), present only when
    /// configured -- the owner's 2026-09-07 ruling, gated off.
    ///
    /// NOT `Attributing`: the server dials nothing. A reservation rides
    /// the requester's inbound, and a circuit's far end is reached over
    /// the connection the destination already holds to this relay.
    /// `ClassGated` for the INFRASTRUCTURE service, so the hop protocol
    /// is offered to the two authorized classes and to nobody else --
    /// which is §8's service admission, and the whole of it: a peer in
    /// no trust set cannot ask.
    pub relay_server: crate::runtime::relay_server_driver::ServerField,
    /// DCUtR (`DCUTR.md`), present only when configured -- the owner's
    /// 2026-09-07 ruling, gated off -- and then under `HolePunchScope`,
    /// which is §13's attempt lifecycle the crate lacks (SPIKE-004:
    /// "one attempt is not one dial").
    ///
    /// `Attributing`, because a punch DIALS at both ends: every such
    /// dial reaches the outbound gate announced as `DcutrHolePunch`
    /// (R12.4) and the root policy judges its destination, an
    /// infrastructure-only one refused (D1). `ClassGated` for the
    /// DATA-PLANE service, unlike the three above: DCUtR upgrades an
    /// application path, so a non-data-plane peer is offered no DCUtR
    /// handler and no attempt ever begins toward it (§2).
    pub dcutr: crate::runtime::dcutr_driver::DcutrField,
}

// EVERY DATA-PLANE BEHAVIOUR ABOVE IS WRAPPED IN `ClassGated`, and that
// is what makes §14's protocol-isolation invariant true rather than
// merely intended.
//
// The invariant is about EXPOSURE, not authority. Authority was already
// refused at four entry points — direct ingress, the GossipSub publisher
// check, `endpoints::build_answer`, and the Kademlia driver's
// `try_admit` — and an implementer who checked only those would find the
// invariant apparently met. What was NOT met was the other half: these
// behaviours used to be installed uniformly on every connection, so an
// infrastructure-only peer was advertised their protocols and could open
// their substreams, and a refusal cost a parse and an accounting charge
// rather than a closed stream. `build_answer`'s pre-trust rate budget
// exists because that is where the exposure used to land.
//
// FOUR, NOT THREE. `kad` is in the wrapped set with `direct`,
// `broadcast` and `endpoints`. Its authority check is `try_admit`'s
// data-plane trust requirement, so such a peer held no routing seat —
// but it could still open the DHT substream and be answered, which is
// the same exposure. An implementer working from a list of three would
// have restricted three and left this one.
//
// Measured rather than argued: retaining an infrastructure-only inbound
// and reading its Identify gave the seven advertised names before this
// wrapper and gives `/ipfs/id/1.0.0` and `/ipfs/id/push/1.0.0` after —
// Identify alone, which is what `transport/libp2p/CONNECTIVITY.md`'s
// protocol matrix grants that class.
//
// WHAT THIS DOES NOT DO, stated because a reader will otherwise assume
// it. A connection's handler is built once, at establishment, and libp2p
// never rebuilds it. So the two directions are not symmetric:
//
// - a peer DEMOTED while connected would otherwise keep every
//   data-plane handler for the connection's life. `connections_to_close`
//   ends it, so the closure lands in `set_trust`'s ADR-0012 count;
// - a peer PROMOTED while connected would leave the peer holding a
//   `Denied` handler beside a later `Allowed` one, which is a pair
//   `NotifyHandler::Any` can route a `kad` query into and lose.
//   `ClassGated::poll` ends that one, since a promotion is not a
//   revocation and is not part of the count.
//
// Both compare against the class the connection was ADMITTED under, so
// a connection that has carried a denying handler all along is left
// alone and its origin still decides. `class_gate.rs` and `dialing.rs`
// pin all of it.

/// The behaviours that exist only when configured, handed to
/// [`SubstrateBehaviour::new`] together: each is a `Toggle`, `None`
/// for a profile that did not configure it (the owner's 2026-09-07
/// ruling for the connectivity three; §13 for Kademlia).
pub struct Configured {
    /// Kademlia, wrapped in `Attributing` under `KademliaQuery`.
    pub kad: Toggle<Attributing<kad::Behaviour<MemoryStore>>>,
    /// The AutoNAT v2 client under its candidate scope.
    pub autonat_client: Toggle<ScopedCandidates<autonat::v2::client::Behaviour>>,
    /// The AutoNAT v2 server field.
    pub autonat_server: crate::runtime::autonat_server_driver::ServerField,
    /// The relay client field.
    pub relay_client: crate::runtime::relay_driver::ClientField,
    /// The relay server field.
    pub relay_server: crate::runtime::relay_server_driver::ServerField,
    /// The DCUtR field.
    pub dcutr: crate::runtime::dcutr_driver::DcutrField,
}

impl Default for Configured {
    /// Nothing configured: every toggle off.
    fn default() -> Self {
        Self {
            kad: Toggle::from(None),
            autonat_client: Toggle::from(None),
            autonat_server: Toggle::from(None),
            relay_client: Toggle::from(None),
            relay_server: Toggle::from(None),
            dcutr: Toggle::from(None),
        }
    }
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
    pub fn new(
        keypair: &identity::Keypair,
        preauth: PreAuthLimits,
        outbound: OutboundAdmission,
        configured: Configured,
        policy: SnapshotHandle,
    ) -> Result<Self, &'static str> {
        let Configured {
            kad,
            autonat_client,
            autonat_server,
            relay_client,
            relay_server,
            dcutr,
        } = configured;
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
            outbound,
            identify: identify::Behaviour::new(identify::Config::new(
                IDENTIFY_PROTOCOL_VERSION.to_owned(),
                keypair.public(),
            )),
            direct: ClassGated::new(
                request_response::Behaviour::with_codec(
                    DirectCodec,
                    // FULL, because a profile both sends and receives directed
                    // messages. Inbound-only would make this peer unable to
                    // initiate, which is not a security posture — an
                    // unauthorized peer is refused by trust, not by declining
                    // to speak.
                    [(DIRECT_PROTOCOL, ProtocolSupport::Full)],
                    request_response::Config::default().with_request_timeout(DIRECT_TIMEOUT),
                ),
                policy.clone(),
            ),
            broadcast: ClassGated::new(
                gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(keypair.clone()),
                    broadcast_config,
                )?,
                policy.clone(),
            ),
            endpoints: ClassGated::new(
                request_response::Behaviour::with_codec(
                    EndpointsCodec,
                    // FULL: a profile both asks and answers. Whether it
                    // ANSWERS is the runtime's decision per query, not a
                    // protocol it declines to speak — an unauthorized or
                    // disabled directory is a refusal frame, so the asker
                    // learns "no" rather than "no such protocol".
                    [(ENDPOINTS_PROTOCOL, ProtocolSupport::Full)],
                    request_response::Config::default().with_request_timeout(ENDPOINTS_TIMEOUT),
                ),
                policy.clone(),
            ),
            kad: ClassGated::new(kad, policy),
            autonat_client,
            autonat_server,
            relay_client,
            relay_server,
            dcutr,
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
