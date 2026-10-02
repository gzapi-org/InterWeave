// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Compiles the views. Debug info outside release builds: the testing
//! backend's element search reads it, and without it the accessibility
//! tree a test walks is empty -- so a test of the tree would pass having
//! seen nothing, were it not for the tests' own non-empty checks.

#![allow(clippy::expect_used)]

fn main() {
    let release = std::env::var("PROFILE").is_ok_and(|p| p == "release");
    let config = slint_build::CompilerConfiguration::new().with_debug_info(!release);
    slint_build::compile_with_config("ui/app.slint", config).expect("the views compile");
}
