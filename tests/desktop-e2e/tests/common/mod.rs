// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The desktop harness, under the name every suite here imports: it lives
//! in `tests/support`'s `e2e` module since `tests/android-e2e` became its
//! second consumer (plan §20 gate (c)).

// Each test binary includes this module and uses a part of it.
#![allow(unused_imports)]

pub(crate) use interweave_test_support::e2e::*;
