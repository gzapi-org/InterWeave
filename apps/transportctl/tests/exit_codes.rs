// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `transportctl`'s exit codes with no daemon (plan §16 (10), (11)): 0 for
//! `--help`, 2 for a usage error, 3 when no daemon answers -- and the lock
//! probe telling "no daemon" from "a daemon whose socket is gone". The
//! commands against a live daemon are `tests/desktop-e2e`'s.

#![cfg(unix)]
#![allow(clippy::expect_used)]

use std::process::{Command, Output};
use std::time::Duration;

use interweave_profile_config::{ProfileLock, ProfilePaths, XdgRoots};

struct Home {
    root: tempfile::TempDir,
    roots: XdgRoots,
}

impl Home {
    fn new() -> Self {
        let root = private_tempdir().expect("tempdir");
        let at = |name: &str| root.path().join(name);
        let roots = XdgRoots {
            config_home: at("config"),
            data_home: at("data"),
            state_home: at("state"),
            cache_home: at("cache"),
            runtime_dir: Some(at("run")),
        };
        Self { root, roots }
    }

    fn run(&self, args: &[&str]) -> Output {
        let at = |name: &str| self.root.path().join(name);
        Command::new(env!("CARGO_BIN_EXE_transportctl"))
            .args(args)
            .env_clear()
            .env("XDG_CONFIG_HOME", at("config"))
            .env("XDG_DATA_HOME", at("data"))
            .env("XDG_STATE_HOME", at("state"))
            .env("XDG_CACHE_HOME", at("cache"))
            .env("XDG_RUNTIME_DIR", at("run"))
            .output()
            .expect("runs")
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn help_is_zero_and_a_usage_error_is_two() {
    let home = Home::new();
    let help = home.run(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("identity restore"));
    for args in [
        &[][..],
        &["status"][..],
        &["--profile", "p", "frobnicate"][..],
        &["--profile", "p", "shutdown", "--grace", "soon"][..],
    ] {
        let out = home.run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
    }
}

/// Every admin command, with no daemon and its lock free, is 3 and says
/// no daemon runs.
#[test]
fn no_daemon_is_three_for_every_admin_command() {
    let home = Home::new();
    for args in [
        &["status"][..],
        &["status", "--json"][..],
        &["endpoints", "list"][..],
        &["endpoints", "revoke", "human"][..],
        &["endpoints", "enable", "human"][..],
        &["endpoints", "disable", "human"][..],
        &["endpoints", "default", "--none"][..],
        &["trust", "list"][..],
        &["trust", "list", "--json"][..],
        &["peers", "list"][..],
        &["peers", "list", "--json"][..],
        &[
            "trust",
            "revoke",
            "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN",
        ][..],
        &["shutdown"][..],
    ] {
        let out = home.run(&[&["--profile", "p"][..], args].concat());
        assert_eq!(out.status.code(), Some(3), "{args:?}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("no daemon is running for profile \"p\""),
            "{args:?}: {}",
            stderr(&out)
        );
    }
}

/// The lock probe: with the profile's lock held and no socket, the
/// unreachable daemon is named as one whose socket does not answer --
/// and the probe takes nothing from the holder.
#[test]
fn a_held_lock_with_no_socket_is_three_naming_the_socket() {
    let home = Home::new();
    let paths = ProfilePaths::resolve("p", &home.roots).expect("paths");
    let lock = ProfileLock::acquire(&paths, Duration::ZERO).expect("the lock");
    let out = home.run(&["--profile", "p", "status"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("holds profile \"p\"'s lock"),
        "{}",
        stderr(&out)
    );
    assert!(
        ProfileLock::is_held(&paths).expect("probed"),
        "still held by this test"
    );
    drop(lock);
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
