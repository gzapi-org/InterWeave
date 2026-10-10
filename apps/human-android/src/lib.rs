// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Android human client's native library (plan section 20): what the
//! APK's Activity runs as `android_main`, and the entry points its Service
//! calls. Thin by rule, as the desktop root is.

#[cfg(target_os = "android")]
mod activity;
