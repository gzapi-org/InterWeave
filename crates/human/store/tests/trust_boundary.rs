// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The store opens under the trust boundary its profile's paths carry
//! (ADR-0028 A 2026-10-08, plan §20 step 1): an embedded profile's store
//! is judged up to the app's data directory and no further, so a
//! directory above it that the walk to `/` refuses does not refuse it.

#![cfg(target_os = "linux")]
#![allow(clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use interweave_human_store::{HumanStore, STORE_FILE, StoreOptions};
use interweave_profile_config::{ProfilePaths, TrustBoundary};

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

/// A data directory standing where Android's does: under a directory
/// other accounts can write (no sticky bit), which the walk to `/`
/// refuses as an ancestor.
fn app_data_under_a_wide_parent() -> (tempfile::TempDir, std::path::PathBuf) {
    let outer = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .expect("tempdir under /tmp");
    let wide = outer.path().join("wide");
    std::fs::create_dir(&wide).expect("mkdir");
    std::fs::set_permissions(&wide, std::fs::Permissions::from_mode(0o777)).expect("chmod");
    let app = wide.join("app");
    std::fs::create_dir(&app).expect("mkdir");
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    (outer, app)
}

#[test]
fn an_embedded_profiles_store_opens_under_its_boundary_and_not_without_it() {
    let (_outer, app) = app_data_under_a_wide_parent();
    let boundary = TrustBoundary::new(&app).expect("the app's data directory");
    let paths = ProfilePaths::resolve_embedded("default", boundary).expect("embedded paths");
    let file = paths.human_dir().join(STORE_FILE);

    // The control: the same file judged to `/` is refused, for the wide
    // ancestor above the boundary, and nothing is made.
    assert!(
        HumanStore::open(&file, StoreOptions::default()).is_err(),
        "judged to /, the wide parent refuses it"
    );
    assert!(
        !app.join("interweave").exists(),
        "a refusal makes nothing under the app's directory"
    );

    let store = HumanStore::open_profile(&paths, StoreOptions::default());
    assert!(store.is_ok(), "under its boundary: {:?}", store.err());
    assert!(file.is_file(), "at the profile's human directory");
    assert_eq!(mode(&paths.human_dir()), 0o700, "made owner-only");
    assert_eq!(mode(&file), 0o600);
}

#[test]
fn a_desktop_profiles_store_is_the_human_directorys_store_file() {
    let outer = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .expect("tempdir under /tmp");
    let roots = interweave_profile_config::XdgRoots {
        config_home: outer.path().join("config"),
        data_home: outer.path().join("data"),
        state_home: outer.path().join("state"),
        cache_home: outer.path().join("cache"),
        runtime_dir: None,
    };
    let paths = ProfilePaths::resolve_offline("default", &roots).expect("desktop paths");
    assert_eq!(paths.boundary(), &TrustBoundary::root());
    drop(HumanStore::open_profile(&paths, StoreOptions::default()).expect("opens"));
    assert!(paths.human_dir().join(STORE_FILE).is_file());
}
