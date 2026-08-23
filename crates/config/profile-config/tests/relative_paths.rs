// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A bare relative filename resolves to the current directory.
//!
//! # Why this is its own test binary
//!
//! It changes the process working directory, which is global state.
//! Cargo runs each integration test file in its own process but runs
//! the tests inside one file on threads, so a `set_current_dir` beside
//! other tests would move the ground under them. One test, one binary,
//! no coordination needed.

#![allow(clippy::expect_used, clippy::panic)]

#[test]
#[cfg(unix)]
fn a_bare_relative_filename_writes_into_the_current_directory() {
    // `Path::parent` returns `Some("")` for a bare relative filename --
    // NOT `None` -- so the `unwrap_or_else(|| Path::new("."))` written
    // for exactly this case never fired. `symlink_metadata("")` then
    // failed with ENOENT, and `write_private_atomic("identity.key")`
    // reported a missing directory rather than checking the one it was
    // about to write into.
    //
    // Normalising fixes the wrong error, not the refusal: a private
    // write into the current directory still requires that directory to
    // be owner-only, which for key material is the right answer. The
    // difference is that it now says which directory and why.
    use std::os::unix::fs::PermissionsExt as _;

    use interweave_profile_config::{
        OWNER_ONLY_DIR, PersistError, create_private_exclusive, write_private_atomic,
    };

    assert_eq!(
        std::path::Path::new("identity.key")
            .parent()
            .map(std::path::Path::as_os_str),
        Some(std::ffi::OsStr::new("")),
        "the premise: a bare filename has an EMPTY parent, not None"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let private = dir.path().join("private");
    std::fs::create_dir(&private).expect("mkdir");
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(OWNER_ONLY_DIR))
        .expect("chmod");

    std::env::set_current_dir(&private).expect("cd");
    write_private_atomic(std::path::Path::new("identity.key"), b"not a real key")
        .expect("a bare filename in a private working directory");
    assert!(private.join("identity.key").exists());

    create_private_exclusive(std::path::Path::new("other.key"), b"not a real key")
        .expect("the exclusive path too");
    assert!(private.join("other.key").exists());

    // And the refusal, when the working directory is not private, names
    // the directory rather than reporting it missing.
    let open = dir.path().join("open");
    std::fs::create_dir(&open).expect("mkdir");
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::env::set_current_dir(&open).expect("cd");
    match write_private_atomic(std::path::Path::new("identity.key"), b"not a real key") {
        Err(PersistError::DirectoryNotPrivate { path, detail }) => {
            assert_eq!(path, std::path::Path::new("."), "it checked the CWD");
            assert!(detail.contains("0755"), "and says why: {detail}");
        }
        other => panic!("expected DirectoryNotPrivate, got {other:?}"),
    }

    // Leave the process somewhere that still exists, so the tempdir can
    // be removed on drop.
    std::env::set_current_dir(dir.path()).expect("cd");
}
