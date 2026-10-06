// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The device framing against the host half's own code: what the host
//! wraps, the device splits and re-frames byte for byte, and the device
//! AAD authenticates the host's ciphertext. A divergence in magic,
//! version, policy placement or AAD order fails here, on the host,
//! before any device run.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../../../harness/src/envelope.rs"]
#[allow(dead_code)]
mod envelope;

use aes_gcm::aead::{Aead, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use spike009::framing;

const FIXTURE_PEER: fn() -> String = || framing::peer_of(&[0u8; 32]).expect("the fixture derives");

#[test]
fn the_layout_constants_are_the_host_halfs() {
    assert_eq!(framing::MAGIC, envelope::MAGIC);
    assert_eq!(framing::VERSION, envelope::VERSION);
    assert_eq!(framing::IV_LEN, envelope::IV_LEN);
    assert_eq!(framing::SEED_LEN, envelope::SEED_LEN);
    assert_eq!(framing::TAG_LEN, envelope::TAG_LEN);
    assert_eq!(framing::LEN, envelope::LEN);
}

#[test]
fn a_host_envelope_splits_reframes_and_authenticates_under_the_device_aad() {
    let peer = FIXTURE_PEER();
    let key = Aes256Gcm::generate_key(&mut OsRng);
    for policy in [
        envelope::Policy::BackgroundCompatible,
        envelope::Policy::UserPresence,
    ] {
        let wrapped = envelope::wrap(&key.into(), policy, &peer, &[0u8; 32]);
        assert_eq!(framing::check(&wrapped), Ok(()));
        let (p, iv, sealed) = framing::split(&wrapped).expect("splits");
        assert_eq!(p, policy as u8);
        assert_eq!(framing::frame(p, iv, sealed).expect("frames"), wrapped);
        // What the Keystore does on the device, with the device's AAD.
        let seed = Aes256Gcm::new(&key)
            .decrypt(
                Nonce::from_slice(iv),
                Payload {
                    msg: sealed,
                    aad: &framing::aad(p, &peer),
                },
            )
            .expect("the device AAD authenticates the host's ciphertext");
        assert_eq!(framing::verify(&seed, &peer), Ok(()));
    }
}

#[test]
fn the_header_refusals_are_the_host_halfs_in_the_same_order() {
    let peer = FIXTURE_PEER();
    let key: [u8; 32] = Aes256Gcm::generate_key(&mut OsRng).into();
    let good = envelope::wrap(
        &key,
        envelope::Policy::BackgroundCompatible,
        &peer,
        &[0u8; 32],
    );
    let derive = |s: &[u8; 32]| framing::peer_of(s);
    let mut cases: Vec<(Vec<u8>, framing::Refused, envelope::Refused)> = Vec::new();
    cases.push((
        good[..good.len() - 1].to_vec(),
        framing::Refused::Length,
        envelope::Refused::Length,
    ));
    let mut m = good.clone();
    m[0] ^= 1;
    cases.push((m, framing::Refused::Magic, envelope::Refused::Magic));
    let mut v = good.clone();
    v[4] = 2;
    cases.push((v, framing::Refused::Version, envelope::Refused::Version));
    let mut p = good.clone();
    p[5] = 2;
    cases.push((p, framing::Refused::Policy, envelope::Refused::Policy));
    for (bad, device, host) in cases {
        assert_eq!(framing::check(&bad), Err(device));
        assert_eq!(envelope::unwrap(&key, &peer, &bad, derive), Err(host));
    }
}

#[test]
fn frame_refuses_lengths_the_layout_does_not_have() {
    assert!(framing::frame(0, &[0; 16], &[0; 48]).is_none());
    assert!(framing::frame(0, &[0; 12], &[0; 44]).is_none());
    assert!(framing::frame(2, &[0; 12], &[0; 48]).is_none());
}

#[test]
fn a_seed_deriving_another_identity_is_refused() {
    let peer = FIXTURE_PEER();
    // TEST-ONLY synthetic seed: a second identity, no real key material.
    assert_eq!(
        framing::verify(&[7u8; 32], &peer),
        Err(framing::Refused::WrongIdentity)
    );
    assert_eq!(
        framing::verify(&[0u8; 31], &peer),
        Err(framing::Refused::WrongIdentity)
    );
}
