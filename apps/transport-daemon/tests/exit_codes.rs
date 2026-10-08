// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The daemon's exit codes on the command line alone (plan §16 (11)):
//! 0 for `--help`, 2 for a usage error, 1 for a refused start.
//!
//! Also why this package has integration tests at all: cargo builds a
//! package's binary for its integration tests, so `cargo test` over the
//! workspace leaves `transport-daemon` beside the test binaries, where
//! `tests/desktop-e2e` runs it.

#![cfg(unix)]
#![allow(clippy::expect_used)]

use std::process::Command;

fn daemon() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_transport-daemon"));
    // No XDG tree at all: a start gets as far as resolving it and no
    // further, whatever the host has.
    command.env_clear();
    command
}

#[test]
fn help_is_zero_and_a_usage_error_is_two() {
    let help = daemon().arg("--help").output().expect("runs");
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("--profile"));
    for args in [
        &[][..],
        &["--profile"][..],
        &["--profile", "a", "--bogus"][..],
    ] {
        let out = daemon().args(args).output().expect("runs");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn a_refused_start_is_one_and_says_why() {
    let tmp = private_tempdir().expect("tempdir");
    let out = daemon()
        .args(["--profile", "absent"])
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .env("XDG_STATE_HOME", tmp.path().join("state"))
        .env("XDG_CACHE_HOME", tmp.path().join("cache"))
        .env("XDG_RUNTIME_DIR", tmp.path())
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("transport-daemon: "),
        "the reason is printed"
    );
}

/// A temporary directory made `0700` at creation, whatever the umask: the
/// ancestor rule judges it, and `tempfile::tempdir()` under umask `002`
/// with a shared primary group is `0775`, refused (j37).
fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt as _;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
}
