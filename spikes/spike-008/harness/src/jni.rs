// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `org.interweave.spike008.Core`'s natives over raw JNI: a path in as
//! UTF-8 bytes, a JSON census (or `{"error":..}`) out. Nothing throws,
//! so no Java exception is left pending.

#![allow(unsafe_code, clippy::missing_safety_doc)]

use std::path::Path;
use std::ptr;

use jni_sys::{JNIEnv, jbyteArray, jclass, jsize};

use crate::store;

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

unsafe fn run(
    env: *mut JNIEnv,
    path: jbyteArray,
    op: fn(&Path) -> Result<String, String>,
) -> jbyteArray {
    let out = match unsafe { bytes(env, path) }.and_then(|p| String::from_utf8(p).ok()) {
        Some(p) => op(Path::new(&p)).unwrap_or_else(|e| format!("{{\"error\":{e:?}}}")),
        None => "{\"error\":\"no path\"}".to_owned(),
    };
    unsafe { array(env, out.as_bytes()) }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike008_Core_seed(
    env: *mut JNIEnv,
    _: jclass,
    path: jbyteArray,
) -> jbyteArray {
    unsafe { run(env, path, store::seed) }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike008_Core_transitions(
    env: *mut JNIEnv,
    _: jclass,
    path: jbyteArray,
) -> jbyteArray {
    unsafe { run(env, path, store::transitions) }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike008_Core_census(
    env: *mut JNIEnv,
    _: jclass,
    path: jbyteArray,
) -> jbyteArray {
    unsafe { run(env, path, store::census) }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike008_Core_redeliver(
    env: *mut JNIEnv,
    _: jclass,
    path: jbyteArray,
) -> jbyteArray {
    unsafe { run(env, path, store::redeliver) }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike008_Core_fill(
    env: *mut JNIEnv,
    _: jclass,
    path: jbyteArray,
) -> jbyteArray {
    unsafe { run(env, path, store::fill) }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_org_interweave_spike008_Core_forensic(
    env: *mut JNIEnv,
    _: jclass,
    path: jbyteArray,
) -> jbyteArray {
    unsafe { run(env, path, store::forensic) }
}
