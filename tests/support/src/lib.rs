// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Shared harness utilities for the InterWeave test suites.
//!
//! **Test-only.** No production crate may depend on this package (CLAUDE.md
//! §4). Everything here exists to make a test say what it means; none of it is
//! safe to have inside a shipped daemon.
//!
//! Stage 0 needs exactly one thing from it: the frozen vectors under
//! `fixtures/` must be loadable from Rust, without a network, a swarm, or a
//! product crate. `tools/checks/verify_fixture_vectors.py` already recomputes
//! those vectors and is the authority on whether they are *correct*; this
//! module answers the different question of whether an implementation can
//! *read* them, which is the half a Python checker cannot cover.
//!
//! Later stages add fake clocks, temporary profiles, peer/swarm factories,
//! daemon runners and eventual assertions here.

// A harness aborts loudly. A fixture that will not read is not a condition to
// propagate — it means the test cannot ask its question, and a `Result` handed
// back to a `#[test]` only moves the same panic one line down while inviting
// the caller to ignore it. The workspace ban on panicking is about paths
// reachable from untrusted remote input, which this crate never sees.
#![allow(clippy::expect_used, clippy::panic)]

pub mod fixtures;
pub mod hex;

use std::path::{Path, PathBuf};

/// The repository root, derived from this package's location.
///
/// Not from the current directory: `cargo test` runs each test binary with the
/// cwd set to its own package, so a relative `fixtures/` path resolves
/// differently depending on which suite is running.
#[must_use]
pub fn repo_root() -> PathBuf {
    // tests/support -> tests -> <root>
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| {
            panic!(
                "expected {} to sit two levels below the repository root",
                manifest.display()
            )
        })
        .to_path_buf()
}
