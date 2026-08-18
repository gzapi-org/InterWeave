// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Path separation and atomic owner-only writes.
//!
//! Two properties that fail silently when they break: a layout whose
//! roles collapsed still runs perfectly until a cache clear takes the
//! identity key with it, and a key file created world-readable is
//! functionally identical to one that is not.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use interweave_profile_config::{
    OWNER_ONLY_DIR, OWNER_ONLY_FILE, PersistError, ProfilePaths, XdgRoots, absolute_or_none,
    create_private_dir, is_owner_only, write_atomic, write_private_atomic,
};

fn roots(base: &Path) -> XdgRoots {
    XdgRoots {
        config_home: base.join("config"),
        data_home: base.join("data"),
        state_home: base.join("state"),
        cache_home: base.join("cache"),
        runtime_dir: Some(base.join("run")),
    }
}

fn paths(base: &Path) -> ProfilePaths {
    ProfilePaths::resolve("default", &roots(base)).expect("resolve")
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn the_five_roles_land_in_five_distinct_places() {
    // A layout whose roles collapsed runs perfectly right up until a
    // cache clear deletes the identity key.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    assert!(p.roles_are_distinct());

    let all: Vec<PathBuf> = vec![
        p.config_file(),
        p.identity_file(),
        p.state_dir().to_path_buf(),
        p.peer_cache_file(),
        p.data_socket(),
    ];
    for (i, a) in all.iter().enumerate() {
        for b in all.iter().skip(i + 1) {
            assert_ne!(a, b, "two roles resolved to the same path");
        }
    }
}

#[test]
fn a_collapsed_layout_is_reported_rather_than_tolerated() {
    // The environment can point two XDG variables at the same directory.
    // Nothing else in the system would notice.
    let dir = tempfile::tempdir().expect("tempdir");
    let same = dir.path().join("everything");
    let collapsed = XdgRoots {
        config_home: same.clone(),
        data_home: same.clone(),
        state_home: same.clone(),
        cache_home: same.clone(),
        runtime_dir: Some(dir.path().join("run")),
    };
    let p = ProfilePaths::resolve("default", &collapsed).expect("resolve");
    assert!(
        !p.roles_are_distinct(),
        "a caller must be able to refuse to start on a collapsed layout"
    );
}

#[test]
fn the_data_and_admin_sockets_are_different_files() {
    // The two IPC boundaries carry different authority. One socket
    // serving both would make that a runtime check instead of a
    // filesystem fact.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    assert_ne!(p.data_socket(), p.admin_socket());
}

#[test]
fn a_profile_name_cannot_escape_or_hide_in_a_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let r = roots(dir.path());
    for bad in [
        "",
        "..",
        "../etc",
        "a/b",
        "a\\b",
        ".hidden",
        "with space",
        "sym*link",
        &"x".repeat(65),
    ] {
        let err = ProfilePaths::resolve(bad, &r)
            .err()
            .unwrap_or_else(|| panic!("{bad:?} must be refused"));
        assert!(
            matches!(err, PersistError::InvalidProfileName { .. }),
            "{bad:?}: unexpected {err}"
        );
    }
    for good in ["default", "work", "a", "test_2", "a-b"] {
        assert!(ProfilePaths::resolve(good, &r).is_ok(), "{good:?}");
    }
}

#[test]
fn a_missing_runtime_dir_is_fatal_rather_than_defaulted() {
    // Its guarantees — owner-only, per-user, per-boot — are exactly what
    // an IPC socket relies on, so inventing /tmp would drop all three.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut r = roots(dir.path());
    r.runtime_dir = None;
    assert!(matches!(
        ProfilePaths::resolve("default", &r),
        Err(PersistError::NoRuntimeDir)
    ));
}

#[test]
#[cfg(unix)]
fn a_private_write_is_owner_only_from_the_moment_it_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    write_private_atomic(&path, b"not a real key").expect("write");

    assert_eq!(mode_of(&path), OWNER_ONLY_FILE);
    assert!(is_owner_only(&path).expect("check"));
}

#[test]
#[cfg(unix)]
fn a_private_directory_is_owner_only() {
    // A 0600 key inside a world-executable directory still leaks its
    // existence, size, and modification time.
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("a").join("b").join("c");
    create_private_dir(&target).expect("create");
    assert_eq!(mode_of(&target), OWNER_ONLY_DIR);
}

#[test]
#[cfg(unix)]
fn is_owner_only_rejects_a_key_file_someone_else_can_read() {
    // A file written correctly by this build may still have been
    // restored from a backup or copied from somewhere less careful.
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    write_private_atomic(&path, b"not a real key").expect("write");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("loosen");
    assert!(
        !is_owner_only(&path).expect("check"),
        "a caller must be able to refuse to load a readable key"
    );
}

#[test]
fn an_atomic_write_replaces_the_previous_contents_completely() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.yaml");
    write_atomic(&path, b"schema_version: 2\nlong original content\n").expect("first");
    write_atomic(&path, b"short\n").expect("second");
    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"short\n",
        "a shorter replacement must not leave a tail of the old file"
    );
}

#[test]
fn an_atomic_write_leaves_no_temporary_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.yaml");
    write_atomic(&path, b"schema_version: 2\n").expect("write");

    let names: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["config.yaml".to_owned()], "left: {names:?}");
}

#[test]
fn an_atomic_write_creates_missing_parents() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deep").join("nested").join("config.yaml");
    write_atomic(&path, b"schema_version: 2\n").expect("write");
    assert!(path.exists());
}

#[test]
#[cfg(unix)]
fn the_identity_file_and_the_config_file_get_different_protection() {
    // Configuration is what a user edits and backs up; the identity key
    // is private-key-equivalent. Writing both the same way would mean one
    // policy silently applies to both.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    create_private_dir(p.identity_dir()).expect("identity dir");
    write_private_atomic(&p.identity_file(), b"not a real key").expect("key");
    write_atomic(&p.config_file(), b"schema_version: 2\n").expect("config");

    assert!(is_owner_only(&p.identity_file()).expect("check"));
    assert_eq!(mode_of(p.identity_dir()), OWNER_ONLY_DIR);
}

#[test]
fn a_relative_xdg_value_is_dropped_rather_than_resolved() {
    // It would resolve against the daemon's working directory — not a
    // location any user chose, and one that changes with how the daemon
    // was started. The failure is invisible: the daemon runs fine and
    // writes the profile somewhere arbitrary.
    use std::ffi::OsString;
    assert_eq!(absolute_or_none(None), None);
    assert_eq!(
        absolute_or_none(Some(OsString::from("relative/path"))),
        None
    );
    assert_eq!(absolute_or_none(Some(OsString::from(""))), None);
    assert_eq!(absolute_or_none(Some(OsString::from("./here"))), None);
    assert_eq!(
        absolute_or_none(Some(OsString::from("/absolute"))),
        Some(PathBuf::from("/absolute"))
    );
}
