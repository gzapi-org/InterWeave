// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The SPIKE-009 device harness's Rust core: the PROPOSED v1 envelope's
//! framing (`spikes/spike-009/harness/src/envelope.rs`, the host half)
//! with the AES-256-GCM step left to AndroidKeyStore.
//!
//! The Keystore cipher takes the associated data this module builds and
//! returns its own IV and the ciphertext with the tag appended; this
//! module frames them, checks a stored envelope's header before any
//! decryption, and re-derives the PeerId from a decrypted seed. Its
//! layout test (`tests/layout.rs`) wraps with the host half's own code
//! and requires these functions to reproduce the bytes and authenticate.

pub mod framing;

mod jni;
