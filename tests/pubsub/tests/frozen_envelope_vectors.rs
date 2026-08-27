// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The broadcast codec against the frozen bytes.
//!
//! `fixtures/gossipsub/broadcast-message-v1-frame.json` is the
//! cross-implementation contract for `BroadcastMessageV1` framing: an
//! Android or third-party client agrees with this codec byte for byte or
//! it does not interoperate. So the test is `encode() == frame_hex`, not
//! a round trip — a round trip proves only that this implementation
//! agrees with itself, which is exactly the property that stays true
//! while the wire drifts.
//!
//! These live in the root suite for the same reason
//! `tests/direct-v2/tests/frozen_frame_vectors.rs` does: the vectors are
//! protocol assets shared with clients outside this workspace, and
//! `architecture/docs/architecture/testing.md` requires frozen fixtures
//! not to be hidden inside a Rust test module.
#![allow(clippy::expect_used, clippy::panic)]

use interweave_test_support::{fixtures, hex};
use interweave_transport_api::{
    BroadcastMessageV1, MAX_PAYLOAD_BYTES, MediaType, MessageId, Payload,
};

/// One vector, read from the fixture rather than restated here.
struct Vector {
    name: String,
    message: BroadcastMessageV1,
    frame: Vec<u8>,
    frame_len: usize,
}

fn vectors() -> Vec<Vector> {
    let file = fixtures::load("gossipsub/broadcast-message-v1-frame.json");
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
                message: BroadcastMessageV1 {
                    message_id: MessageId::parse_hex(v["message_id"].as_str().expect("message_id"))
                        .expect("fixture message id parses"),
                    sent_at_ms: v["sent_at_ms"].as_u64().expect("sent_at_ms"),
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
    assert_eq!(vectors.len(), 5, "five vectors, per the fixture README");
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
        let decoded = BroadcastMessageV1::decode(&frame, MAX_PAYLOAD_BYTES)
            .unwrap_or_else(|e| panic!("vector `{name}` did not decode: {e}"));
        assert_eq!(decoded, message, "vector `{name}` round trip");
    }
}

/// Every frozen frame declares version 1, read from the BYTES rather than
/// from the struct — the struct has no version field, because
/// `BroadcastMessageV1` *is* version 1, and a reader learning the version
/// from anywhere but the wire would learn nothing.
#[test]
fn every_frozen_vector_declares_the_version_in_its_first_byte() {
    for Vector { name, frame, .. } in vectors() {
        assert_eq!(
            frame.first().copied(),
            Some(1),
            "vector `{name}` must declare version 1 in band"
        );
    }
}

/// `sent_at_ms` reaches the wire and changes nothing else.
///
/// The fixture carries a `u64::MAX` twin of an ordinary vector: same
/// message id, same media type, same payload. If any admission input were
/// derived from the timestamp, these two frames would have to differ
/// somewhere other than those eight bytes — so this pins PUBSUB.md's
/// "diagnostic only" against the frozen bytes rather than against a
/// comment.
#[test]
fn the_sent_at_twin_differs_from_its_ordinary_vector_in_exactly_eight_bytes() {
    let all = vectors();
    let ordinary = all
        .iter()
        .find(|v| v.name == "with-media-type")
        .expect("the with-media-type vector");
    let twin = all
        .iter()
        .find(|v| v.name == "sent-at-ms-is-only-bytes")
        .expect("the sent-at twin");

    assert_eq!(
        ordinary.frame.len(),
        twin.frame.len(),
        "a timestamp does not change the frame's length"
    );
    let differing: Vec<usize> = ordinary
        .frame
        .iter()
        .zip(&twin.frame)
        .enumerate()
        .filter_map(|(i, (a, b))| (a != b).then_some(i))
        .collect();
    assert_eq!(
        differing,
        (17..25).collect::<Vec<_>>(),
        "only the eight sent_at_ms bytes may differ"
    );

    let decoded_ordinary =
        BroadcastMessageV1::decode(&ordinary.frame, MAX_PAYLOAD_BYTES).expect("decodes");
    let decoded_twin = BroadcastMessageV1::decode(&twin.frame, MAX_PAYLOAD_BYTES).expect("decodes");
    assert_eq!(decoded_twin.sent_at_ms, u64::MAX);
    assert_eq!(
        decoded_ordinary.payload, decoded_twin.payload,
        "the payload is untouched by the timestamp"
    );
    assert_eq!(decoded_ordinary.message_id, decoded_twin.message_id);
}

/// The absent media type is asserted from the FROZEN bytes, not from a
/// value this test constructed — so a codec that turns absence into
/// `Some("")` fails here and not only in a unit test written from the
/// same belief as the codec.
#[test]
fn the_frozen_vectors_carry_an_absent_media_type_not_an_empty_one() {
    let all = vectors();
    let absent = all
        .iter()
        .find(|v| v.name == "absent-media-type")
        .expect("the absent-media-type vector");

    assert_eq!(
        absent.frame[25], 0,
        "media_type_len is the byte after version, id and timestamp"
    );
    let decoded = BroadcastMessageV1::decode(&absent.frame, MAX_PAYLOAD_BYTES).expect("decodes");
    assert!(
        decoded.payload.media_type().is_none(),
        "a zero length is absence, not an empty media type"
    );

    let empty_payload = all
        .iter()
        .find(|v| v.name == "empty-payload")
        .expect("the empty-payload vector");
    let decoded = BroadcastMessageV1::decode(&empty_payload.frame, MAX_PAYLOAD_BYTES)
        .expect("an empty payload is legal");
    assert!(decoded.payload.is_empty());
    assert!(
        decoded.payload.media_type().is_some(),
        "an empty payload still carries its media type: the two absences are unrelated"
    );
}
