// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `org.interweave.spike009.Core`'s natives, over raw JNI. The surface
//! is byte arrays and ints only; a PeerId crosses as its UTF-8 bytes.
//! A null return or a nonzero code is the caller's to report. The Rust
//! code throws nothing; the one JNI call here that can raise is
//! `NewByteArray`, whose allocation failure returns null with Java's
//! `OutOfMemoryError` pending, which the caller then sees thrown.

#![allow(unsafe_code, clippy::missing_safety_doc)]

use std::ptr;

use jni_sys::{JNIEnv, jbyteArray, jclass, jint, jsize};

use crate::framing;

unsafe fn bytes(env: *mut JNIEnv, array: jbyteArray) -> Option<Vec<u8>> {
    if array.is_null() {
        return None;
    }
    let f = unsafe { &**env };
    let len = unsafe { (f.GetArrayLength?)(env, array) };
    let mut out = vec![0u8; usize::try_from(len).ok()?];
    unsafe { (f.GetByteArrayRegion?)(env, array, 0, len, out.as_mut_ptr().cast()) };
    Some(out)
}

unsafe fn array(env: *mut JNIEnv, data: &[u8]) -> jbyteArray {
    let f = unsafe { &**env };
    let Ok(len) = jsize::try_from(data.len()) else {
        return ptr::null_mut();
    };
    let (Some(new), Some(set)) = (f.NewByteArray, f.SetByteArrayRegion) else {
        return ptr::null_mut();
    };
    let out = unsafe { new(env, len) };
    if !out.is_null() {
        unsafe { set(env, out, 0, len, data.as_ptr().cast()) };
    }
    out
}

fn utf8(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes).ok()
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike009_Core_peerOf(
    env: *mut JNIEnv,
    _: jclass,
    seed: jbyteArray,
) -> jbyteArray {
    match unsafe { bytes(env, seed) }.and_then(|s| framing::peer_of(&s)) {
        Some(peer) => unsafe { array(env, peer.as_bytes()) },
        None => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike009_Core_aad(
    env: *mut JNIEnv,
    _: jclass,
    policy: jint,
    peer: jbyteArray,
) -> jbyteArray {
    let Ok(policy) = u8::try_from(policy) else {
        return ptr::null_mut();
    };
    match unsafe { bytes(env, peer) } {
        Some(p) if utf8(&p).is_some() => unsafe {
            array(env, &framing::aad(policy, utf8(&p).unwrap_or_default()))
        },
        _ => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike009_Core_frame(
    env: *mut JNIEnv,
    _: jclass,
    policy: jint,
    iv: jbyteArray,
    sealed: jbyteArray,
) -> jbyteArray {
    let (Ok(policy), Some(iv), Some(sealed)) =
        (u8::try_from(policy), unsafe { bytes(env, iv) }, unsafe {
            bytes(env, sealed)
        })
    else {
        return ptr::null_mut();
    };
    match framing::frame(policy, &iv, &sealed) {
        Some(e) => unsafe { array(env, &e) },
        None => ptr::null_mut(),
    }
}

/// 0 when the header passes, else the `Refused` code.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike009_Core_check(
    env: *mut JNIEnv,
    _: jclass,
    envelope: jbyteArray,
) -> jint {
    match unsafe { bytes(env, envelope) } {
        Some(e) => framing::check(&e).map_or_else(|r| r as jint, |()| 0),
        None => framing::Refused::Length as jint,
    }
}

/// Part 0 the policy byte (one byte), 1 the IV, 2 the sealed bytes;
/// null when the header does not pass.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike009_Core_part(
    env: *mut JNIEnv,
    _: jclass,
    envelope: jbyteArray,
    which: jint,
) -> jbyteArray {
    let Some(e) = (unsafe { bytes(env, envelope) }) else {
        return ptr::null_mut();
    };
    match (framing::split(&e), which) {
        (Some((policy, _, _)), 0) => unsafe { array(env, &[policy]) },
        (Some((_, iv, _)), 1) => unsafe { array(env, iv) },
        (Some((_, _, sealed)), 2) => unsafe { array(env, sealed) },
        _ => ptr::null_mut(),
    }
}

/// 0 when the seed derives `peer`, else `WrongIdentity`'s code.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike009_Core_verify(
    env: *mut JNIEnv,
    _: jclass,
    seed: jbyteArray,
    peer: jbyteArray,
) -> jint {
    let (Some(seed), Some(peer)) = (unsafe { bytes(env, seed) }, unsafe { bytes(env, peer) })
    else {
        return framing::Refused::WrongIdentity as jint;
    };
    match utf8(&peer) {
        Some(p) => framing::verify(&seed, p).map_or_else(|r| r as jint, |()| 0),
        None => framing::Refused::WrongIdentity as jint,
    }
}

/// The PeerId a typed phrase restores, or null when the production
/// parse refuses it (D7's picker). The phrase's bytes are not retained
/// past the call; zeroing them is best-effort (a plain write the optimiser
/// may drop), and the Java caller's `String` copy is not cleared at all.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike009_Core_peerOfPhrase(
    env: *mut JNIEnv,
    _: jclass,
    words: jbyteArray,
) -> jbyteArray {
    let Some(mut w) = (unsafe { bytes(env, words) }) else {
        return ptr::null_mut();
    };
    let peer = utf8(&w).and_then(framing::peer_of_phrase);
    w.fill(0);
    match peer {
        Some(p) => unsafe { array(env, p.as_bytes()) },
        None => ptr::null_mut(),
    }
}
