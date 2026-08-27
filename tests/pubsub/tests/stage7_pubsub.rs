// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The prerequisites Stage 7 is built against.
//!
//! This package was activated when Stage 7 opened, and carries only what
//! can be true before the behaviour exists: that the frozen vectors are
//! present, frozen, and shaped the way the implementation will be built
//! to read them. The plan's required tests — mesh duplicate identity,
//! the ADR-0029 Accept / Ignore / Reject mapping, an unauthorized
//! publisher, propagation under the frozen policy — arrive with the
//! behaviour they describe.
//!
//! Asserting anything about GossipSub itself here would be a stub
//! asserting nothing, which is the opposite of what activating a package
//! is for: the point of `[workspace].members` carrying this path is that
//! the package COMPILES AND RUNS from the moment the stage opens, so the
//! first behaviour test lands in a package that already works.
//!
//! The same two vectors are recomputed from their declared algorithms by
//! `tools/checks/verify_fixture_vectors.py` on every CI run. What is
//! checked here is different and deliberately so: that this stage's own
//! test package can read them, and that their SHAPE has not changed
//! under the code about to be written against it.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_test_support::fixtures;

/// The mesh duplicate identity, frozen before anything can drift.
///
/// ADR-0029 keys mesh dedup on the authenticated publisher PeerId and
/// the GossipSub wire sequence number — NOT the application envelope ID.
/// The distinction is the whole reason this vector is frozen: two
/// authenticated publishers may legitimately use one envelope ID, and a
/// dedup that collapsed them would drop a message nobody sent twice.
#[test]
fn the_message_id_vectors_this_stage_is_built_against_exist() {
    let file = fixtures::load("gossipsub/gossipsub-message-id-v1.json");

    assert_eq!(
        file["algorithm"]["id"], "gossipsub-message-id-v1",
        "the fixture must name the algorithm it freezes"
    );
    let vectors = file["vectors"].as_array().expect("a vectors array");
    assert_eq!(vectors.len(), 4, "four message-id vectors");

    for vector in vectors {
        for field in ["name", "peer_id", "sequence_number", "sha256"] {
            assert!(
                vector.get(field).is_some(),
                "vector {:?} lacks `{field}`, which the id is computed from",
                vector["name"]
            );
        }
    }
}

/// The ChannelId -> topic derivation, frozen the same way.
///
/// A topic key is how a ChannelId reaches the mesh, and it is derived
/// rather than transmitted. If this derivation moved, two peers on the
/// same channel would subscribe to different topics and simply never
/// hear each other — a silent partition rather than an error.
#[test]
fn the_topic_key_vectors_this_stage_is_built_against_exist() {
    let file = fixtures::load("gossipsub/gossipsub-topic-key-v1.json");

    assert_eq!(
        file["algorithm"]["id"], "gossipsub-topic-key-v1",
        "the fixture must name the algorithm it freezes"
    );
    let vectors = file["vectors"].as_array().expect("a vectors array");
    assert_eq!(vectors.len(), 5, "five topic-key vectors");

    for vector in vectors {
        for field in ["name", "channel_id", "sha256"] {
            assert!(
                vector.get(field).is_some(),
                "vector {:?} lacks `{field}`, which the topic is derived from",
                vector["name"]
            );
        }
    }
}
