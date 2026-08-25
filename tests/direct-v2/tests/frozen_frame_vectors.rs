// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The codec against the frozen bytes.
//!
//! `fixtures/direct-v2/direct-message-v2-frame.json` is the
//! cross-implementation contract for `DirectMessageV2` framing: an
//! Android or third-party client agrees with this codec byte for byte or
//! it does not interoperate. So the test is `encode() == frame_hex`, not
//! a round trip — a round trip proves only that this implementation
//! agrees with itself, which is exactly the property that stays true
//! while the wire drifts.
//!
//! These live in the root suite rather than beside the codec because the
//! vectors are protocol assets shared with clients outside this
//! workspace, and `architecture/docs/architecture/testing.md` requires
//! frozen fixtures not to be hidden inside a Rust test module.
#![allow(clippy::expect_used, clippy::panic)]

use interweave_test_support::{fixtures, hex};
use interweave_transport_api::{
    DirectMessageV2, EndpointId, MAX_PAYLOAD_BYTES, MediaType, MessageId, Payload,
};

/// One vector, read from the fixture rather than restated here.
struct Vector {
    name: String,
    message: DirectMessageV2,
    frame: Vec<u8>,
    frame_len: usize,
}

fn vectors() -> Vec<Vector> {
    let file = fixtures::load("direct-v2/direct-message-v2-frame.json");
    file["vectors"]
        .as_array()
        .expect("a vectors array")
        .iter()
        .map(|v| {
            let media = v["media_type"]
                .as_str()
                .map(|m| MediaType::parse(m).expect("fixture media type parses"));
            let payload_bytes =
                hex::decode(v["payload_hex"].as_str().expect("payload_hex")).expect("valid hex");
            Vector {
                name: v["name"].as_str().expect("name").to_owned(),
                message: DirectMessageV2 {
                    message_id: MessageId::parse_hex(v["message_id"].as_str().expect("message_id"))
                        .expect("fixture message id parses"),
                    sent_at_ms: v["sent_at_ms"].as_u64().expect("sent_at_ms"),
                    source_endpoint: EndpointId::parse(
                        v["source_endpoint"].as_str().expect("source_endpoint"),
                    )
                    .expect("fixture source endpoint parses"),
                    destination_endpoint: v["destination_endpoint"]
                        .as_str()
                        .map(|d| EndpointId::parse(d).expect("fixture destination parses")),
                    payload: Payload::at_ceiling(media, payload_bytes)
                        .expect("fixture payload within the ceiling"),
                },
                frame: hex::decode(v["frame_hex"].as_str().expect("frame_hex")).expect("valid hex"),
                frame_len: usize::try_from(v["frame_len"].as_u64().expect("frame_len"))
                    .expect("a frame length fits usize"),
            }
        })
        .collect()
}

/// ENCODE IS BYTE-IDENTICAL. The direction that pins interoperability.
#[test]
fn every_frozen_vector_encodes_to_exactly_its_frozen_bytes() {
    let vectors = vectors();
    assert_eq!(vectors.len(), 6, "six vectors, per the fixture README");
    for Vector {
        name,
        message,
        frame,
        frame_len,
    } in vectors
    {
        let encoded = message.encode();
        assert_eq!(
            hex::encode(&encoded),
            hex::encode(&frame),
            "vector `{name}` did not encode to its frozen bytes"
        );
        assert_eq!(encoded.len(), frame_len, "vector `{name}` frame_len");
    }
}

/// DECODE RECOVERS EVERY FIELD. The other direction, so a codec that
/// happened to emit the right bytes from wrong internals still fails.
#[test]
fn every_frozen_vector_decodes_back_to_its_fields() {
    for Vector {
        name,
        message,
        frame,
        ..
    } in vectors()
    {
        let decoded = DirectMessageV2::decode(&frame, MAX_PAYLOAD_BYTES)
            .unwrap_or_else(|e| panic!("vector `{name}` did not decode: {e}"));
        assert_eq!(decoded, message, "vector `{name}` round trip");
    }
}

/// The two zero-length encodings mean different things, and the fixtures
/// carry one of each. Asserted from the FROZEN bytes rather than from a
/// value this test constructed, so a codec that treats them alike fails
/// here and not only in a unit test written from the same belief.
#[test]
fn the_frozen_vectors_distinguish_omitted_destination_from_absent_media() {
    let all = vectors();

    let default_route = all
        .iter()
        .find(|v| v.name == "default-destination")
        .expect("the default-destination vector");
    let decoded =
        DirectMessageV2::decode(&default_route.frame, MAX_PAYLOAD_BYTES).expect("decodes");
    assert_eq!(
        decoded.destination_endpoint, None,
        "destination_endpoint_len = 0 is the receiver's default, not a label"
    );
    assert!(
        decoded.payload.media_type().is_some(),
        "this vector still carries a media type"
    );

    let absent_media = all
        .iter()
        .find(|v| v.name == "absent-media-type")
        .expect("the absent-media-type vector");
    let decoded = DirectMessageV2::decode(&absent_media.frame, MAX_PAYLOAD_BYTES).expect("decodes");
    assert_eq!(
        decoded.payload.media_type(),
        None,
        "media_type_len = 0 is absence, never an empty string"
    );
    assert!(
        decoded.destination_endpoint.is_some(),
        "this vector still carries an explicit destination"
    );
}

/// `sent_at_ms` is on the wire and out of the fingerprint. The fixture
/// pair differs only in that field, so the frames must differ while the
/// content the fingerprint covers does not.
#[test]
fn two_vectors_differing_only_in_sent_at_ms_produce_different_frames() {
    let all = vectors();
    let a = all
        .iter()
        .find(|v| v.name == "explicit-destination")
        .expect("explicit-destination");
    let b = all
        .iter()
        .find(|v| v.name == "sent-at-ms-is-not-fingerprinted")
        .expect("the sent_at_ms pair");

    assert_ne!(a.message.sent_at_ms, b.message.sent_at_ms);
    assert_ne!(a.frame, b.frame, "the frames differ");
    assert_eq!(
        a.message.payload.bytes(),
        b.message.payload.bytes(),
        "the payload the fingerprint covers does not"
    );
    assert_eq!(
        a.message.payload.media_type(),
        b.message.payload.media_type()
    );
}

/// Both endpoint labels at the 64-byte ceiling still encode, and the
/// single-byte length fields hold them.
#[test]
fn the_maximum_endpoint_vector_encodes_at_the_ceiling() {
    let all = vectors();
    let max = all
        .iter()
        .find(|v| v.name == "max-endpoints-64")
        .expect("max-endpoints-64");
    assert_eq!(max.message.source_endpoint.as_str().len(), 64);
    assert_eq!(
        max.message
            .destination_endpoint
            .as_ref()
            .expect("an explicit destination")
            .as_str()
            .len(),
        64
    );
    assert_eq!(hex::encode(&max.message.encode()), hex::encode(&max.frame));
}
