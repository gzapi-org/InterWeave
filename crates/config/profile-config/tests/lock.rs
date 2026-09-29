// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The profile lock (plan §16 (6), precondition P5), between real
//! processes: a second holder is refused while the first lives, a CHILD
//! process holding it blocks the parent, and the lock comes back when the
//! child is killed -- the kernel releases a flock with its holder, which
//! is what makes a crashed daemon no obstacle to its restart.
//!
//! The child is this test binary run again, selecting
//! `child_holds_the_lock_when_asked`, which does nothing unless the
//! parent names a state directory in `CHILD_STATE`.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use interweave_profile_config::{
    DAEMON_LOCK_WAIT, OWNER_ONLY_FILE, PersistError, ProfileLock, ProfilePaths, XdgRoots,
};

const CHILD_STATE: &str = "INTERWEAVE_TEST_LOCK_CHILD_STATE";

fn paths(base: &Path) -> ProfilePaths {
    let roots = XdgRoots {
        config_home: base.join("config"),
        data_home: base.join("data"),
        state_home: base.join("state"),
        cache_home: base.join("cache"),
        runtime_dir: None,
    };
    ProfilePaths::resolve_offline("work", &roots).expect("paths")
}

/// The child's side: hold the lock, say so, and wait to be killed.
#[test]
fn child_holds_the_lock_when_asked() {
    let Some(base) = std::env::var_os(CHILD_STATE) else {
        return;
    };
    let _held =
        ProfileLock::acquire(&paths(Path::new(&base)), Duration::ZERO).expect("child locks");
    println!("HELD");
    std::thread::sleep(Duration::from_secs(120));
}

#[test]
fn a_second_holder_is_refused_while_the_first_lives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    let first = ProfileLock::acquire(&p, Duration::ZERO).expect("the first holder");
    assert!(matches!(
        ProfileLock::acquire(&p, Duration::ZERO),
        Err(PersistError::ProfileLocked { .. })
    ));
    assert!(ProfileLock::is_held(&p).expect("probe"));
    drop(first);
    assert!(
        !ProfileLock::is_held(&p).expect("probe"),
        "released on drop"
    );
    ProfileLock::acquire(&p, Duration::ZERO).expect("free again");
}

/// Released, never unlinked; owner-only; the pid is diagnostic text.
#[test]
fn the_lock_file_stays_owner_only_and_names_its_holder() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    let lock = ProfileLock::acquire(&p, Duration::ZERO).expect("locks");
    let path = lock.path().to_path_buf();
    let text = std::fs::read_to_string(&path).expect("readable");
    assert!(
        text.starts_with(&format!("pid {}\n", std::process::id())),
        "{text}"
    );
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, OWNER_ONLY_FILE);
    drop(lock);
    assert!(path.exists(), "released, never unlinked");
}

/// A holder that lets go within the wait is waited for.
#[test]
fn an_acquisition_waits_out_a_momentary_holder() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    let brief = ProfileLock::acquire(&p, Duration::ZERO).expect("locks");
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(brief);
    });
    ProfileLock::acquire(&p, DAEMON_LOCK_WAIT).expect("waited for it");
    releaser.join().expect("joined");
}

#[test]
fn a_child_holding_the_lock_blocks_the_parent_until_it_is_killed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    let mut child = Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "child_holds_the_lock_when_asked",
            "--exact",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_STATE, dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the child starts");
    let stdout = child.stdout.take().expect("piped");
    let mut lines = BufReader::new(stdout).lines();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "the child never took the lock");
        let line = lines.next().expect("the child writes").expect("utf-8");
        // `contains`, not equality: with one test thread libtest prints
        // `test <name> ... ` without a newline before running it, so the
        // child's line arrives after that prefix.
        if line.contains("HELD") {
            break;
        }
    }

    assert!(
        matches!(
            ProfileLock::acquire(&p, Duration::from_millis(100)),
            Err(PersistError::ProfileLocked { .. })
        ),
        "the child's lock blocks the parent"
    );
    assert!(ProfileLock::is_held(&p).expect("probe"));

    child.kill().expect("killed");
    child.wait().expect("reaped");
    ProfileLock::acquire(&p, DAEMON_LOCK_WAIT).expect("the kernel released the dead holder's lock");
}

#[test]
fn a_state_directory_others_can_write_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    std::fs::create_dir_all(p.state_dir()).expect("mkdir");
    std::fs::set_permissions(p.state_dir(), std::fs::Permissions::from_mode(0o770)).expect("chmod");
    assert!(matches!(
        ProfileLock::acquire(&p, Duration::ZERO),
        Err(PersistError::DirectoryNotPrivate { .. })
    ));
}

#[test]
fn a_missing_lock_file_is_not_held() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(!ProfileLock::is_held(&paths(dir.path())).expect("probe"));
}
