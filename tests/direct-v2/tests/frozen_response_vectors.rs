// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The response codec against the frozen bytes.
//!
//! `fixtures/direct-v2/direct-response-v2-frame.json` freezes the
//! `AcceptedV2`/`RejectedV2` frame that `DIRECT.md` §Response byte layout
//! states. Until that section existed the layout lived only in
//! `direct_codec.rs`; this test is what holds the codec to the text from
//! now on. Encode equals the frozen bytes, and decode recovers every field,
//! so a codec that emits the right bytes from the wrong internals fails
//! too. The reason is read through its name in the schema's vocabulary,
//! never through a code restated here.
#![allow(clippy::expect_used, clippy::panic)]

use interweave_test_support::{fixtures, hex};
use interweave_transport_api::{DirectRejectReason, EndpointId, MessageId};
use interweave_transport_libp2p::direct_codec::{DirectResponse, decode_response, encode_response};

fn vectors() -> Vec<(String, DirectResponse, Vec<u8>)> {
    let file = fixtures::load("direct-v2/direct-response-v2-frame.json");
    file["vectors"]
        .as_array()
        .expect("a vectors array")
        .iter()
        .map(|v| {
            let name = v["name"].as_str().expect("name").to_owned();
            let message_id = MessageId::parse_hex(v["message_id"].as_str().expect("message_id"))
                .expect("fixture message id parses");
            let response = match v["kind"].as_str().expect("kind") {
                "accepted" => DirectResponse::Accepted {
                    message_id,
                    resolved_endpoint: EndpointId::parse(
                        v["resolved_destination_endpoint"].as_str().expect("label"),
                    )
                    .expect("fixture label parses"),
                },
                "rejected" => DirectResponse::Rejected {
                    message_id,
                    reason: reason(v["reason"].as_str().expect("reason")),
                },
                other => panic!("{name}: unknown kind {other}"),
            };
            let frame = hex::decode(v["frame_hex"].as_str().expect("frame_hex")).expect("hex");
            (name, response, frame)
        })
        .collect()
}

/// The schema's names, mapped to the variants by name. No code appears
/// here: the numbering under test is the codec's.
fn reason(name: &str) -> DirectRejectReason {
    match name {
        "no_route" => DirectRejectReason::NoRoute,
        "unauthorized_peer" => DirectRejectReason::UnauthorizedPeer,
        "overloaded" => DirectRejectReason::Overloaded,
        "malformed" => DirectRejectReason::Malformed,
        "too_large" => DirectRejectReason::TooLarge,
        "shutting_down" => DirectRejectReason::ShuttingDown,
        "unsupported" => DirectRejectReason::Unsupported,
        other => panic!("{other} is not in the reject-reason vocabulary"),
    }
}

/// The names in `schemas/direct/reject-reason.schema.json`'s `enum`, in
/// order, read from the schema file itself.
fn schema_reasons() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../architecture/contracts/schemas/direct/reject-reason.schema.json");
    let text = std::fs::read_to_string(path).expect("the schema");
    let start = text.find("\"enum\"").expect("an enum");
    let open = start + text[start..].find('[').expect("its list");
    let close = open + text[open..].find(']').expect("its end");
    let names: Vec<String> = text[open + 1..close]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect();
    // A parse that found nothing would make the coverage loop vacuous.
    assert!(names.len() >= 7, "the schema's reasons, parsed: {names:?}");
    names
}

#[test]
fn every_response_vector_encodes_to_its_frozen_bytes_and_decodes_back() {
    let all = vectors();
    // A vector dropped fails the count. Coverage is held to the
    // VOCABULARY'S authority, the schema's enum read from its file: every
    // name there must map to a production variant (`reason` panics on one
    // it does not know) and have a frozen vector. A reason added to the
    // schema without a production variant or without a vector fails here.
    // A variant added to production alone is not seen by this test; the
    // wire numbering it would need is the schema's, so it has no code
    // until the schema names it.
    assert_eq!(all.len(), 9, "nine vectors, per the fixture README");
    for name in schema_reasons() {
        let r = reason(&name);
        assert!(
            all.iter().any(|(_, response, _)| {
                matches!(response, DirectResponse::Rejected { reason, .. } if *reason == r)
            }),
            "{name} has no frozen vector"
        );
    }
    for (name, response, frame) in all {
        assert_eq!(
            hex::encode(&encode_response(&response)),
            hex::encode(&frame),
            "vector `{name}` did not encode to its frozen bytes"
        );
        assert_eq!(
            decode_response(&frame).unwrap_or_else(|e| panic!("`{name}` did not decode: {e}")),
            response,
            "vector `{name}` decode"
        );
    }
}

/// `DIRECT.md`: 0 and 8..255 are unassigned and refused, never read as
/// `unsupported`; a byte after the last field is malformed. Each beside the
/// frozen vector it was cut from, which decodes.
#[test]
fn unassigned_codes_and_trailing_bytes_are_refused_beside_the_frozen_frame() {
    let (_, _, unsupported) = vectors()
        .into_iter()
        .find(|(n, _, _)| n == "rejected-unsupported")
        .expect("the code-7 vector");
    assert!(decode_response(&unsupported).is_ok());
    for code in [0u8, 8, 255] {
        let mut f = unsupported.clone();
        *f.last_mut().expect("a reason byte") = code;
        assert!(decode_response(&f).is_err(), "code {code} accepted");
    }
    let mut extra = unsupported;
    extra.push(0);
    assert!(decode_response(&extra).is_err());
}
