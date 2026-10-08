// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! What the contract text says no frame may carry is refused, each beside
//! a positive control one byte or one character inside the bound — so a
//! refusal that came from a broken harness rather than the rule fails the
//! control instead of passing as enforcement.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use interweave_independent_codecs::broadcast_v1::BroadcastMessageV1;
use interweave_independent_codecs::direct_response_v2::DirectResponseV2;
use interweave_independent_codecs::direct_v2::DirectMessageV2;
use interweave_independent_codecs::ipc_v2;
use interweave_independent_codecs::{MAX_PAYLOAD_BYTES, fingerprint};

/// The `PeerId` the IPC goldens carry: a real one, in the common/peer-id
/// grammar, so a control is a frame the contract accepts.
const GOLDEN_PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

fn direct(
    source: &str,
    dest: Option<&str>,
    media: Option<&str>,
    payload: usize,
) -> DirectMessageV2 {
    DirectMessageV2 {
        message_id: [7; 16],
        sent_at_ms: 1,
        source_endpoint: source.to_owned(),
        destination_endpoint: dest.map(str::to_owned),
        media_type: media.map(str::to_owned),
        payload: vec![0xAB; payload],
    }
}

/// Encode under the bound, then patch the bytes past it: the encoder
/// refuses what the decoder is being asked to refuse.
fn direct_bytes(source: &str, media_len: usize, payload: usize) -> Vec<u8> {
    let mut out = vec![7u8; 16];
    out.extend_from_slice(&1u64.to_be_bytes());
    out.push(u8::try_from(source.len()).unwrap());
    out.extend_from_slice(source.as_bytes());
    out.push(0);
    out.push(u8::try_from(media_len).unwrap());
    out.extend(std::iter::repeat_n(b'm', media_len));
    out.extend_from_slice(&u32::try_from(payload).unwrap().to_be_bytes());
    out.extend(std::iter::repeat_n(0xABu8, payload));
    out
}

#[test]
fn direct_payload_ceiling_is_exact() {
    let at = direct_bytes("human", 0, MAX_PAYLOAD_BYTES);
    assert_eq!(
        DirectMessageV2::decode(&at).unwrap().payload.len(),
        MAX_PAYLOAD_BYTES
    );
    let over = direct_bytes("human", 0, MAX_PAYLOAD_BYTES + 1);
    assert!(
        DirectMessageV2::decode(&over)
            .unwrap_err()
            .0
            .contains("above")
    );
    assert!(
        direct("human", None, None, MAX_PAYLOAD_BYTES)
            .encode()
            .is_ok()
    );
    assert!(
        direct("human", None, None, MAX_PAYLOAD_BYTES + 1)
            .encode()
            .is_err()
    );
}

#[test]
fn a_declared_payload_past_the_frame_is_refused_before_it_is_taken() {
    let mut f = direct_bytes("human", 0, 0);
    let n = f.len();
    f[n - 4..].copy_from_slice(&5u32.to_be_bytes());
    let err = DirectMessageV2::decode(&f).unwrap_err();
    assert!(err.0.contains("declared"), "{err}");
    f.extend_from_slice(b"hello");
    assert!(DirectMessageV2::decode(&f).is_ok());
}

#[test]
fn direct_media_type_is_one_to_128_ascii() {
    assert!(DirectMessageV2::decode(&direct_bytes("human", 128, 1)).is_ok());
    assert!(DirectMessageV2::decode(&direct_bytes("human", 129, 1)).is_err());
    assert!(
        direct("human", None, Some(&"m".repeat(128)), 1)
            .encode()
            .is_ok()
    );
    assert!(
        direct("human", None, Some(&"m".repeat(129)), 1)
            .encode()
            .is_err()
    );
    assert!(direct("human", None, Some(""), 1).encode().is_err());
    assert!(direct("human", None, Some("tëxt"), 1).encode().is_err());
    // Printable only (the schemas' pattern; see `is_media_type`): the
    // first and last printable bytes pass, a tab and DEL do not.
    assert!(direct("human", None, Some(" ~"), 1).encode().is_ok());
    assert!(direct("human", None, Some("a\tb"), 1).encode().is_err());
    assert!(direct("human", None, Some("a\u{7f}"), 1).encode().is_err());
}

#[test]
fn direct_source_is_always_present_and_in_the_grammar() {
    assert!(DirectMessageV2::decode(&direct_bytes("h", 0, 0)).is_ok());
    let mut none = direct_bytes("h", 0, 0);
    none.remove(16 + 8 + 1);
    none[16 + 8] = 0;
    assert!(
        DirectMessageV2::decode(&none)
            .unwrap_err()
            .0
            .contains("always present")
    );
    assert!(DirectMessageV2::decode(&direct_bytes("Human", 0, 0)).is_err());
    assert!(DirectMessageV2::decode(&direct_bytes(&"a".repeat(64), 0, 0)).is_ok());
    assert!(DirectMessageV2::decode(&direct_bytes(&"a".repeat(65), 0, 0)).is_err());
    assert!(direct("claude", Some("9bad"), None, 0).encode().is_err());
    assert!(direct("claude", Some("good"), None, 0).encode().is_ok());
}

#[test]
fn a_byte_after_the_payload_is_a_different_frame() {
    let ok = direct_bytes("human", 0, 2);
    assert!(DirectMessageV2::decode(&ok).is_ok());
    let mut extra = ok;
    extra.push(0);
    assert!(
        DirectMessageV2::decode(&extra)
            .unwrap_err()
            .0
            .contains("after")
    );
}

fn broadcast_bytes(version: u8, payload: usize) -> Vec<u8> {
    let mut out = vec![version];
    out.extend_from_slice(&[3u8; 16]);
    out.extend_from_slice(&0u64.to_be_bytes());
    out.push(0);
    out.extend_from_slice(&u32::try_from(payload).unwrap().to_be_bytes());
    out.extend(std::iter::repeat_n(1u8, payload));
    out
}

/// The unsupported-major row at the codec layer: the version byte is the
/// broadcast wire's only version, and a value but 1 is unreadable.
#[test]
fn a_broadcast_version_other_than_one_is_refused() {
    assert!(BroadcastMessageV1::decode(&broadcast_bytes(1, 0)).is_ok());
    for v in [0u8, 2, 255] {
        let err = BroadcastMessageV1::decode(&broadcast_bytes(v, 0)).unwrap_err();
        assert!(err.0.contains("version"), "{v}: {err}");
    }
}

#[test]
fn broadcast_payload_ceiling_is_exact() {
    assert!(BroadcastMessageV1::decode(&broadcast_bytes(1, MAX_PAYLOAD_BYTES)).is_ok());
    assert!(BroadcastMessageV1::decode(&broadcast_bytes(1, MAX_PAYLOAD_BYTES + 1)).is_err());
}

#[test]
fn an_empty_media_type_is_not_absence_in_the_fingerprint() {
    assert!(fingerprint::fingerprint(None, b"x").is_ok());
    assert!(fingerprint::fingerprint(Some("t"), b"x").is_ok());
    assert!(fingerprint::fingerprint(Some(""), b"x").is_err());
    assert_ne!(
        fingerprint::fingerprint(None, b"").unwrap(),
        fingerprint::fingerprint(Some("a"), b"").unwrap()
    );
}

fn ipc(body: &str) -> Result<ipc_v2::Envelope, String> {
    ipc_v2::decode_frame(&ipc_v2::encode_frame(body.as_bytes()).unwrap()).map_err(|e| e.0)
}

#[test]
fn ipc_envelope_rules_hold_beside_their_controls() {
    let cases: &[(&str, &str)] = &[
        (
            r#"{"type":"cancel","id":"1"}"#,
            r#"{"type":"cancel","id":""}"#,
        ),
        (
            r#"{"type":"cancel","id":"1"}"#,
            r#"{"type":"cancel","id":"1","x":0}"#,
        ),
        (
            r#"{"type":"cancel","id":"1"}"#,
            r#"{"type":"kancel","id":"1"}"#,
        ),
        (
            &format!(r#"{{"type":"cancel","id":"{}"}}"#, "é".repeat(128)),
            &format!(r#"{{"type":"cancel","id":"{}"}}"#, "é".repeat(129)),
        ),
        (
            r#"{"type":"response","id":"1","ok":false,"error":{"code":"Timeout"}}"#,
            r#"{"type":"response","id":"1","ok":false,"result":{},"error":{"code":"Timeout"}}"#,
        ),
        (
            r#"{"type":"response","id":"1","ok":true,"result":{}}"#,
            r#"{"type":"response","id":"1","ok":true,"error":{"code":"Timeout"}}"#,
        ),
        (
            r#"{"type":"event","sequence":0,"event_type":"peer.disconnected","data":{}}"#,
            r#"{"type":"event","sequence":-1,"event_type":"peer.disconnected","data":{}}"#,
        ),
        (
            r#"{"type":"event","sequence":0,"event_type":"peer.path_changed","data":{}}"#,
            r#"{"type":"event","sequence":0,"event_type":"peer.vanished","data":{}}"#,
        ),
        (
            r#"{"type":"server_state","health":"degraded"}"#,
            r#"{"type":"server_state","health":"unknown"}"#,
        ),
        (
            r#"{"type":"hello","ipc_version":{"major":3,"minor":0},"client":{"kind":"k"}}"#,
            r#"{"type":"hello","ipc_version":{"major":0,"minor":0},"client":{"kind":"k"}}"#,
        ),
        (
            r#"{"type":"close","code":"VersionIncompatible","supported":[{"major":2,"minor":3}]}"#,
            r#"{"type":"close","code":"VersionIncompatible","supported":[]}"#,
        ),
    ];
    for (ok, bad) in cases {
        assert!(ipc(ok).is_ok(), "control refused: {ok}: {:?}", ipc(ok));
        assert!(ipc(bad).is_err(), "accepted: {bad}");
    }
    // The schema rules below the class level, each beside its control.
    let nonce = |n: &str| format!(r#"{{"type":"ping","nonce":"{n}"}}"#);
    let lease = |members: &str| {
        format!(
            r#"{{"type":"hello_response","ipc_version":{{"major":2,"minor":3}},"transport_contract_version":"2.0","peer":"PEER","granted_capabilities":["events"]{members}}}"#
        )
        .replace("PEER", GOLDEN_PEER)
    };
    let full = r#","endpoint":"human","endpoint_lease_epoch":"AAAAAAAAAAAAAAAA","event_queue":256"#;
    let owned: Vec<(String, String)> = vec![
        // common/peer-id: 12D3KooW or Qm, then exactly 44 base58btc.
        (lease(""), lease("").replace(GOLDEN_PEER, "12D3KooW")),
        // One non-base58btc character (0, O, I, l) in the tail alone, the
        // prefix and the length kept, so only the alphabet can refuse it.
        (lease(""), lease("").replace(GOLDEN_PEER, &format!("{}0{}", &GOLDEN_PEER[..8], &GOLDEN_PEER[9..]))),
        (lease(""), lease("").replace(GOLDEN_PEER, &format!("{}O{}", &GOLDEN_PEER[..8], &GOLDEN_PEER[9..]))),
        (lease(""), lease("").replace(GOLDEN_PEER, &format!("{}l{}", &GOLDEN_PEER[..8], &GOLDEN_PEER[9..]))),
        (lease(""), lease("").replace(GOLDEN_PEER, &format!("{}I{}", &GOLDEN_PEER[..8], &GOLDEN_PEER[9..]))),
        (lease(""), lease("").replace(GOLDEN_PEER, &format!("{GOLDEN_PEER}x"))),
        (
            lease("").replace(GOLDEN_PEER, &format!("Qm{}", &GOLDEN_PEER[8..])),
            lease("").replace(GOLDEN_PEER, &format!("Qn{}", &GOLDEN_PEER[8..])),
        ),
        // frame $defs/keepalive_nonce: 16..64 of [A-Za-z0-9_-].
        (nonce(&"A".repeat(16)), nonce(&"A".repeat(15))),
        (nonce(&"_-".repeat(32)), nonce(&"A".repeat(65))),
        (nonce(&"A".repeat(16)), nonce(&format!("{}=", "A".repeat(16)))),
        // hello-response: the three lease members together or not at all.
        (lease(""), lease(r#","endpoint":"human""#)),
        (lease(full), lease(r#","endpoint":"human","endpoint_lease_epoch":"AAAAAAAAAAAAAAAA""#)),
        (lease(full), lease(r#","event_queue":256"#)),
        (lease(full), lease(&full.replace("AAAAAAAAAAAAAAAA", "AAAAAAAAAAAAAAA"))),
        (lease(full), lease(&full.replace("256", "0"))),
        (lease(full), lease(&full.replace("\"human\"", "\"Human\""))),
        (
            lease(""),
            lease("").replace(r#"["events"]"#, r#"["events","events"]"#),
        ),
        // close: VersionIncompatible names what is supported; message <= 2048.
        (
            r#"{"type":"close","code":"Timeout"}"#.to_owned(),
            r#"{"type":"close","code":"VersionIncompatible"}"#.to_owned(),
        ),
        (
            format!(r#"{{"type":"close","code":"Timeout","message":"{}"}}"#, "é".repeat(2048)),
            format!(r#"{{"type":"close","code":"Timeout","message":"{}"}}"#, "é".repeat(2049)),
        ),
        // response error.message <= 2048.
        (
            format!(r#"{{"type":"response","id":"1","ok":false,"error":{{"code":"Timeout","message":"{}"}}}}"#, "m".repeat(2048)),
            format!(r#"{{"type":"response","id":"1","ok":false,"error":{{"code":"Timeout","message":"{}"}}}}"#, "m".repeat(2049)),
        ),
        // hello: client.version <= 128; features unique, at most 8.
        (
            format!(r#"{{"type":"hello","ipc_version":{{"major":2,"minor":0}},"client":{{"kind":"k","version":"{}"}}}}"#, "v".repeat(128)),
            format!(r#"{{"type":"hello","ipc_version":{{"major":2,"minor":0}},"client":{{"kind":"k","version":"{}"}}}}"#, "v".repeat(129)),
        ),
        (
            r#"{"type":"hello","ipc_version":{"major":2,"minor":0},"client":{"kind":"k"},"features":["keepalive"]}"#.to_owned(),
            r#"{"type":"hello","ipc_version":{"major":2,"minor":0},"client":{"kind":"k"},"features":["keepalive","keepalive"]}"#.to_owned(),
        ),
        (
            r#"{"type":"hello","ipc_version":{"major":2,"minor":0},"client":{"kind":"k"},"requested_capabilities":["a","b","c","d","e","f","g","h"]}"#.to_owned(),
            r#"{"type":"hello","ipc_version":{"major":2,"minor":0},"client":{"kind":"k"},"requested_capabilities":["a","b","c","d","e","f","g","h","i"]}"#.to_owned(),
        ),
    ];
    for (ok, bad) in &owned {
        assert!(ipc(ok).is_ok(), "control refused: {ok}: {:?}", ipc(ok));
        assert!(ipc(bad).is_err(), "accepted: {bad}");
    }
    assert!(ipc("[]").is_err());

    // A complete prefix with its body still arriving is Incomplete, beside
    // the same bytes complete, which split; a missing prefix is too.
    assert_eq!(
        ipc_v2::split_frame(&[0, 0, 0, 2, b'{']),
        Ok(ipc_v2::Split::Incomplete)
    );
    assert_eq!(ipc_v2::split_frame(&[0, 0]), Ok(ipc_v2::Split::Incomplete));
    assert_eq!(
        ipc_v2::split_frame(&[0, 0, 0, 2, b'{', b'}', 9]),
        Ok(ipc_v2::Split::Frame(&b"{}"[..], &[9][..]))
    );
    assert!(ipc_v2::decode_frame(&[0, 0, 0, 2, b'{', b'}', 0]).is_err());
}

/// `DIRECT.md` §Response byte layout, each refusal beside its control.
#[test]
fn response_rules_hold_beside_their_controls() {
    let accepted = |label: &str| {
        let mut f = vec![1u8];
        f.extend_from_slice(&[5; 16]);
        f.push(u8::try_from(label.len()).unwrap());
        f.extend_from_slice(label.as_bytes());
        f
    };
    let rejected = |code: u8| {
        let mut f = vec![2u8];
        f.extend_from_slice(&[5; 16]);
        f.push(code);
        f
    };
    assert!(DirectResponseV2::decode(&accepted("human")).is_ok());
    assert!(DirectResponseV2::decode(&accepted(&"a".repeat(64))).is_ok());
    assert_eq!(accepted(&"a".repeat(64)).len(), 82);
    assert!(
        DirectResponseV2::decode(&accepted("")).is_err(),
        "a zero label"
    );
    assert!(DirectResponseV2::decode(&accepted(&"a".repeat(65))).is_err());
    assert!(DirectResponseV2::decode(&accepted("Human")).is_err());
    for code in 1..=7 {
        assert!(DirectResponseV2::decode(&rejected(code)).is_ok(), "{code}");
    }
    for code in [0u8, 8, 255] {
        assert!(DirectResponseV2::decode(&rejected(code)).is_err(), "{code}");
    }
    let mut tag3 = rejected(1);
    tag3[0] = 3;
    assert!(DirectResponseV2::decode(&tag3).is_err());
    let mut long = rejected(1);
    long.push(0);
    assert!(DirectResponseV2::decode(&long).is_err());
    let mut long = accepted("human");
    long.push(b'x');
    assert!(DirectResponseV2::decode(&long).is_err());
    assert!(
        DirectResponseV2::decode(&rejected(1)[..17]).is_err(),
        "no reason byte"
    );
    assert!(
        DirectResponseV2::Rejected {
            message_id: [0; 16],
            reason: "gone"
        }
        .encode()
        .is_err()
    );
}
