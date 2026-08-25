// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Experiment B: GossipSub duplicate-cache and authenticity ordering.
//!
//! `PUBSUB.md` requires an implementation to verify, against the exact
//! target rust-libp2p version, that an invalid signed-source/sequence
//! claim cannot create a lasting duplicate-cache entry which suppresses
//! a later valid message with the same mesh id. That is the second
//! experiment here; the first is the simpler claim that two authenticated
//! publishers reusing one application-envelope message id stay distinct
//! on the mesh.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use sha2::{Digest, Sha256};

use futures::StreamExt;
use libp2p::gossipsub::{self, IdentTopic, MessageAuthenticity, MessageId, ValidationMode};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, Swarm, noise, tcp, yamux};

use crate::direct::note;

#[derive(NetworkBehaviour)]
struct Behaviour {
    gossipsub: gossipsub::Behaviour,
}

/// How a mesh id is computed for the experiment.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IdRule {
    /// `GossipSubMessageIdV1` ITSELF, not its shape.
    ///
    /// The first version of this experiment hashed nothing: it returned
    /// raw `source || u64be(sequence)`. That separates two publishers,
    /// which made B1 pass, but it is not the frozen function -- so the
    /// pass said nothing about the calculation Stage 7 will ship.
    SourceAndSequence,
    /// PAYLOAD-DERIVED, and only for B2. See the note there: the public
    /// API does not let a caller choose a sequence number, so a
    /// source+sequence collision between a forged message and a genuine
    /// one cannot be arranged through it. Deriving the id from the
    /// payload forces exactly the collision the ordering question is
    /// about, and changes nothing else about the receive path.
    PayloadDerived,
}

fn config(rule: IdRule) -> gossipsub::Config {
    config_with(rule, ValidationMode::Strict)
}

/// `validation` is the LOCAL node's own mode.
///
/// The forger needs `Permissive` for itself, because the library refuses
/// to build a node that publishes unsigned while requiring signatures on
/// receipt -- a good refusal, and one that says something about the
/// design: unsigned publishing is a whole-node posture, not a per-message
/// choice. The RECEIVER stays `Strict`, which is the mode under test.
fn config_with(rule: IdRule, validation: ValidationMode) -> gossipsub::Config {
    let mut builder = gossipsub::ConfigBuilder::default();
    builder
        .validation_mode(validation)
        .heartbeat_interval(Duration::from_millis(200));
    match rule {
        IdRule::SourceAndSequence => {
            builder.message_id_fn(|message: &gossipsub::Message| {
                MessageId::from(
                    gossipsub_message_id_v1(
                        message.source.as_ref(),
                        message.sequence_number.unwrap_or(0),
                    )
                    .to_vec(),
                )
            });
        }
        IdRule::PayloadDerived => {
            builder.message_id_fn(|message: &gossipsub::Message| {
                let mut hasher = DefaultHasher::new();
                message.data.hash(&mut hasher);
                MessageId::from(hasher.finish().to_be_bytes().to_vec())
            });
        }
    }
    builder.build().expect("gossipsub config")
}

/// `GossipSubMessageIdV1`, exactly as `PUBSUB.md` freezes it.
///
/// ```text
/// domain    = UTF8("interweave/gossipsub-message-id/v1\0")
/// canonical = domain || u16be(len(source)) || source || u64be(sequence)
/// id        = SHA-256(canonical)
/// ```
///
/// Copied here rather than imported because no production crate exposes
/// it yet -- GossipSub belongs to Stage 7. The golden vector below is
/// what keeps the copy honest.
fn gossipsub_message_id_v1(source: Option<&PeerId>, sequence: u64) -> [u8; 32] {
    const DOMAIN: &[u8] = b"interweave/gossipsub-message-id/v1\0";
    let source_bytes = source.map_or_else(Vec::new, |p| p.to_bytes());
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update(
        u16::try_from(source_bytes.len())
            .unwrap_or(u16::MAX)
            .to_be_bytes(),
    );
    hasher.update(&source_bytes);
    hasher.update(sequence.to_be_bytes());
    hasher.finalize().into()
}

/// The copy above against the repository's frozen vectors.
///
/// A spike that reimplements a frozen calculation and does not check it
/// is a spike measuring its own reimplementation.
pub fn b0_message_id_matches_the_golden_vectors() {
    // fixtures/gossipsub/gossipsub-message-id-v1.json, also quoted in
    // PUBSUB.md.
    const ZERO_SEED_PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    let vectors: [(u64, &str); 3] = [
        (
            0,
            "7f037dd538d9cccfb1949ca26b875c469173e6b248f1b68553ccaeb16bf9cf89",
        ),
        (
            1,
            "daa108a21185fe3cd017c553e3041986ae124061356366be4cc7105fa28182df",
        ),
        (
            u64::MAX,
            "1eaa16ade59e3214aa5080e1bce06cae5e27733f823081ee26a9d9bfae3aabb0",
        ),
    ];
    let peer: PeerId = ZERO_SEED_PEER.parse().expect("canonical peer id");
    let mut all = true;
    for (sequence, expected) in vectors {
        let got = gossipsub_message_id_v1(Some(&peer), sequence);
        let hex = got.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let ok = hex == expected;
        all &= ok;
        note(
            &format!("sequence {sequence} matches the frozen vector"),
            ok,
        );
    }
    note("the id function under test IS GossipSubMessageIdV1", all);
}

fn node(
    authenticity: MessageAuthenticity,
    rule: IdRule,
    validation: ValidationMode,
) -> Swarm<Behaviour> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("tcp")
        .with_behaviour(|_| Behaviour {
            gossipsub: gossipsub::Behaviour::new(authenticity, config_with(rule, validation))
                .expect("gossipsub behaviour"),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
        .build()
}

/// A node that signs as itself.
fn signing_node(rule: IdRule) -> Swarm<Behaviour> {
    let swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("tcp");
    // The keypair is needed by `MessageAuthenticity::Signed`, and the
    // builder does not hand it back, so it is rebuilt here from a fresh
    // identity that the Swarm then adopts.
    let keypair = libp2p::identity::Keypair::generate_ed25519();
    let _ = swarm;
    libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("tcp")
        .with_behaviour(|_| Behaviour {
            gossipsub: gossipsub::Behaviour::new(
                MessageAuthenticity::Signed(keypair),
                config(rule),
            )
            .expect("gossipsub behaviour"),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
        .build()
}

async fn listen(swarm: &mut Swarm<Behaviour>) -> Multiaddr {
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("addr"))
        .expect("listen");
    loop {
        if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
            return address;
        }
    }
}

/// Drive every node for `how_long`, collecting messages the receiver got.
async fn pump(
    nodes: &mut [&mut Swarm<Behaviour>],
    how_long: Duration,
    received: &mut Vec<Delivery>,
) {
    let deadline = tokio::time::Instant::now() + how_long;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        let mut futures = Vec::new();
        for (index, node) in nodes.iter_mut().enumerate() {
            futures.push((index, node));
        }
        // One step at a time across all nodes, so nothing starves.
        for (index, node) in futures {
            let step = tokio::time::timeout(Duration::from_millis(20), node.select_next_some());
            if let Ok(event) = step.await
                && let SwarmEvent::Behaviour(BehaviourEvent::Gossipsub(gossipsub::Event::Message {
                    message,
                    message_id,
                    ..
                })) = event
            {
                received.push(Delivery {
                    node: index,
                    source: message.source,
                    data: message.data.clone(),
                    id: message_id,
                    sequence: message.sequence_number,
                });
            }
        }
    }
}

/// Connect every node to the first one and subscribe them all.
async fn mesh_up(nodes: &mut [&mut Swarm<Behaviour>], topic: &IdentTopic) -> Multiaddr {
    let addr = listen(nodes[0]).await;
    for node in nodes.iter_mut() {
        node.behaviour_mut()
            .gossipsub
            .subscribe(topic)
            .expect("subscribe");
    }
    for node in nodes.iter_mut().skip(1) {
        node.dial(addr.clone()).expect("dial");
    }
    // Let connections, subscriptions and the mesh settle.
    let mut nothing = Vec::new();
    pump(nodes, Duration::from_secs(2), &mut nothing).await;
    addr
}

/// B1 — two authenticated publishers, one application-envelope id.
pub async fn b1_distinct_mesh_ids() {
    let topic = IdentTopic::new("/spike-002/broadcast");
    let mut receiver = signing_node(IdRule::SourceAndSequence);
    let mut first = signing_node(IdRule::SourceAndSequence);
    let mut second = signing_node(IdRule::SourceAndSequence);
    let first_peer = *first.local_peer_id();
    let second_peer = *second.local_peer_id();

    {
        let mut nodes: Vec<&mut Swarm<Behaviour>> = vec![&mut receiver, &mut first, &mut second];
        let _ = mesh_up(&mut nodes, &topic).await;
    }

    // ONE application-envelope message id, carried by two publishers.
    // The envelope is opaque to the mesh, which is the point: the id
    // function never reads it.
    let envelope = b"{\"message_id\":\"same-application-id\"}".to_vec();

    let mut received = Vec::new();
    {
        let mut nodes: Vec<&mut Swarm<Behaviour>> = vec![&mut receiver, &mut first, &mut second];
        let mut warmup = Vec::new();
        pump(&mut nodes, Duration::from_millis(500), &mut warmup).await;

        let a = nodes[1]
            .behaviour_mut()
            .gossipsub
            .publish(topic.clone(), envelope.clone());
        let b = nodes[2]
            .behaviour_mut()
            .gossipsub
            .publish(topic.clone(), envelope.clone());
        note("first publish accepted locally", a.is_ok());
        note("second publish accepted locally", b.is_ok());

        pump(&mut nodes, Duration::from_secs(3), &mut received).await;
    }

    let received: Vec<_> = received.into_iter().filter(|d| d.node == 0).collect();
    let ids: Vec<&MessageId> = received.iter().map(|d| &d.id).collect();
    let distinct = {
        let mut seen: Vec<&MessageId> = Vec::new();
        for id in &ids {
            if !seen.contains(id) {
                seen.push(id);
            }
        }
        seen.len()
    };
    note("messages delivered to the receiver", received.len());
    note("distinct mesh ids among them", distinct);
    note(
        "both publishers reached the application",
        received.iter().any(|d| d.source == Some(first_peer))
            && received.iter().any(|d| d.source == Some(second_peer)),
    );
}

/// B2 — the ordering `PUBSUB.md` demands.
pub async fn b2_authenticity_before_cache() {
    let topic = IdentTopic::new("/spike-002/broadcast");

    // The victim signs; the forger claims the victim's identity without
    // signing, which is exactly an "invalid signed-source claim".
    let victim_keypair = libp2p::identity::Keypair::generate_ed25519();
    let victim_peer = PeerId::from_public_key(&victim_keypair.public());

    // THE STRICT RECEIVER is the node under test. The PERMISSIVE one is
    // the positive control the first version of this experiment lacked:
    // without it, "the forged message was not delivered" is equally
    // explained by the forgery never having reached anyone, and the
    // experiment closes the spike without the invalid message ever
    // touching a receive path.
    let mut receiver = signing_node(IdRule::PayloadDerived);
    let mut permissive = node(
        MessageAuthenticity::Signed(libp2p::identity::Keypair::generate_ed25519()),
        IdRule::PayloadDerived,
        ValidationMode::Permissive,
    );
    let mut forger = node(
        MessageAuthenticity::Author(victim_peer),
        IdRule::PayloadDerived,
        ValidationMode::Permissive,
    );
    let mut victim = libp2p::SwarmBuilder::with_existing_identity(victim_keypair.clone())
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("tcp")
        .with_behaviour(|_| Behaviour {
            gossipsub: gossipsub::Behaviour::new(
                MessageAuthenticity::Signed(victim_keypair),
                config(IdRule::PayloadDerived),
            )
            .expect("gossipsub behaviour"),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
        .build();

    // WIRED BY HAND, because the star topology `mesh_up` builds put the
    // permissive receiver DOWNSTREAM of the strict one -- and the strict
    // one rejects the forgery, so it never forwards it. The control then
    // reported zero for the same reason the experiment did, which is a
    // control that cannot fail independently of what it is controlling.
    //
    // The permissive receiver therefore dials the FORGER directly, and
    // its path to the forged message does not pass through the node
    // under test.
    let strict_addr = listen(&mut receiver).await;
    let forger_addr = listen(&mut forger).await;
    for (node, addr) in [
        (&mut forger, strict_addr.clone()),
        (&mut victim, strict_addr.clone()),
        (&mut permissive, forger_addr.clone()),
    ] {
        node.behaviour_mut()
            .gossipsub
            .subscribe(&topic)
            .expect("subscribe");
        node.dial(addr).expect("dial");
    }
    receiver
        .behaviour_mut()
        .gossipsub
        .subscribe(&topic)
        .expect("subscribe");
    {
        let mut nodes: Vec<&mut Swarm<Behaviour>> =
            vec![&mut receiver, &mut forger, &mut victim, &mut permissive];
        let mut nothing = Vec::new();
        pump(&mut nodes, Duration::from_secs(3), &mut nothing).await;
    }

    // ONE payload, so the forged and the genuine message share a mesh id
    // under the experiment's id rule.
    let payload = b"the-contested-message".to_vec();

    let mut after_forgery = Vec::new();
    let mut after_genuine = Vec::new();
    {
        let mut nodes: Vec<&mut Swarm<Behaviour>> =
            vec![&mut receiver, &mut forger, &mut victim, &mut permissive];
        let mut warmup = Vec::new();
        pump(&mut nodes, Duration::from_millis(500), &mut warmup).await;

        // CONTROL, so a pass cannot mean "nothing reached anyone". A
        // different payload from the victim first: if the receiver does
        // not get this, the mesh is not up and the rest of the
        // experiment measures the wiring rather than the ordering.
        let control_payload = b"control-message".to_vec();
        let control = nodes[2]
            .behaviour_mut()
            .gossipsub
            .publish(topic.clone(), control_payload.clone());
        note("control publish accepted", control.is_ok());
        let mut control_seen = Vec::new();
        pump(&mut nodes, Duration::from_secs(2), &mut control_seen).await;
        note(
            "control delivered to the strict receiver",
            !deliveries(&control_seen, 0, &control_payload).is_empty(),
        );
        note(
            "receiver's connected peers",
            nodes[0].network_info().num_peers(),
        );

        let forged = nodes[1]
            .behaviour_mut()
            .gossipsub
            .publish(topic.clone(), payload.clone());
        note("forged publish accepted by its own node", forged.is_ok());
        pump(&mut nodes, Duration::from_secs(2), &mut after_forgery).await;
        // THE EVIDENCE THAT IT ARRIVED. A permissive receiver on the same
        // mesh delivering the forged message proves it left the forger
        // and reached a receive path; the strict receiver's silence is
        // then attributable to validation rather than to non-arrival.
        //
        // FILTERED BY THE EXACT CONTESTED PAYLOAD, not merely "some
        // event landed at this index in this window". The control
        // publication above is delivered asynchronously and can be
        // delayed into THIS pump call's window rather than its own --
        // `pump` polls every node for a fixed duration regardless of
        // what has already arrived, so a late control delivery would
        // otherwise be counted as if it were the forged message and
        // the verdict below would pass without the contested payload
        // ever having arrived at all.
        note(
            "forged message delivered to the PERMISSIVE receiver",
            !deliveries(&after_forgery, 3, &payload).is_empty(),
        );

        let genuine = nodes[2]
            .behaviour_mut()
            .gossipsub
            .publish(topic.clone(), payload.clone());
        note("genuine publish accepted by its own node", genuine.is_ok());
        pump(&mut nodes, Duration::from_secs(3), &mut after_genuine).await;
    }

    // ATTRIBUTED BY SEQUENCE NUMBER, ACROSS ALL WINDOWS. Forged and
    // genuine share payload and mesh id by construction and source by
    // design, so payload cannot tell them apart -- and a forgery
    // delayed past its own pump window would have been counted as the
    // genuine delivery. The per-publisher sequence number is what
    // neither can fake: the forger's is random, the victim's is its own
    // counter.
    //
    // The forged sequence is known from the permissive receiver. The
    // genuine one is NOT independently observable: the permissive
    // receiver sits behind the forger, whose own duplicate cache
    // already holds this mesh id from the forgery it published, so it
    // never forwards the genuine message onward -- a first version of
    // this attribution tried to read the genuine sequence there and
    // found nothing. It does not need to be observed. Exactly two
    // publications of this payload exist in the experiment, so a
    // strict delivery whose sequence is not the forged one IS the
    // genuine one, by elimination, whatever window it landed in.
    let mut everything: Vec<Delivery> = Vec::new();
    everything.extend(after_forgery.iter().cloned());
    everything.extend(after_genuine.iter().cloned());

    let forged_seq: Option<u64> = deliveries(&after_forgery, 3, &payload)
        .first()
        .and_then(|d| d.sequence);
    let strict_all = deliveries(&everything, 0, &payload);
    let strict_seqs: Vec<Option<u64>> = strict_all.iter().map(|d| d.sequence).collect();

    note(
        "forged sequence (seen at permissive)",
        format!("{forged_seq:?}"),
    );
    note(
        "strict deliveries of the contested payload, ALL windows",
        strict_all.len(),
    );
    note("their sequence numbers", format!("{strict_seqs:?}"));

    let forgery_reached_a_receive_path = forged_seq.is_some();
    // Exactly one, carrying a sequence, and not the forgery's -- so
    // the forgery was not delivered late either, and the one that was
    // delivered can only be the genuine publication.
    let strict_got_exactly_the_genuine_one =
        strict_all.len() == 1 && strict_seqs[0].is_some() && strict_seqs[0] != forged_seq;
    note(
        "forgery reached a receive path",
        forgery_reached_a_receive_path,
    );
    note(
        "strict delivered exactly one, and it is not the forgery",
        strict_got_exactly_the_genuine_one,
    );
    note(
        "VERDICT: authenticity precedes the duplicate cache",
        forgery_reached_a_receive_path && strict_got_exactly_the_genuine_one,
    );
    note(
        "  (arrived at a receive path, rejected by strict, left nothing behind)",
        "",
    );
}

/// One gossipsub delivery, with everything needed to say WHICH
/// publication it was.
///
/// `sequence` is the field that makes attribution possible. Forged and
/// genuine share payload and mesh id by construction -- that collision
/// is the experiment -- and share `source` by design, since the forger
/// claims the victim's identity. What they cannot share is the
/// per-publisher sequence number: the forger's is random, the victim's
/// is its own counter, and neither can produce the other's.
#[derive(Debug, Clone)]
struct Delivery {
    node: usize,
    source: Option<PeerId>,
    data: Vec<u8>,
    id: MessageId,
    sequence: Option<u64>,
}

/// Every delivery of `payload` at node `at`, whatever window it landed
/// in.
fn deliveries<'a>(events: &'a [Delivery], at: usize, payload: &[u8]) -> Vec<&'a Delivery> {
    events
        .iter()
        .filter(|d| d.node == at && d.data == payload)
        .collect()
}

/// B3 -- an invalid SIGNED source claim, injected on the wire.
///
/// B2 uses `MessageAuthenticity::Author`, which publishes UNSIGNED with
/// a claimed source. `PUBSUB.md` requires the receive path to reject an
/// invalid **signed** source claim before it can create a lasting
/// duplicate-cache entry, and a missing signature is not that: it could
/// in principle be refused earlier, leaving the signed case untested
/// while the spike reported PASS. Review caught that, and it is the
/// reason this experiment exists rather than an extension of B2.
///
/// Nothing in the public API can produce the message: `publish` only
/// ever signs correctly, and no public call accepts a prebuilt
/// `RawMessage`. So the frames are written directly to a
/// `/meshsub/1.1.0` substream by [`crate::inject`].
///
/// # The control is the point
///
/// Hand-rolled protobuf invites its own failure mode: an encoding the
/// receiver cannot parse is also rejected, and the experiment would
/// then report success for a reason having nothing to do with
/// signatures. So the SAME injector sends a correctly-signed message
/// first. The receiver must deliver that one. Only then does a
/// rejection of the mutated one mean what it says.
pub async fn b3_invalid_signed_claim_is_rejected() {
    use crate::inject::{Injector, signed_message};

    let topic = IdentTopic::new("/spike-002/broadcast");
    let victim_keypair = libp2p::identity::Keypair::generate_ed25519();
    let victim_peer = PeerId::from_public_key(&victim_keypair.public());
    let topic_string = topic.hash().into_string();

    // Both messages claim the victim and share a mesh id under the
    // payload-derived rule, which is the collision the ordering
    // question needs.
    let payload = b"the-signed-contested-message".to_vec();

    // CONTROL: signature computed over exactly the data it carries.
    let valid = signed_message(&victim_keypair, &topic_string, &payload, &payload, 1);
    // UNDER TEST: signature present and well-formed, computed over
    // DIFFERENT bytes, so it cannot verify over what is carried.
    let invalid = signed_message(
        &victim_keypair,
        &topic_string,
        &payload,
        b"something-else",
        2,
    );
    assert_ne!(valid, invalid, "the two injections must differ");

    // The control's result GATES the verdict below. A broken encoder
    // makes the control fail and the invalid case "pass" -- rejected,
    // but for the wrong reason -- and a verdict that did not depend on
    // its own control reported PASS anyway. Mutating the field order
    // caught exactly that, which is why `control_ok` exists.
    let mut control_ok = false;
    for (label, message, expect_delivered) in [
        ("correctly signed (control)", valid, true),
        ("signed, signature invalid", invalid, false),
    ] {
        let mut receiver = signing_node(IdRule::PayloadDerived);
        let addr = listen(&mut receiver).await;
        receiver
            .behaviour_mut()
            .gossipsub
            .subscribe(&topic)
            .expect("subscribe");

        let mut injector = libp2p::SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .expect("tcp")
            .with_behaviour(|_| Injector::new(&topic_string, &message))
            .expect("behaviour")
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
            .build();
        injector.dial(addr.clone()).expect("dial");

        // Drive both for a fixed window and record what the RECEIVER
        // delivered to its application.
        let mut delivered = 0_usize;
        let mut sources: Vec<Option<PeerId>> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        while tokio::time::Instant::now() < deadline {
            tokio::select! {
                e = receiver.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Gossipsub(
                        gossipsub::Event::Message { message, .. })) = e
                        && message.data == payload
                    {
                        delivered += 1;
                        sources.push(message.source);
                    }
                }
                _ = injector.select_next_some() => {}
                () = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }

        note(&format!("  {label}: delivered"), delivered);
        if expect_delivered {
            control_ok = delivered == 1 && sources.first() == Some(&Some(victim_peer));
            note(
                "  the injector CAN produce an acceptable message",
                control_ok,
            );
            continue;
        }
        note("  and an invalid signature is refused", delivered == 0);

        // THE COLLISION, which is the actual ordering question. The
        // rejection above is only half: what `PUBSUB.md` requires is
        // that the rejected message left NOTHING BEHIND, so a genuine
        // message with the same mesh id is still delivered afterwards.
        // A receive path that cached before verifying would suppress
        // it, and every number above would look identical.
        let mut victim = libp2p::SwarmBuilder::with_existing_identity(victim_keypair.clone())
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .expect("tcp")
            .with_behaviour(|_| Behaviour {
                gossipsub: gossipsub::Behaviour::new(
                    MessageAuthenticity::Signed(victim_keypair.clone()),
                    config(IdRule::PayloadDerived),
                )
                .expect("gossipsub behaviour"),
            })
            .expect("behaviour")
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
            .build();
        victim
            .behaviour_mut()
            .gossipsub
            .subscribe(&topic)
            .expect("subscribe");
        victim.dial(addr.clone()).expect("dial");

        // Let the mesh form before publishing, or the publish has
        // nobody to reach and the test measures the wiring.
        let settle = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < settle {
            tokio::select! {
                _ = receiver.select_next_some() => {}
                _ = victim.select_next_some() => {}
                () = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
        let published = victim
            .behaviour_mut()
            .gossipsub
            .publish(topic.clone(), payload.clone());
        note(
            "  genuine publish accepted by its own node",
            published.is_ok(),
        );

        let mut genuine_delivered = 0_usize;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        while tokio::time::Instant::now() < deadline {
            tokio::select! {
                e = receiver.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Gossipsub(
                        gossipsub::Event::Message { message, .. })) = e
                        && message.data == payload
                    {
                        genuine_delivered += 1;
                    }
                }
                _ = victim.select_next_some() => {}
                () = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
        note(
            "  genuine message with the SAME mesh id delivered after it",
            genuine_delivered,
        );
        note(
            "  VERDICT: an invalid signed claim leaves no cache entry",
            control_ok && delivered == 0 && genuine_delivered == 1,
        );
    }
}
