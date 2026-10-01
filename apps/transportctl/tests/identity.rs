// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The identity commands as a process: the phrase on stdin, never argv;
//! hidden on a terminal; a refusal that never repeats it.
//!
//! The terminal cases run the binary under `script(1)`, which gives it a
//! real pseudo-terminal: what the terminal echoes comes back in the
//! output, so a phrase typed with echo on is visible there.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::io::{Read as _, Write as _};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// IDENTITY-RECOVERY.md's golden fixture: test-only, never a key.
const GOLDEN_WORDS: &str = "abandon abandon abandon abandon abandon abandon abandon abandon \
     abandon abandon abandon abandon abandon abandon abandon abandon \
     abandon abandon abandon abandon abandon abandon abandon art";
const GOLDEN_PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

fn transportctl() -> &'static str {
    env!("CARGO_BIN_EXE_transportctl")
}

fn other_peer() -> String {
    interweave_profile_identity::ProfileIdentity::generate()
        .transport_identity()
        .expect("a peer id")
        .as_str()
        .to_owned()
}

/// Run with `input` piped to stdin and no environment: verify needs none.
fn piped(args: &[&str], input: &str) -> Output {
    let mut child = Command::new(transportctl())
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("runs");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("written");
    child.wait_with_output().expect("ends")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn verify_reads_a_piped_phrase_and_compares() {
    let out = piped(
        &["identity", "verify", "--expected-peer-id", GOLDEN_PEER],
        &format!("{GOLDEN_WORDS}\n"),
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains(&format!("verified: the phrase restores {GOLDEN_PEER}")));

    let other = other_peer();
    let out = piped(
        &["identity", "verify", "--expected-peer-id", &other],
        &format!("{GOLDEN_WORDS}\n"),
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("a different identity"), "{stderr}");
    assert!(
        !text(&out.stdout).contains("abandon") && !stderr.contains("abandon"),
        "the refusal repeats no word: {stderr}"
    );
}

#[test]
fn verify_takes_the_peer_from_a_piped_record() {
    let words: Vec<&str> = GOLDEN_WORDS.split_whitespace().collect();
    let record = serde_json::json!({
        "format": interweave_profile_identity::FORMAT,
        "identity_algorithm": interweave_profile_identity::ALGORITHM,
        "expected_peer_id": GOLDEN_PEER,
        "words": words,
    });
    let out = piped(&["identity", "verify"], &record.to_string());
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(!text(&out.stdout).contains("abandon"), "nothing echoed");
}

/// What `script(1)` returned, and everything the terminal showed.
struct Session {
    code: Option<i32>,
    screen: String,
}

/// Run `shell` under `script(1)`; once `prompt` is on the screen, type
/// `keys`. The settle after the prompt is because `rpassword` prints its
/// prompt BEFORE it switches echo off: keys arriving in between would be
/// echoed by the terminal, which is the race, not the property.
fn under_a_terminal(shell: &str, prompt: &str, keys: &[u8]) -> Session {
    let mut child = Command::new("script")
        .args(["-qec", shell, "/dev/null"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TERM", "dumb")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("script(1) runs");
    let screen = Arc::new(Mutex::new(Vec::new()));
    let reader = {
        let screen = Arc::clone(&screen);
        let mut stdout = child.stdout.take().expect("stdout");
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 1024];
            while let Ok(n) = stdout.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                screen.lock().expect("lock").extend_from_slice(&chunk[..n]);
            }
        })
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !text(&screen.lock().expect("lock")).contains(prompt) {
        assert!(
            Instant::now() < deadline,
            "no prompt: {}",
            text(&screen.lock().expect("lock"))
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(500));
    let mut stdin = child.stdin.take().expect("stdin");
    stdin.write_all(keys).expect("typed");
    stdin.flush().expect("flushed");
    let code = loop {
        if let Some(status) = child.try_wait().expect("waits") {
            break status.code();
        }
        assert!(Instant::now() < deadline, "script(1) did not end");
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    reader.join().expect("reader");
    let screen = text(&screen.lock().expect("lock"));
    Session { code, screen }
}

/// On a terminal the phrase is read with echo off: typed in full, the
/// command verifies it, and not one word of it is on the screen.
#[test]
fn a_phrase_typed_on_a_terminal_is_not_echoed() {
    let shell = format!(
        "{} identity verify --expected-peer-id {GOLDEN_PEER}",
        transportctl()
    );
    let session = under_a_terminal(
        &shell,
        "recovery phrase (hidden)",
        format!("{GOLDEN_WORDS}\r").as_bytes(),
    );
    assert_eq!(session.code, Some(0), "{}", session.screen);
    assert!(session.screen.contains("verified"), "{}", session.screen);
    assert!(
        !session.screen.contains("abandon"),
        "the phrase was echoed: {}",
        session.screen
    );
}

/// Ctrl-C at the prompt ends the command AND leaves the terminal as it
/// found it: `rpassword` raises SIGINT before restoring echo, so without
/// the handler the process dies with echo off and `stty` says `-echo`.
#[test]
fn ctrl_c_at_the_prompt_restores_the_terminal() {
    let shell = format!(
        "{} identity verify --expected-peer-id {GOLDEN_PEER}; echo exit=$?; stty -a",
        transportctl()
    );
    let session = under_a_terminal(&shell, "recovery phrase (hidden)", b"aban\x03");
    assert!(session.screen.contains("exit=1"), "{}", session.screen);
    assert!(session.screen.contains("interrupted"), "{}", session.screen);
    let modes: Vec<&str> = session.screen.split_whitespace().collect();
    assert!(
        modes.contains(&"echo") && !modes.contains(&"-echo"),
        "echo restored: {}",
        session.screen
    );
    assert!(
        modes.contains(&"isig") && modes.contains(&"icanon"),
        "the rest restored too: {}",
        session.screen
    );
}
