// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The desktop harness, under the name every suite here imports: it lives
//! in `tests/support`'s `e2e` module since `tests/android-e2e` became its
//! second consumer (plan §20 gate (c)).

// Each test binary includes this module and uses a part of it.
#![allow(unused_imports)]

pub(crate) use interweave_test_support::e2e::*;

// The schema validator stays out of the support crate (its file says why).
#[path = "../../../support/shared/schema_validator.rs"]
#[allow(
    dead_code,
    reason = "each test binary includes it; not every one validates"
)]
mod schema;
pub(crate) use schema::schema_validator;
