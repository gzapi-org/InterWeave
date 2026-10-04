// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The shipped binary's start-up, run as a process: each refusal exits
//! with its sysexits(3) code and a message, before any window. A build
//! with no windowing backend gets as far as the window and says so, with
//! the store created and nothing left held.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use interweave_human_desktop::run::{EX_CONFIG, EX_DATAERR, EX_TEMPFAIL, EX_UNAVAILABLE, EX_USAGE};
use interweave_profile_config::{HumanClientLock, ProfilePaths, XdgRoots};
use interweave_profile_identity::ProfileIdentity;

const PROFILE: &str = "desk";

/// A private XDG tree, as the binary sees it.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        for sub in ["config", "data", "state", "cache", "run"] {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(dir.path().join(sub))
                .expect("xdg dir");
        }
        Self { dir }
    }

    fn root(&self, sub: &str) -> PathBuf {
        self.dir.path().join(sub)
    }

    fn roots(&self) -> XdgRoots {
        XdgRoots {
            config_home: self.root("config"),
            data_home: self.root("data"),
            state_home: self.root("state"),
            cache_home: self.root("cache"),
            runtime_dir: Some(self.root("run")),
        }
    }

    fn paths(&self) -> ProfilePaths {
        ProfilePaths::resolve(PROFILE, &self.roots()).expect("paths")
    }

    /// A valid profile document; `kinds` is the human endpoint's allowed
    /// client kinds.
    fn write_config(&self, kinds: &str) {
        let peer = ProfileIdentity::generate()
            .transport_identity()
            .expect("peer");
        let text = format!(
            "schema_version: 2
runtime: {{ deployment: daemon-ipc }}
profile: {{ name: {PROFILE} }}
transport:
  backend: libp2p
  listen: {{ addresses: [\"/ip4/127.0.0.1/tcp/0\"] }}
identity: {{ algorithm: ed25519 }}
trust: {{ policy: static-allowlist, allowed_peers: [\"{peer}\"] }}
endpoints:
  registration_policy: configured-only
  default_direct_endpoint: human
  directory: {{ enabled: true }}
  entries:
    - {{ id: human, enabled: true, advertise: true, allowed_client_kinds: [{kinds}], inbound: inherit_profile_trust, outbound: inherit_profile_trust }}
ipc: {{ enabled: true, socket_layout: split-data-admin }}
channels: {{ desired: [] }}
",
            peer = peer.as_str()
        );
        let file = self.paths().config_file();
        std::fs::create_dir_all(file.parent().expect("parent")).expect("config dir");
        std::fs::write(&file, text).expect("config");
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_human-desktop"))
            .args(args)
            .env_clear()
            .env("HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.root("config"))
            .env("XDG_DATA_HOME", self.root("data"))
            .env("XDG_STATE_HOME", self.root("state"))
            .env("XDG_CACHE_HOME", self.root("cache"))
            .env("XDG_RUNTIME_DIR", self.root("run"))
            .output()
            .expect("the binary runs")
    }

    fn store(&self) -> PathBuf {
        self.paths().human_dir().join("human.sqlite")
    }
}

fn code_and_message(out: &Output) -> (i32, String) {
    (
        out.status.code().expect("an exit code"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_command_line_needs_a_profile() {
    let home = Home::new();
    let (code, message) = code_and_message(&home.run(&[]));
    assert_eq!(code, i32::from(EX_USAGE), "{message}");
    assert!(message.contains("--profile"), "{message}");
}

#[test]
fn a_profile_with_no_configuration_is_refused() {
    let home = Home::new();
    let (code, message) = code_and_message(&home.run(&["--profile", PROFILE]));
    assert_eq!(code, i32::from(EX_CONFIG), "{message}");
    assert!(!home.store().exists(), "no store is created");
}

#[test]
fn a_profile_with_no_endpoint_for_this_client_is_refused() {
    let home = Home::new();
    home.write_config("claude-channel");
    let (code, message) = code_and_message(&home.run(&["--profile", PROFILE]));
    assert_eq!(code, i32::from(EX_CONFIG), "{message}");
    assert!(message.contains("human-client"), "{message}");
    assert!(!home.store().exists(), "no store is created");
}

#[test]
fn a_good_profile_gets_as_far_as_the_window_with_its_store_made_private() {
    let home = Home::new();
    home.write_config("human-client");
    let (code, message) = code_and_message(&home.run(&["--profile", PROFILE]));
    assert_eq!(code, i32::from(EX_UNAVAILABLE), "{message}");
    assert!(message.contains("window"), "{message}");
    let store = home.store();
    assert!(
        store.exists(),
        "the store was opened at {}",
        store.display()
    );
    let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode(&store), 0o600);
    assert_eq!(mode(&home.paths().human_dir()), 0o700);
    // Nothing is left held: a second run gets just as far.
    let (again, message) = code_and_message(&home.run(&["--profile", PROFILE]));
    assert_eq!(again, i32::from(EX_UNAVAILABLE), "{message}");
}

#[test]
fn a_store_this_version_cannot_read_is_left_alone_and_needs_recovery() {
    let home = Home::new();
    home.write_config("human-client");
    let dir = home.paths().human_dir();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .expect("human dir");
    let store = home.store();
    std::fs::write(&store, b"this is not a database").expect("garbage");
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600)).expect("mode");
    let (code, message) = code_and_message(&home.run(&["--profile", PROFILE]));
    assert_eq!(code, i32::from(EX_DATAERR), "{message}");
    assert!(message.contains("recovery"), "{message}");
    assert_eq!(
        std::fs::read(&store).expect("still there"),
        b"this is not a database",
        "never renamed, moved, rewritten or deleted"
    );
}

#[test]
fn a_second_window_on_the_same_profile_is_refused() {
    let home = Home::new();
    home.write_config("human-client");
    let _held = HumanClientLock::acquire(&home.paths(), Duration::ZERO).expect("the first window");
    let (code, message) = code_and_message(&home.run(&["--profile", PROFILE]));
    assert_eq!(code, i32::from(EX_TEMPFAIL), "{message}");
    assert!(message.contains("already open"), "{message}");
    assert!(
        !message.contains("human-desktop.lock"),
        "what, not why: no lock path: {message}"
    );
    assert!(
        !home.store().exists(),
        "the store is not opened without the lock"
    );
}
