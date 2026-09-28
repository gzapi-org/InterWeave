// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Two runtimes composed from two DIFFERENT shipped example profiles --
//! `human-desktop.yaml` and `human-android.yaml` -- exchange direct and
//! broadcast over real sockets, and the frame vectors of both wires
//! (`direct-message-v2-frame.json`, `broadcast-message-v1-frame.json`)
//! carry their payload and media type through the composed runtimes
//! unchanged (plan §15 (2), decided 2026-09-27). What of a vector does
//! NOT travel -- its endpoints, its timestamp, its frame bytes -- is the
//! paragraph below.
//!
//! The profiles are the shipped documents, projected as
//! `crates/transport/composition/tests/shipped_examples.rs` projects them
//! (the sections `ProfileConfig` models; placeholders made concrete -- the
//! allowlist's placeholder becomes the other node), with ONE addition,
//! stated here: a static-bootstrap entry on the desktop side naming the
//! Android node. The examples discover through the peer cache and
//! Kademlia seeded from it, which reach nothing on a fresh host, and
//! wiring the pair through a hand-written cache file would be the same
//! addition in a less legible form.
//!
//! What the fixtures prove here is the round trip: the vector's payload
//! and media type, and the frozen message id for the first vector of each
//! file (`id_for` says why not every one), sent through one composed
//! runtime, arrive at the other identical. That the encoding is byte-exact is pinned beside the codecs
//! (`tests/direct-v2/tests/frozen_frame_vectors.rs`,
//! `tests/pubsub/tests/frozen_envelope_vectors.rs`); an independent codec
//! is Stage 17's.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Duration;

use interweave_local_client_api::{
    DataCapability, DataSessionBinding, DataSessionPort, SessionEvent, SessionRequest,
};
use interweave_local_client_conformance_tests::{PATIENCE, receive};
use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_test_support::{fixtures, hex};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointId, MediaType, MessageId, Payload,
    TransportEvent, TransportIdentity, TransportRuntime,
};
use interweave_transport_composition::{ComposedRuntime, CompositionOptions};

const MODELLED: [&str; 7] = [
    "schema_version",
    "trust",
    "endpoints",
    "discovery",
    "channels",
    "transport",
    "runtime",
];

/// `name` from the shipped examples, projected, `other` put where the
/// allowlist's placeholder is, and -- when given -- a static entry for it.
fn example(name: &str, other: &TransportIdentity, static_route: Option<&str>) -> ProfileConfig {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../architecture/config/examples")
        .join(name);
    let mut raw = std::fs::read_to_string(&path)
        .expect("the example is readable")
        .replace("<PEER_A>", other.as_str());
    // Every other placeholder -- a relay, a probe server -- names a peer
    // this test never runs: a fresh identity each.
    while let Some(start) = raw.find('<') {
        let Some(len) = raw[start..].find('>') else {
            break;
        };
        let token = raw[start..=start + len].to_owned();
        let stand_in = ProfileIdentity::generate()
            .transport_identity()
            .expect("peer id");
        raw = raw.replace(&token, stand_in.as_str());
    }
    let whole: serde_norway::Value = serde_norway::from_str(&raw).expect("YAML");
    let mapping = whole.as_mapping().expect("a mapping");
    let mut projected = serde_norway::Mapping::new();
    for key in MODELLED {
        if let Some(value) = mapping.get(serde_norway::Value::from(key)) {
            projected.insert(serde_norway::Value::from(key), value.clone());
        }
    }
    let transport = projected
        .get(serde_norway::Value::from("transport"))
        .and_then(serde_norway::Value::as_mapping)
        .cloned()
        .unwrap_or_default();
    let mut kept = serde_norway::Mapping::new();
    if let Some(connectivity) = transport.get(serde_norway::Value::from("connectivity")) {
        kept.insert(
            serde_norway::Value::from("connectivity"),
            connectivity.clone(),
        );
    }
    projected.insert(
        serde_norway::Value::from("transport"),
        serde_norway::Value::Mapping(kept),
    );
    if let Some(route) = static_route {
        let entry: serde_norway::Value = serde_norway::from_str(&format!(
            "type: static-bootstrap\nenabled: true\npriority: 5\nconfig:\n  peers: [\"{route}\"]\n"
        ))
        .expect("a provider entry");
        projected
            .get_mut(serde_norway::Value::from("discovery"))
            .and_then(|d| d.get_mut("providers"))
            .and_then(serde_norway::Value::as_sequence_mut)
            .expect("the example lists providers")
            .push(entry);
    }
    serde_norway::from_value(serde_norway::Value::Mapping(projected))
        .unwrap_or_else(|e| panic!("{name} does not parse: {e}"))
}

async fn wait_connected(runtime: &mut ComposedRuntime, peer: &TransportIdentity) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(TransportEvent::PeerConnected { peer: got, .. })) if &got == peer => return,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the runtime stopped"),
            Err(_) => panic!("no PeerConnected within {PATIENCE:?}"),
        }
    }
}

/// The desktop example dialling the Android example.
struct Pair {
    desktop: ComposedRuntime,
    android: ComposedRuntime,
    desktop_peer: TransportIdentity,
    android_peer: TransportIdentity,
    /// The examples' peer-cache files, removed when the pair drops.
    _scratch: tempfile::TempDir,
}

async fn pair() -> Pair {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let options = |cache: &str| CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        peer_cache_file: Some(scratch.path().join(cache)),
        ..CompositionOptions::default()
    };
    let desktop_id = ProfileIdentity::generate();
    let android_id = ProfileIdentity::generate();
    let desktop_peer = desktop_id.transport_identity().expect("peer id");
    let android_peer = android_id.transport_identity().expect("peer id");
    let mut android = ComposedRuntime::start(
        &android_id,
        &example("human-android.yaml", &desktop_peer, None),
        options("android.peers.json"),
    )
    .await
    .expect("the Android example composes");
    let route = format!("{}/p2p/{}", android.listening()[0], android_peer.as_str());
    let mut desktop = ComposedRuntime::start(
        &desktop_id,
        &example("human-desktop.yaml", &android_peer, Some(&route)),
        options("desktop.peers.json"),
    )
    .await
    .expect("the desktop example composes");
    wait_connected(&mut desktop, &android_peer).await;
    wait_connected(&mut android, &desktop_peer).await;
    Pair {
        desktop,
        android,
        desktop_peer,
        android_peer,
        _scratch: scratch,
    }
}

/// A session on the examples' `human` endpoint, whose only allowed client
/// kind is `human-client`.
fn human_session() -> SessionRequest {
    SessionRequest::new(
        "human-client",
        Some(EndpointId::parse("human").expect("valid")),
        [DataCapability::Commands, DataCapability::Events],
    )
    .expect("in bounds")
}

/// The id a vector is sent under. Every vector in a fixture shares ONE
/// message id -- the vectors vary the encoding, not the identity -- so on
/// one pair of runtimes the second would be a duplicate and deduplication
/// would rightly drop it. The first keeps the frozen id; the rest differ
/// from it in the last byte.
fn id_for(frozen: &str, index: usize) -> MessageId {
    let frozen = MessageId::parse_hex(frozen).expect("fixture message id parses");
    let mut bytes = *frozen.as_bytes();
    bytes[15] ^= u8::try_from(index).expect("a handful of vectors");
    MessageId::from_bytes(bytes)
}

fn payload(media: Option<&str>, payload_hex: &str) -> Payload {
    Payload::at_ceiling(
        media.map(|m| MediaType::parse(m).expect("fixture media type parses")),
        hex::decode(payload_hex).expect("valid hex"),
    )
    .expect("within the ceiling")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_frozen_direct_vector_crosses_two_example_profiles_intact() {
    let pair = pair().await;
    let from = pair
        .desktop
        .sessions()
        .open(human_session())
        .await
        .expect("the desktop human session leases");
    let to = pair
        .android
        .sessions()
        .open(human_session())
        .await
        .expect("the Android human session leases");

    let file = fixtures::load("direct-v2/direct-message-v2-frame.json");
    let vectors = file["vectors"].as_array().expect("a vectors array");
    assert!(!vectors.is_empty());
    for (index, v) in vectors.iter().enumerate() {
        let name = v["name"].as_str().expect("name");
        let id = id_for(v["message_id"].as_str().expect("message_id"), index);
        let body = payload(
            v["media_type"].as_str(),
            v["payload_hex"].as_str().expect("payload_hex"),
        );
        // The receiver's default endpoint, whatever the vector names: the
        // Android example configures `human` only, and the source is the
        // sender's lease however the vector spells it.
        from.send_direct(
            DirectDestination {
                peer: pair.android_peer.clone(),
                endpoint: None,
            },
            id,
            body.clone(),
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: not accepted: {e:?}"));
        let got = receive(&to, PATIENCE).await;
        let [SessionEvent::Direct(message)] = got.as_slice() else {
            panic!("{name}: one direct message: {got:?}");
        };
        assert_eq!(message.message_id, id, "{name}: the id crossed intact");
        assert_eq!(message.payload, body, "{name}: the payload crossed intact");
        assert_eq!(&message.source_peer, &pair.desktop_peer, "{name}");
    }

    from.close().await.expect("closes");
    to.close().await.expect("closes");
    pair.desktop.shutdown().await.expect("stops");
    pair.android.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_frozen_broadcast_vector_crosses_two_example_profiles_intact() {
    let pair = pair().await;
    let channel = ChannelId::parse("interop").expect("valid");
    let from = pair
        .desktop
        .sessions()
        .open(human_session())
        .await
        .expect("opens");
    let to = pair
        .android
        .sessions()
        .open(human_session())
        .await
        .expect("opens");
    from.join(channel.clone()).await.expect("joins");
    to.join(channel.clone()).await.expect("joins");

    let file = fixtures::load("gossipsub/broadcast-message-v1-frame.json");
    let vectors = file["vectors"].as_array().expect("a vectors array");
    assert!(!vectors.is_empty());
    for (index, v) in vectors.iter().enumerate() {
        let name = v["name"].as_str().expect("name");
        let message = BroadcastMessageV1 {
            message_id: id_for(v["message_id"].as_str().expect("message_id"), index),
            sent_at_ms: v["sent_at_ms"].as_u64().expect("sent_at_ms"),
            payload: payload(
                v["media_type"].as_str(),
                v["payload_hex"].as_str().expect("payload_hex"),
            ),
        };
        // The mesh forms on its own schedule: publish until it arrives.
        // A republish is a new publisher sequence number, so each is
        // delivered, and a late copy of an EARLIER vector may arrive now:
        // only this vector's id counts as its arrival.
        let deadline = tokio::time::Instant::now() + PATIENCE;
        let received = loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{name}: never arrived"
            );
            from.broadcast(channel.clone(), message.clone())
                .await
                .expect("accepted locally");
            let arrived = receive(&to, Duration::from_millis(500))
                .await
                .into_iter()
                .find_map(|event| match event {
                    SessionEvent::Broadcast(b) if b.message_id == message.message_id => Some(b),
                    _ => None,
                });
            if let Some(received) = arrived {
                break received;
            }
        };
        assert_eq!(received.payload, message.payload, "{name}");
        assert_eq!(&received.source_peer, &pair.desktop_peer, "{name}");
    }

    from.close().await.expect("closes");
    to.close().await.expect("closes");
    pair.desktop.shutdown().await.expect("stops");
    pair.android.shutdown().await.expect("stops");
}
