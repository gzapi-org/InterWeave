// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Production against the independent codecs (plan §20 (c); testing.md
//! §Compatibility fixtures, A 2026-10-08).
//!
//! `interweave-independent-codecs` is written from the contract text with no
//! path package in its graph (`tools/checks/check_independent_codecs.sh`).
//! Here, where both are in reach, every production encoding decodes there
//! to the same fields, and every independent encoding decodes in
//! production. Both are tried over a few thousand deterministic messages
//! that cover each field's edges. Then every frame is mutated one byte at a
//! time, and the two decoders must agree on accept and refuse and, when
//! both accept, on every field. A disagreement is a contract finding for
//! architect-cto, never a test to bend: the contract text is the oracle.
//!
//! The domain is the one both media-type readings share (printable ASCII),
//! until 01a11c8b is ruled; see the codecs' README.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use interweave_independent_codecs as ind;
use interweave_transport_api::{
    BroadcastMessageV1, DirectMessageV2, EndpointId, MAX_PAYLOAD_BYTES, MediaType, MessageId,
    Payload,
};
use interweave_transport_runtime::direct_content_fingerprint_v1;

/// xorshift64*: deterministic, so a disagreement reproduces from its case
/// number.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }
    fn pick<'a>(&mut self, from: &'a [u8]) -> u8 {
        from[self.below(from.len())]
    }
}

const ENDPOINT_HEAD: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const ENDPOINT_TAIL: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789._-";

fn endpoint(r: &mut Rng) -> String {
    // The edges: one character, 64, and in between.
    let len = match r.below(4) {
        0 => 1,
        1 => 64,
        _ => 1 + r.below(64),
    };
    let mut s = String::from(char::from(r.pick(ENDPOINT_HEAD)));
    while s.len() < len {
        s.push(char::from(r.pick(ENDPOINT_TAIL)));
    }
    s
}

fn media(r: &mut Rng) -> Option<String> {
    let len = match r.below(5) {
        0 => return None,
        1 => 1,
        2 => 128,
        _ => 1 + r.below(128),
    };
    Some(
        (0..len)
            .map(|_| char::from(u8::try_from(0x20 + r.below(0x5f)).unwrap()))
            .collect(),
    )
}

fn payload(r: &mut Rng) -> Vec<u8> {
    let len = match r.below(6) {
        0 => 0,
        1 => MAX_PAYLOAD_BYTES,
        2 => 1,
        _ => r.below(512),
    };
    (0..len)
        .map(|_| u8::try_from(r.below(256)).unwrap())
        .collect()
}

fn id(r: &mut Rng) -> [u8; 16] {
    let mut a = [0u8; 16];
    a[..8].copy_from_slice(&r.next().to_be_bytes());
    a[8..].copy_from_slice(&r.next().to_be_bytes());
    a
}

fn sent_at(r: &mut Rng) -> u64 {
    match r.below(4) {
        0 => 0,
        1 => u64::MAX,
        _ => r.next(),
    }
}

fn ind_direct(r: &mut Rng) -> ind::direct_v2::DirectMessageV2 {
    ind::direct_v2::DirectMessageV2 {
        message_id: id(r),
        sent_at_ms: sent_at(r),
        source_endpoint: endpoint(r),
        destination_endpoint: if r.below(3) == 0 {
            None
        } else {
            Some(endpoint(r))
        },
        media_type: media(r),
        payload: payload(r),
    }
}

fn to_prod_direct(m: &ind::direct_v2::DirectMessageV2) -> DirectMessageV2 {
    DirectMessageV2 {
        message_id: MessageId::from_bytes(m.message_id),
        sent_at_ms: m.sent_at_ms,
        source_endpoint: EndpointId::parse(m.source_endpoint.clone()).expect("source"),
        destination_endpoint: m
            .destination_endpoint
            .clone()
            .map(|d| EndpointId::parse(d).expect("destination")),
        payload: Payload::at_ceiling(
            m.media_type
                .clone()
                .map(|t| MediaType::parse(t).expect("media")),
            m.payload.clone(),
        )
        .expect("payload"),
    }
}

fn from_prod_direct(m: &DirectMessageV2) -> ind::direct_v2::DirectMessageV2 {
    ind::direct_v2::DirectMessageV2 {
        message_id: *m.message_id.as_bytes(),
        sent_at_ms: m.sent_at_ms,
        source_endpoint: m.source_endpoint.as_str().to_owned(),
        destination_endpoint: m
            .destination_endpoint
            .as_ref()
            .map(|d| d.as_str().to_owned()),
        media_type: m.payload.media_type().map(|t| t.as_str().to_owned()),
        payload: m.payload.bytes().to_vec(),
    }
}

fn ind_broadcast(r: &mut Rng) -> ind::broadcast_v1::BroadcastMessageV1 {
    ind::broadcast_v1::BroadcastMessageV1 {
        message_id: id(r),
        sent_at_ms: sent_at(r),
        media_type: media(r),
        payload: payload(r),
    }
}

fn to_prod_broadcast(m: &ind::broadcast_v1::BroadcastMessageV1) -> BroadcastMessageV1 {
    BroadcastMessageV1 {
        message_id: MessageId::from_bytes(m.message_id),
        sent_at_ms: m.sent_at_ms,
        payload: Payload::at_ceiling(
            m.media_type
                .clone()
                .map(|t| MediaType::parse(t).expect("media")),
            m.payload.clone(),
        )
        .expect("payload"),
    }
}

fn from_prod_broadcast(m: &BroadcastMessageV1) -> ind::broadcast_v1::BroadcastMessageV1 {
    ind::broadcast_v1::BroadcastMessageV1 {
        message_id: *m.message_id.as_bytes(),
        sent_at_ms: m.sent_at_ms,
        media_type: m.payload.media_type().map(|t| t.as_str().to_owned()),
        payload: m.payload.bytes().to_vec(),
    }
}

const CASES: u64 = 2_000;

#[test]
fn direct_frames_agree_in_both_directions() {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    for case in 0..CASES {
        let m = ind_direct(&mut r);
        let ours = m.encode().expect("a legal message encodes");
        let theirs = to_prod_direct(&m).encode();
        assert_eq!(ours, theirs, "case {case}: the two encodings differ");
        let back = DirectMessageV2::decode(&ours, MAX_PAYLOAD_BYTES)
            .unwrap_or_else(|e| panic!("case {case}: production refused ours: {e}"));
        assert_eq!(from_prod_direct(&back), m, "case {case}: production decode");
        let again = ind::direct_v2::DirectMessageV2::decode(&theirs)
            .unwrap_or_else(|e| panic!("case {case}: ours refused production's: {e}"));
        assert_eq!(again, m, "case {case}: independent decode");
    }
}

#[test]
fn broadcast_frames_agree_in_both_directions() {
    let mut r = Rng(0xD1B5_4A32_D192_ED03);
    for case in 0..CASES {
        let m = ind_broadcast(&mut r);
        let ours = m.encode().expect("a legal message encodes");
        let theirs = to_prod_broadcast(&m).encode();
        assert_eq!(ours, theirs, "case {case}: the two encodings differ");
        let back = BroadcastMessageV1::decode(&ours, MAX_PAYLOAD_BYTES)
            .unwrap_or_else(|e| panic!("case {case}: production refused ours: {e}"));
        assert_eq!(
            from_prod_broadcast(&back),
            m,
            "case {case}: production decode"
        );
        let again = ind::broadcast_v1::BroadcastMessageV1::decode(&theirs)
            .unwrap_or_else(|e| panic!("case {case}: ours refused production's: {e}"));
        assert_eq!(again, m, "case {case}: independent decode");
    }
}

#[test]
fn fingerprints_agree() {
    let mut r = Rng(0x2545_F491_4F6C_DD1D);
    for case in 0..CASES {
        let media = media(&mut r);
        let bytes = payload(&mut r);
        let ours = ind::fingerprint::fingerprint(media.as_deref(), &bytes).expect("legal");
        let theirs = direct_content_fingerprint_v1(media.as_deref(), &bytes).expect("legal");
        let ours: String = ours.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(ours, format!("{theirs:x}"), "case {case}");
    }
}

/// Every one-byte change of a legal frame, both decoders: the same verdict,
/// and when both accept, the same fields. A frame small enough to mutate
/// exhaustively is used, so every position is tried with the values that
/// sit on the rules (0, 1, the bounds, the printable edges, 0xFF).
#[test]
fn mutated_frames_get_the_same_verdict_from_both_decoders() {
    const VALUES: &[u8] = &[
        0x00, 0x01, 0x02, 0x08, 0x09, 0x1F, 0x20, 0x2D, 0x2E, 0x30, 0x40, 0x41, 0x5F, 0x61, 0x7A,
        0x7E, 0x7F, 0x80, 0xC3, 0xFF,
    ];
    let mut r = Rng(0xA076_1D64_78BD_642F);
    let mut checked = 0usize;
    for _ in 0..40 {
        let mut m = ind_direct(&mut r);
        m.payload.truncate(4);
        if m.media_type.as_ref().is_some_and(|t| t.len() > 8) {
            m.media_type = Some("text/x".to_owned());
        }
        m.source_endpoint.truncate(6);
        if let Some(d) = &mut m.destination_endpoint {
            d.truncate(6);
        }
        let frame = m.encode().expect("legal");
        for at in 0..frame.len() {
            for &v in VALUES {
                let mut f = frame.clone();
                f[at] = v;
                let ours = ind::direct_v2::DirectMessageV2::decode(&f);
                let theirs = DirectMessageV2::decode(&f, MAX_PAYLOAD_BYTES);
                match (&ours, &theirs) {
                    (Ok(a), Ok(b)) => assert_eq!(*a, from_prod_direct(b), "byte {at} = {v:#04x}"),
                    (Err(_), Err(_)) => {}
                    _ => panic!(
                        "byte {at} = {v:#04x} of {}: independent {ours:?}, production {theirs:?}",
                        hex(&frame)
                    ),
                }
                checked += 1;
            }
        }
        let b = ind::broadcast_v1::BroadcastMessageV1 {
            message_id: m.message_id,
            sent_at_ms: m.sent_at_ms,
            media_type: m.media_type.clone(),
            payload: m.payload.clone(),
        };
        let frame = b.encode().expect("legal");
        for at in 0..frame.len() {
            for &v in VALUES {
                let mut f = frame.clone();
                f[at] = v;
                let ours = ind::broadcast_v1::BroadcastMessageV1::decode(&f);
                let theirs = BroadcastMessageV1::decode(&f, MAX_PAYLOAD_BYTES);
                match (&ours, &theirs) {
                    (Ok(a), Ok(b)) => {
                        assert_eq!(*a, from_prod_broadcast(b), "byte {at} = {v:#04x}");
                    }
                    (Err(_), Err(_)) => {}
                    _ => panic!(
                        "broadcast byte {at} = {v:#04x} of {}: independent {ours:?}, production {theirs:?}",
                        hex(&frame)
                    ),
                }
                checked += 1;
            }
        }
        // Truncation at every length, and one byte appended.
        for cut in 0..frame.len() {
            assert_eq!(
                ind::broadcast_v1::BroadcastMessageV1::decode(&frame[..cut]).is_ok(),
                BroadcastMessageV1::decode(&frame[..cut], MAX_PAYLOAD_BYTES).is_ok(),
                "broadcast cut at {cut}"
            );
        }
        let mut long = frame.clone();
        long.push(0);
        assert!(ind::broadcast_v1::BroadcastMessageV1::decode(&long).is_err());
        assert!(BroadcastMessageV1::decode(&long, MAX_PAYLOAD_BYTES).is_err());
    }
    assert!(checked > 40_000, "only {checked} mutations were tried");
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
