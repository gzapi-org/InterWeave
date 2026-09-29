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
    // RELEASED ON DROP, WAITED FOR BRIEFLY. A sibling test forks a child
    // process, and between its fork and exec the child shares this
    // process's open file descriptions -- the lock's included -- so the
    // release can lag the drop by that instant (seen once under a full
    // run). The lock's own users retry the same way (`DAEMON_LOCK_WAIT`).
    let deadline = std::time::Instant::now() + DAEMON_LOCK_WAIT;
    while ProfileLock::is_held(&p).expect("probe") {
        assert!(std::time::Instant::now() < deadline, "released on drop");
        std::thread::sleep(Duration::from_millis(10));
    }
    ProfileLock::acquire(&p, DAEMON_LOCK_WAIT).expect("free again");
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

/// The state directory, created private, for a test to plant things in.
fn state_dir(p: &ProfilePaths) -> &Path {
    interweave_profile_config::create_private_dir(p.state_dir()).expect("state dir");
    p.state_dir()
}

/// A `profile.lock` planted as a link is refused before anything is
/// written, and what it points at is left as it was (#145 review F1): the
/// target here is owner-only, the case a path check passed.
#[test]
fn a_planted_symlink_is_refused_and_its_target_untouched() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    let target = dir.path().join("precious");
    std::fs::write(&target, b"keep me").expect("write");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    std::os::unix::fs::symlink(&target, state_dir(&p).join("profile.lock")).expect("link");

    assert!(matches!(
        ProfileLock::acquire(&p, Duration::ZERO),
        Err(PersistError::FileNotPrivate { .. })
    ));
    assert!(matches!(
        ProfileLock::is_held(&p),
        Err(PersistError::FileNotPrivate { .. })
    ));
    assert_eq!(std::fs::read(&target).expect("read"), b"keep me");
}

/// A DANGLING link is refused too, by the probe as well as the holder:
/// following it read as "no lock file", i.e. "not running" (#145
/// re-review 2), and creating through it would make a file wherever it
/// points.
#[test]
fn a_dangling_symlink_is_refused_and_creates_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    let nowhere = dir.path().join("created-through-the-link");
    std::os::unix::fs::symlink(&nowhere, state_dir(&p).join("profile.lock")).expect("link");
    assert!(matches!(
        ProfileLock::is_held(&p),
        Err(PersistError::FileNotPrivate { .. })
    ));
    assert!(matches!(
        ProfileLock::acquire(&p, Duration::ZERO),
        Err(PersistError::FileNotPrivate { .. })
    ));
    assert!(!nowhere.exists(), "nothing was created through the link");
}

/// A second name for the lock file -- a hard link, which a path check
/// cannot tell from the file itself -- is refused on the opened handle.
#[test]
fn a_hard_linked_lock_file_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    drop(ProfileLock::acquire(&p, Duration::ZERO).expect("created"));
    std::fs::hard_link(ProfileLock::path_for(&p), dir.path().join("second-name"))
        .expect("hard link");
    assert!(matches!(
        ProfileLock::acquire(&p, Duration::ZERO),
        Err(PersistError::FileNotPrivate { .. })
    ));
}

/// A lock file wider than owner-only is refused rather than narrowed (#145
/// review F2), by the holder and the probe alike.
#[test]
fn a_lock_file_wider_than_owner_only_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    let lock = state_dir(&p).join("profile.lock");
    std::fs::write(&lock, b"").expect("write");
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert!(matches!(
        ProfileLock::acquire(&p, Duration::ZERO),
        Err(PersistError::FileNotPrivate { .. })
    ));
    assert!(matches!(
        ProfileLock::is_held(&p),
        Err(PersistError::FileNotPrivate { .. })
    ));
}
