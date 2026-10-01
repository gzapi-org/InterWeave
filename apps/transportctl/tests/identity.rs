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

use std::fs::DirBuilder;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use interweave_profile_config::{ProfileLock, ProfilePaths, XdgRoots};
use interweave_profile_identity::{ProfileIdentity, RecoveryRecord};

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

/// One user's XDG tree with profile `p` configured; its key optional.
struct Home {
    root: tempfile::TempDir,
    paths: ProfilePaths,
}

fn private_dir(path: &Path) {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .expect("a private directory");
}

impl Home {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let at = |name: &str| root.path().join(name);
        let roots = XdgRoots {
            config_home: at("config"),
            data_home: at("data"),
            state_home: at("state"),
            cache_home: at("cache"),
            runtime_dir: Some(at("run")),
        };
        let paths = ProfilePaths::resolve("p", &roots).expect("paths");
        let config = paths.config_file();
        private_dir(config.parent().expect("a config directory"));
        std::fs::write(
            &config,
            "schema_version: 2
profile: { name: p }
trust: { policy: static-allowlist, allowed_peers: [] }
endpoints:
  entries:
    - { id: human, enabled: true, advertise: false }
channels: { desired: [] }
discovery: { providers: [] }
transport: { listen: { addresses: [\"/ip4/127.0.0.1/tcp/0\"] } }
",
        )
        .expect("the profile written");
        Self { root, paths }
    }

    /// A key where the profile's default names it; its `PeerId`.
    fn write_key(&self) -> String {
        let identity = ProfileIdentity::generate();
        let file = self.paths.identity_file();
        private_dir(file.parent().expect("an identity directory"));
        identity.save(&file).expect("the key saved");
        identity
            .transport_identity()
            .expect("a peer id")
            .as_str()
            .to_owned()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self, args: &[&str]) -> Command {
        let at = |name: &str| self.root.path().join(name);
        let mut command = Command::new(transportctl());
        command
            .args(args)
            .env_clear()
            .env("XDG_CONFIG_HOME", at("config"))
            .env("XDG_DATA_HOME", at("data"))
            .env("XDG_STATE_HOME", at("state"))
            .env("XDG_CACHE_HOME", at("cache"))
            .env("XDG_RUNTIME_DIR", at("run"));
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args)
            .stdin(Stdio::null())
            .output()
            .expect("runs")
    }

    /// The same environment as a prefix for a `script(1)` shell line.
    fn shell_env(&self) -> String {
        let at = |name: &str| self.root.path().join(name).display().to_string();
        format!(
            "XDG_CONFIG_HOME={} XDG_DATA_HOME={} XDG_STATE_HOME={} XDG_CACHE_HOME={} \
             XDG_RUNTIME_DIR={}",
            at("config"),
            at("data"),
            at("state"),
            at("cache"),
            at("run")
        )
    }
}

/// To a new file: owner-only whatever the umask, a record that restores
/// the profile's own `PeerId`, and no word of it anywhere but the file.
#[test]
fn a_backup_to_a_new_file_is_owner_only_and_restores_the_profile() {
    let home = Home::new();
    let peer = home.write_key();
    let file = home.file("record.json");
    let out = home.run(&[
        "--profile",
        "p",
        "identity",
        "backup",
        "--to-file",
        file.to_str().expect("utf-8"),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        std::fs::metadata(&file).expect("written").mode() & 0o777,
        0o600
    );
    let record: RecoveryRecord =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("a record");
    assert_eq!(record.expected_peer_id.as_deref(), Some(peer.as_str()));
    let restored = record.restore().expect("restores");
    assert_eq!(
        restored.transport_identity().expect("a peer id").as_str(),
        peer
    );
    assert!(text(&out.stdout).contains(&peer), "{}", text(&out.stdout));
    for word in &record.words {
        assert!(
            !text(&out.stdout).split_whitespace().any(|w| w == word)
                && !text(&out.stderr).split_whitespace().any(|w| w == word),
            "a word of the phrase outside the file"
        );
    }
}

/// Not a terminal and no file: refused, and nothing written to the pipe.
#[test]
fn a_backup_to_a_pipe_is_refused() {
    let home = Home::new();
    home.write_key();
    let out = home.run(&["--profile", "p", "identity", "backup"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "nothing on the pipe");
    assert!(
        text(&out.stderr).contains("not a terminal"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn a_backup_never_overwrites_a_file() {
    let home = Home::new();
    home.write_key();
    let file = home.file("existing.json");
    std::fs::write(&file, b"kept").expect("planted");
    let out = home.run(&[
        "--profile",
        "p",
        "identity",
        "backup",
        "--to-file",
        file.to_str().expect("utf-8"),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(std::fs::read(&file).expect("read"), b"kept");
}

/// The lock held -- a running daemon -- refuses the backup before the key
/// is read, and writes nothing.
#[test]
fn a_backup_under_a_held_lock_is_refused() {
    let home = Home::new();
    home.write_key();
    let lock = ProfileLock::acquire(&home.paths, Duration::ZERO).expect("the lock");
    let file = home.file("record.json");
    let out = home.run(&[
        "--profile",
        "p",
        "identity",
        "backup",
        "--to-file",
        file.to_str().expect("utf-8"),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("lock is held"),
        "{}",
        text(&out.stderr)
    );
    assert!(!file.exists(), "nothing written");
    drop(lock);
}

/// On a terminal, the record is shown there.
#[test]
fn a_backup_on_a_terminal_shows_the_record() {
    let home = Home::new();
    let peer = home.write_key();
    let shell = format!(
        "{} {} --profile p identity backup; echo done=$?",
        home.shell_env(),
        transportctl()
    );
    let session = under_a_terminal(&shell, "done=", b"");
    assert!(session.screen.contains("done=0"), "{}", session.screen);
    assert!(
        session
            .screen
            .contains(&format!("\"expected_peer_id\": \"{peer}\"")),
        "{}",
        session.screen
    );
}
