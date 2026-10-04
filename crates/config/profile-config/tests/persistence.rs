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
    create_private_dir, create_private_exclusive, is_owner_only, write_atomic,
    write_private_atomic,
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
        p.data_socket().expect("resolved with a runtime dir"),
    ];
    for (i, a) in all.iter().enumerate() {
        for b in all.iter().skip(i + 1) {
            assert_ne!(a, b, "two roles resolved to the same path");
        }
    }
}

/// The human client's directory (R4, architect-cto's ruling of relay seq
/// 11163): under the state root, and none of the daemon's paths -- not
/// one of them, and not a directory above one.
#[test]
fn the_human_dir_is_under_state_and_holds_no_daemon_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path());
    let human = p.human_dir();
    assert_eq!(human.parent(), Some(p.state_dir()));
    let daemon: Vec<PathBuf> = vec![
        p.config_file(),
        p.identity_file(),
        p.peer_cache_file(),
        p.state_dir().join(interweave_profile_config::LOCK_FILE),
        p.data_socket().expect("socket"),
        p.admin_socket().expect("socket"),
        p.config_dir().to_path_buf(),
        p.identity_dir().to_path_buf(),
        p.cache_dir().to_path_buf(),
    ];
    for path in &daemon {
        assert_ne!(&human, path, "{}", path.display());
        assert!(
            !path.starts_with(&human),
            "{} is inside the human dir",
            path.display()
        );
    }
    assert!(p.roles_are_distinct());
}

/// Every other role pointed into the human directory, and one that
/// contains it, is reported -- each row on its own, so dropping any role
/// or either direction from the check fails a row. The control: the same
/// base layout is accepted.
#[test]
fn a_role_in_or_above_the_human_dir_is_reported() {
    type Move = fn(&mut XdgRoots, &std::path::Path, &std::path::Path);
    let dir = tempfile::tempdir().expect("tempdir");
    let base = paths(dir.path());
    assert!(base.roles_are_distinct(), "the control layout is accepted");
    let human = base.human_dir();
    let state_home = roots(dir.path()).state_home;
    let rows: [(&str, Move); 5] = [
        ("data root inside", |r, h, _| {
            r.data_home = h.to_path_buf();
        }),
        ("config root inside", |r, h, _| {
            r.config_home = h.to_path_buf();
        }),
        ("cache root inside", |r, h, _| {
            r.cache_home = h.to_path_buf();
        }),
        ("runtime root inside", |r, h, _| {
            r.runtime_dir = Some(h.to_path_buf());
        }),
        ("runtime root above", |r, _, s| {
            r.runtime_dir = Some(s.to_path_buf());
        }),
    ];
    for (name, apply) in rows {
        let mut moved = roots(dir.path());
        apply(&mut moved, &human, &state_home);
        let p = ProfilePaths::resolve("default", &moved).expect("resolve");
        assert!(!p.roles_are_distinct(), "{name}");
    }
}

/// A root holding `..` is refused outright, whichever role it is: compared
/// lexically it could name the human directory while looking like
/// somewhere else. The state row is the one that matters most -- it moves
/// the human directory itself while config sits where it really is.
#[test]
fn a_role_path_with_a_parent_component_is_refused() {
    type Move = fn(&mut XdgRoots, &std::path::Path);
    let dir = tempfile::tempdir().expect("tempdir");
    let real_human = paths(dir.path()).human_dir();
    let rows: [(&str, Move); 5] = [
        ("config", |r, b| {
            r.config_home = b.join("x").join("..").join("config");
        }),
        ("data", |r, b| {
            r.data_home = b.join("x").join("..").join("data");
        }),
        ("cache", |r, b| {
            r.cache_home = b.join("x").join("..").join("cache");
        }),
        ("runtime", |r, b| {
            r.runtime_dir = Some(b.join("x").join("..").join("run"));
        }),
        ("state", |r, b| {
            r.state_home = b.join("x").join("..").join("state");
        }),
    ];
    for (name, apply) in rows {
        let mut moved = roots(dir.path());
        apply(&mut moved, dir.path());
        if name == "state" {
            // On disk the human directory is where it always was, and
            // config is put right inside it.
            moved.config_home = real_human.clone();
        }
        let p = ProfilePaths::resolve("default", &moved).expect("resolve");
        assert!(!p.roles_are_distinct(), "{name}");
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
    assert_ne!(
        p.data_socket().expect("socket"),
        p.admin_socket().expect("socket")
    );
}

#[test]
fn no_profiles_data_socket_is_anothers_admin_socket() {
    // `work-admin` is a legal profile name; under `<profile>-admin.sock`
    // its data socket was `work`'s admin socket. The `.` of
    // `<profile>.admin.sock` is outside the name alphabet (LOCAL-IPC.md,
    // A 2026-10-01).
    let dir = tempfile::tempdir().expect("tempdir");
    let roots = XdgRoots {
        config_home: dir.path().join("c"),
        data_home: dir.path().join("d"),
        state_home: dir.path().join("s"),
        cache_home: dir.path().join("k"),
        runtime_dir: Some(dir.path().join("run")),
    };
    let names = ["work", "work-admin", "work_admin", "admin", "w"];
    for a in names {
        for b in names {
            let data = ProfilePaths::resolve(a, &roots)
                .expect("paths")
                .data_socket()
                .expect("socket");
            let admin = ProfilePaths::resolve(b, &roots)
                .expect("paths")
                .admin_socket()
                .expect("socket");
            assert_ne!(data, admin, "{a}'s data socket is {b}'s admin socket");
        }
    }
    // And a name with the separator in it is not a profile name at all.
    assert!(ProfilePaths::resolve("work.admin", &roots).is_err());
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

/// The offline roles resolve without a runtime directory, to the same
/// places the daemon's resolution gives them; only the sockets are
/// unavailable, and asking for one is `NoRuntimeDir`, never a default.
#[test]
fn the_offline_roles_resolve_without_a_runtime_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let online = paths(dir.path());
    let mut r = roots(dir.path());
    r.runtime_dir = None;
    let offline = ProfilePaths::resolve_offline("default", &r).expect("resolves offline");
    assert_eq!(offline.config_file(), online.config_file());
    assert_eq!(offline.identity_file(), online.identity_file());
    assert_eq!(offline.state_dir(), online.state_dir());
    assert_eq!(offline.peer_cache_file(), online.peer_cache_file());
    assert!(offline.roles_are_distinct());
    assert!(matches!(
        offline.data_socket(),
        Err(PersistError::NoRuntimeDir)
    ));
    assert!(matches!(
        offline.admin_socket(),
        Err(PersistError::NoRuntimeDir)
    ));
    assert!(matches!(
        ProfilePaths::resolve_offline("../etc", &r),
        Err(PersistError::InvalidProfileName { .. })
    ));
}

#[test]
#[cfg(unix)]
fn a_private_write_is_owner_only_from_the_moment_it_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Under a subdirectory the write creates itself: `tempdir()` is
    // 0755, and a private write now refuses a parent that open.
    let path = dir.path().join("state").join("identity.key");
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
    let path = dir.path().join("state").join("identity.key");
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

#[test]
#[cfg(unix)]
fn key_material_refuses_a_parent_directory_that_is_not_private() {
    // The module said "the directory matters as much as the file" and
    // then called `create_dir_all`, which makes a 0755 directory when
    // the path is new and does nothing at all when it is not. A 0600
    // key inside a directory another account can write is a key that
    // account can REPLACE; inside one they can traverse it still leaks
    // its existence, size, and mtime. The mode on the file was half the
    // guarantee, stated as the whole one.
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let open = dir.path().join("open");
    std::fs::create_dir(&open).expect("mkdir");
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    for mode in [0o755, 0o701, 0o770] {
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(mode)).expect("chmod");
        let path = open.join("identity.key");
        assert!(
            matches!(
                write_private_atomic(&path, b"not a real key"),
                Err(PersistError::DirectoryNotPrivate { .. })
            ),
            "mode {mode:o} must be refused"
        );
        assert!(
            matches!(
                create_private_exclusive(&path, b"not a real key"),
                Err(PersistError::DirectoryNotPrivate { .. })
            ),
            "mode {mode:o} must be refused on the exclusive path too"
        );
        assert!(!path.exists(), "nothing may be written into it");
    }

    // Narrowed, both paths work -- so the refusals were about the
    // directory and not about the write.
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(OWNER_ONLY_DIR))
        .expect("chmod");
    write_private_atomic(&open.join("a.key"), b"not a real key").expect("0700 is acceptable");
    create_private_exclusive(&open.join("b.key"), b"not a real key").expect("0700 is acceptable");

    // Non-secret configuration keeps the old behaviour: it is not key
    // material and an XDG config directory is legitimately 0755.
    let plain = dir.path().join("plain");
    std::fs::create_dir(&plain).expect("mkdir");
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    write_atomic(&plain.join("config.toml"), b"schema_version = 2").expect("config is not secret");
}

#[test]
#[cfg(unix)]
fn a_symlinked_parent_is_refused_however_private_its_target() {
    // Following the link asks the question about somewhere other than
    // where the write lands: the target can be a perfectly good 0700
    // directory while the LINK sits somewhere anyone can repoint.
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).expect("mkdir");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(OWNER_ONLY_DIR))
        .expect("chmod");

    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    assert!(matches!(
        write_private_atomic(&link.join("identity.key"), b"not a real key"),
        Err(PersistError::DirectoryNotPrivate { .. })
    ));
}

#[test]
#[cfg(unix)]
fn a_squatted_temporary_name_is_an_error_and_not_a_write_elsewhere() {
    // The temporary was opened with `create(true).truncate(true)`, which
    // FOLLOWS a symlink -- so a name someone could predict was a name
    // they could point at a file they wanted truncated and filled with
    // whatever this process was about to write. `create_new` refuses
    // both an existing file and a link.
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir(&state).expect("mkdir");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(OWNER_ONLY_DIR))
        .expect("chmod");

    // The name is unguessable now, which is the other half of the fix,
    // so the test occupies the name the writer will use by driving the
    // same helper the writer drives. Squatting the target itself is the
    // observable version of the same refusal.
    let victim = dir.path().join("victim");
    std::fs::write(&victim, b"precious").expect("write");
    let path = state.join("identity.key");
    std::os::unix::fs::symlink(&victim, &path).expect("symlink the TARGET");

    // The exclusive path must not follow it, and must not truncate the
    // victim on the way to finding out.
    assert!(matches!(
        create_private_exclusive(&path, b"not a real key"),
        Err(PersistError::AlreadyExists)
    ));
    assert_eq!(
        std::fs::read(&victim).expect("read"),
        b"precious",
        "the link target must be untouched"
    );
}
