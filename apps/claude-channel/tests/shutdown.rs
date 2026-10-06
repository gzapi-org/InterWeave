// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The first signal is final (`plugin/LIFECYCLE.md` §Shutdown): Claude
//! Code sends SIGINT and never closes stdin, so the process must end on
//! the signal alone. The real binary, its stdin a pipe held open.

#![cfg(unix)]
#![allow(clippy::expect_used)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Generous against a loaded host; the host's own SIGTERM follows the
/// SIGINT after about 100 ms, so a pass well inside this is the claim.
const PROMPT: Duration = Duration::from_secs(2);

fn bridge(scratch: &std::path::Path) -> Child {
    let run = scratch.join("run");
    std::fs::create_dir_all(&run).expect("run dir");
    Command::new(env!("CARGO_BIN_EXE_claude-channel"))
        .args(["--profile", "shutdown", "--endpoint", "claude"])
        .env("XDG_RUNTIME_DIR", &run)
        .env("XDG_STATE_HOME", scratch.join("state"))
        .env("XDG_CONFIG_HOME", scratch.join("config"))
        .env("XDG_DATA_HOME", scratch.join("data"))
        .env("XDG_CACHE_HOME", scratch.join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawns")
}

fn exited_within(child: &mut Child, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if child.try_wait().expect("waitable").is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn the_first_sigint_ends_the_process_with_stdin_held_open() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let mut child = bridge(scratch.path());
    let _stdin = child.stdin.take().expect("held open");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !exited_within(&mut child, Duration::from_millis(500)),
        "the control: with stdin open and no signal it keeps serving"
    );
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(killed.success());
    let ended = exited_within(&mut child, PROMPT);
    if !ended {
        let _ = child.kill();
    }
    assert!(ended, "the first SIGINT ended it, stdin still open");
}
