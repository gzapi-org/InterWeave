// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Rust derivations reproduce every frozen GossipSub vector.
//!
//! `verify_fixture_vectors.py` recomputes these from the specification,
//! so the fixtures are an independent statement of what the algorithms
//! produce rather than a transcript of this implementation. Until now
//! nothing in Rust consumed them, which meant two implementations could
//! disagree with only the Python one being checked — the same gap
//! `crates/transport/runtime/tests/frozen_fingerprints.rs` closed for the
//! direct fingerprint.
//!
//! # Why the mesh-id vectors are read here and not in the runtime crate
//!
//! They give the publisher as a printable PeerId, deliberately: the
//! fixture keeps one source of truth per vector, because a stored byte
//! copy could drift from the printable form it claims to be. Turning that
//! string into `PeerId::to_bytes()` is a libp2p concern — the neutral
//! contract keeps `TransportIdentity` a validated string with no byte
//! accessor — so the conversion happens here, through the same parser
//! production uses, rather than through a decoder written for a test.
#![allow(clippy::expect_used, clippy::panic)]

use std::str::FromStr;

use interweave_test_support::{fixtures, hex};
use interweave_transport_api::ChannelId;
use interweave_transport_runtime::{gossipsub_message_id_v1, topic_key_v1};
use libp2p::PeerId;

#[test]
fn every_frozen_topic_key_vector_reproduces() {
    let file = fixtures::load("gossipsub/gossipsub-topic-key-v1.json");
    assert_eq!(file["algorithm"]["id"], "gossipsub-topic-key-v1");
    let vectors = file["vectors"].as_array().expect("a vectors array");
    assert_eq!(vectors.len(), 5, "five vectors, per the fixture README");

    for v in vectors {
        let name = v["name"].as_str().expect("name");
        let channel = ChannelId::parse(v["channel_id"].as_str().expect("channel_id"))
            .unwrap_or_else(|e| panic!("vector `{name}` channel id does not parse: {e:?}"));
        let expected = hex::decode(v["sha256"].as_str().expect("sha256")).expect("valid hex");

        assert_eq!(
            topic_key_v1(&channel).as_bytes().as_slice(),
            expected.as_slice(),
            "vector `{name}` did not reproduce"
        );
    }
}

#[test]
fn every_frozen_mesh_id_vector_reproduces() {
    let file = fixtures::load("gossipsub/gossipsub-message-id-v1.json");
    assert_eq!(file["algorithm"]["id"], "gossipsub-message-id-v1");
    let vectors = file["vectors"].as_array().expect("a vectors array");
    assert_eq!(vectors.len(), 4, "four vectors, per the fixture README");

    for v in vectors {
        let name = v["name"].as_str().expect("name");
        let peer = PeerId::from_str(v["peer_id"].as_str().expect("peer_id"))
            .unwrap_or_else(|e| panic!("vector `{name}` peer id does not parse: {e:?}"));
        let sequence = v["sequence_number"].as_u64().expect("sequence_number");
        let expected = hex::decode(v["sha256"].as_str().expect("sha256")).expect("valid hex");

        assert_eq!(
            gossipsub_message_id_v1(&peer.to_bytes(), sequence)
                .as_bytes()
                .as_slice(),
            expected.as_slice(),
            "vector `{name}` did not reproduce"
        );
    }
}

/// The two properties the vector set exists to pin, asserted from the
/// FIXTURE rather than from values this test invented — so an
/// implementation written from the same belief as its unit tests still
/// fails here.
#[test]
fn the_frozen_vectors_keep_two_publishers_and_two_sequences_apart() {
    let file = fixtures::load("gossipsub/gossipsub-message-id-v1.json");
    let vectors = file["vectors"].as_array().expect("a vectors array");

    let by_name = |n: &str| {
        vectors
            .iter()
            .find(|v| v["name"] == n)
            .unwrap_or_else(|| panic!("the `{n}` vector"))
    };

    // Same sequence number, different publisher. PUBSUB.md: these MUST
    // remain distinct at the duplicate cache, which is why keying on the
    // envelope id alone is forbidden.
    let first = by_name("zero-seed-peer-sequence-0");
    let second = by_name("second-peer-sequence-0");
    assert_eq!(
        first["sequence_number"], second["sequence_number"],
        "the pair is only meaningful if the sequence numbers match"
    );
    assert_ne!(
        first["peer_id"], second["peer_id"],
        "and only if the publishers differ"
    );
    assert_ne!(
        first["sha256"], second["sha256"],
        "two publishers at one sequence number must not collide"
    );

    // Same publisher, different sequence numbers, including the far edge
    // of the u64 the wire carries.
    let max = by_name("zero-seed-peer-sequence-max");
    assert_eq!(max["sequence_number"].as_u64(), Some(u64::MAX));
    assert_ne!(first["sha256"], max["sha256"]);
}

/// The topic derivation is case-sensitive, asserted from the frozen twin.
///
/// ADR-0025 makes ChannelId case-sensitive, and the fixture carries the
/// pair for exactly this reason: a case-folding "convenience" anywhere
/// above the hash merges two channels into one mesh, and nothing reports
/// it — the peers simply hear each other when they should not.
#[test]
fn the_frozen_case_twin_is_a_different_topic() {
    let file = fixtures::load("gossipsub/gossipsub-topic-key-v1.json");
    let vectors = file["vectors"].as_array().expect("a vectors array");

    let general = vectors
        .iter()
        .find(|v| v["channel_id"] == "general")
        .expect("the `general` vector");
    let twin = vectors
        .iter()
        .find(|v| v["channel_id"] == "General")
        .expect("the case-differing twin");

    assert_ne!(general["sha256"], twin["sha256"], "the fixture pins this");

    let lower = topic_key_v1(&ChannelId::parse("general").expect("valid"));
    let upper = topic_key_v1(&ChannelId::parse("General").expect("valid"));
    assert_ne!(lower, upper, "and so must the implementation");
}
