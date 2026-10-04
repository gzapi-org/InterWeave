// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The human client's lock (Stage 15, R5): one desktop client per
//! profile, held in `human_dir()` beside its store, on the profile lock's
//! mechanism. What the mechanism itself does between real processes --
//! a child's lock blocking the parent, released when it is killed -- is
//! `tests/lock.rs`'s; what is checked here is what this lock adds: its
//! directory, its refusal, and that it is not the profile lock.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use interweave_profile_config::{
    DAEMON_LOCK_WAIT, HumanClientLock, OWNER_ONLY_DIR, OWNER_ONLY_FILE, PersistError, ProfileLock,
    ProfilePaths, XdgRoots, create_private_dir,
};

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

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).expect("meta").permissions().mode() & 0o777
}

/// A second client is refused while the first holds it -- as another
/// INSTANCE, not as the daemon -- and the lock comes back when the first
/// lets go.
#[test]
fn a_second_client_is_refused_while_the_first_holds_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    assert!(!HumanClientLock::is_held(&p).expect("probe"), "none yet");
    let first = HumanClientLock::acquire(&p, Duration::ZERO).expect("the first client");
    let refused = HumanClientLock::acquire(&p, Duration::ZERO).expect_err("the second");
    assert!(
        matches!(&refused, PersistError::InstanceLocked { path } if path == first.path()),
        "{refused:?}"
    );
    let said = refused.to_string();
    assert!(
        said.contains("another human client") && !said.contains("profile is in use"),
        "names the client, not the daemon: {said}"
    );
    assert!(HumanClientLock::is_held(&p).expect("probe"));
    drop(first);
    // Waited for briefly, as `tests/lock.rs` explains: a sibling test's
    // fork can share the descriptor for an instant.
    let deadline = std::time::Instant::now() + DAEMON_LOCK_WAIT;
    while HumanClientLock::is_held(&p).expect("probe") {
        assert!(std::time::Instant::now() < deadline, "released on drop");
        std::thread::sleep(Duration::from_millis(10));
    }
    HumanClientLock::acquire(&p, DAEMON_LOCK_WAIT).expect("free again");
}

/// The lock lives in `human_dir()`, created owner-only when missing, and
/// its file is owner-only and stays after release.
#[test]
fn it_lives_in_the_human_dir_created_owner_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    assert!(!p.human_dir().exists());
    let lock = HumanClientLock::acquire(&p, Duration::ZERO).expect("locks");
    assert_eq!(lock.path().parent(), Some(p.human_dir().as_path()));
    assert_eq!(lock.path(), HumanClientLock::path_for(&p));
    assert_eq!(mode(&p.human_dir()), OWNER_ONLY_DIR);
    assert_eq!(mode(lock.path()), OWNER_ONLY_FILE);
    let path = lock.path().to_path_buf();
    drop(lock);
    assert!(path.exists(), "released, never unlinked");
}

/// THE TWO LOCKS ARE TWO: a client holding its lock does not hold the
/// profile's, and a daemon holding the profile's does not hold the
/// client's -- the client runs beside the daemon, never instead of it.
#[test]
fn the_client_lock_and_the_profile_lock_do_not_exclude_each_other() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    let daemon = ProfileLock::acquire(&p, Duration::ZERO).expect("the daemon's");
    let client = HumanClientLock::acquire(&p, Duration::ZERO).expect("the client's beside it");
    assert_ne!(daemon.path(), client.path());
    assert!(HumanClientLock::is_held(&p).expect("probe"));
    assert!(ProfileLock::is_held(&p).expect("probe"));
}

/// An existing owner-only human dir is accepted, as the store's own
/// creation leaves it; one others can enter is refused, by the holder
/// and the probe alike -- the probe with the lock file present AND gone,
/// since a file removed through a wide directory leaves its holder
/// holding the old inode.
#[test]
fn a_wide_human_dir_is_refused_with_or_without_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    create_private_dir(&p.human_dir()).expect("the store's dir");
    drop(HumanClientLock::acquire(&p, Duration::ZERO).expect("an existing 0700 dir is fine"));
    std::fs::set_permissions(p.human_dir(), std::fs::Permissions::from_mode(0o750)).expect("chmod");
    assert!(matches!(
        HumanClientLock::acquire(&p, Duration::ZERO),
        Err(PersistError::DirectoryNotPrivate { .. })
    ));
    assert!(matches!(
        HumanClientLock::is_held(&p),
        Err(PersistError::DirectoryNotPrivate { .. })
    ));
    std::fs::remove_file(HumanClientLock::path_for(&p)).expect("the file gone");
    assert!(
        matches!(
            HumanClientLock::is_held(&p),
            Err(PersistError::DirectoryNotPrivate { .. })
        ),
        "no file is not 'not held' in a directory others can write"
    );
}

/// The state directory above `human_dir()` is judged too: whoever can
/// write it can rename `human_dir()` away and let a second client lock a
/// fresh one. Refused by the holder and the probe, naming the state
/// directory; the control is the same tree with the state directory
/// owner-only again, which locks.
#[test]
fn a_wide_state_directory_is_refused_for_the_client_too() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    create_private_dir(&p.human_dir()).expect("both dirs, owner-only");
    std::fs::set_permissions(p.state_dir(), std::fs::Permissions::from_mode(0o770)).expect("chmod");
    for refused in [
        HumanClientLock::acquire(&p, Duration::ZERO).map(drop),
        HumanClientLock::is_held(&p).map(drop),
    ] {
        assert!(
            matches!(&refused, Err(PersistError::DirectoryNotPrivate { path, .. }) if path == p.state_dir()),
            "{refused:?}"
        );
    }
    std::fs::set_permissions(p.state_dir(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    drop(HumanClientLock::acquire(&p, Duration::ZERO).expect("the control: owner-only again"));
}

/// With the state directory owner-only and no `human_dir()` in it, the
/// probe answers not held and creates nothing: an absent directory holds
/// no holder's file.
#[test]
fn an_absent_human_dir_is_not_held_and_not_created() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    create_private_dir(p.state_dir()).expect("state dir");
    assert!(!HumanClientLock::is_held(&p).expect("probe"));
    assert!(!p.human_dir().exists(), "the probe creates nothing");
}

/// The human dir, created private, for a test to plant things in.
fn human_dir(p: &ProfilePaths) -> std::path::PathBuf {
    create_private_dir(&p.human_dir()).expect("human dir");
    p.human_dir()
}

/// A planted link -- to an owner-only file, or to nothing -- is refused
/// before anything is written or created, by the holder and the probe.
#[test]
fn a_planted_or_dangling_symlink_is_refused_and_nothing_is_touched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    let target = dir.path().join("precious");
    std::fs::write(&target, b"keep me").expect("write");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let lock = HumanClientLock::path_for(&p);
    human_dir(&p);
    std::os::unix::fs::symlink(&target, &lock).expect("link");
    assert!(matches!(
        HumanClientLock::acquire(&p, Duration::ZERO),
        Err(PersistError::FileNotPrivate { .. })
    ));
    assert!(matches!(
        HumanClientLock::is_held(&p),
        Err(PersistError::FileNotPrivate { .. })
    ));
    assert_eq!(std::fs::read(&target).expect("read"), b"keep me");

    std::fs::remove_file(&lock).expect("unlink the link");
    let nowhere = dir.path().join("created-through-the-link");
    std::os::unix::fs::symlink(&nowhere, &lock).expect("dangling link");
    assert!(matches!(
        HumanClientLock::is_held(&p),
        Err(PersistError::FileNotPrivate { .. })
    ));
    assert!(matches!(
        HumanClientLock::acquire(&p, Duration::ZERO),
        Err(PersistError::FileNotPrivate { .. })
    ));
    assert!(!nowhere.exists(), "nothing was created through the link");
}

/// A hard-linked or wide lock file is refused on the opened handle; the
/// control is the same file, owner-only and single-linked, accepted.
#[test]
fn a_hard_linked_or_wide_lock_file_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    drop(HumanClientLock::acquire(&p, Duration::ZERO).expect("created"));
    let second = dir.path().join("second-name");
    std::fs::hard_link(HumanClientLock::path_for(&p), &second).expect("hard link");
    assert!(matches!(
        HumanClientLock::acquire(&p, Duration::ZERO),
        Err(PersistError::FileNotPrivate { .. })
    ));
    std::fs::remove_file(&second).expect("one name again");
    drop(HumanClientLock::acquire(&p, Duration::ZERO).expect("the control: single-linked"));
    std::fs::set_permissions(
        HumanClientLock::path_for(&p),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("chmod");
    assert!(matches!(
        HumanClientLock::acquire(&p, Duration::ZERO),
        Err(PersistError::FileNotPrivate { .. })
    ));
    assert!(matches!(
        HumanClientLock::is_held(&p),
        Err(PersistError::FileNotPrivate { .. })
    ));
}
