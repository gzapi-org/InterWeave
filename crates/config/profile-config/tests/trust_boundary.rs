// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The trust boundary carried by `ProfilePaths` (ADR-0028 A 2026-10-08;
//! plan §20 step 1): an embedded runtime's paths lie under one root
//! directly beneath the boundary, and the lock, the configuration load
//! and the trust overlay -- which take the paths or a path from them --
//! judge their directories only up to it. Each case runs beside a
//! control: the same layout reached through desktop paths, which judge
//! to `/` and are refused at the ancestor above the boundary.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use interweave_profile_config::trust_overlay::TrustOverlay;
use interweave_profile_config::{
    HumanClientLock, PersistError, ProfileConfig, ProfileLock, ProfilePaths, TrustBoundary,
    XdgRoots, create_private_dir_within, is_owner_only,
};

/// `T/open/app`: the app's data directory, `0700` and ours, under
/// `open`, which every walk to `/` refuses (other-writable, not sticky)
/// -- the stand-in for Android's `system`-owned `/data/data`.
fn app_under_a_refused_ancestor() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .expect("tempdir");
    let open = root.path().join("open");
    let app = open.join("app");
    for (dir, mode) in [(&open, 0o777), (&app, 0o700)] {
        std::fs::create_dir(dir).expect("mkdir");
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
    (root, open, app)
}

/// Desktop paths with every root under `base`: the control, judged to `/`.
fn desktop_paths(base: &Path) -> ProfilePaths {
    let roots = XdgRoots {
        config_home: base.join("config"),
        data_home: base.join("data"),
        state_home: base.join("state"),
        cache_home: base.join("cache"),
        runtime_dir: None,
    };
    ProfilePaths::resolve_offline("work", &roots).expect("paths")
}

fn embedded_paths(app: &Path) -> ProfilePaths {
    let boundary = TrustBoundary::new(app).expect("a boundary");
    ProfilePaths::resolve_embedded("work", boundary).expect("paths")
}

/// The embedded layout: one root `<app>/interweave`, the four offline
/// roles under it with the profile's tree, no socket, every role apart.
#[test]
fn the_embedded_layout_is_one_root_under_the_boundary() {
    let (_root, _open, app) = app_under_a_refused_ancestor();
    let p = embedded_paths(&app);
    let root = app.join("interweave");
    assert_eq!(p.boundary().path(), app);
    assert_eq!(p.config_dir(), root.join("config/profiles/work"));
    assert_eq!(p.identity_dir(), root.join("data/profiles/work"));
    assert_eq!(p.state_dir(), root.join("state/profiles/work"));
    assert_eq!(p.cache_dir(), root.join("cache/profiles/work"));
    assert!(p.roles_are_distinct());
    assert!(matches!(p.data_socket(), Err(PersistError::NoRuntimeDir)));
    assert!(matches!(
        ProfilePaths::resolve_embedded("../work", TrustBoundary::new(&app).expect("boundary")),
        Err(PersistError::InvalidProfileName { .. })
    ));
}

/// Both locks are taken under the boundary, and create the runtime's
/// root owner-only on the way; desktop paths under the same refused
/// ancestor are refused and create nothing.
#[test]
fn the_locks_judge_up_to_the_boundary() {
    let (_root, open, app) = app_under_a_refused_ancestor();
    let control = desktop_paths(&app.join("desktop"));
    assert!(matches!(
        ProfileLock::acquire(&control, Duration::ZERO),
        Err(PersistError::DirectoryNotPrivate { path, .. }) if path == open
    ));
    assert!(!app.join("desktop").exists(), "the control created nothing");

    let p = embedded_paths(&app);
    let _lock = ProfileLock::acquire(&p, Duration::ZERO).expect("the profile lock");
    assert!(ProfileLock::is_held(&p).expect("probe"));
    let _human = HumanClientLock::acquire(&p, Duration::ZERO).expect("the human lock");
    assert!(is_owner_only(&app.join("interweave")).expect("mode"));
}

/// `ProfileConfig::load` judges the configuration directory up to the
/// boundary; the same document under desktop paths is refused.
#[test]
fn the_configuration_loads_under_the_boundary() {
    let (_root, open, app) = app_under_a_refused_ancestor();
    let document = "schema_version: 2
profile:
  name: work
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  entries:
    - id: human
      enabled: true
      advertise: false
";
    let p = embedded_paths(&app);
    let control = desktop_paths(&app.join("desktop"));
    for paths in [&p, &control] {
        create_private_dir_within(paths.config_dir(), p.boundary()).expect("config dir");
        std::fs::write(paths.config_file(), document).expect("write");
        std::fs::set_permissions(paths.config_file(), std::fs::Permissions::from_mode(0o644))
            .expect("chmod");
    }
    assert!(matches!(
        ProfileConfig::load(&control),
        Err(interweave_profile_config::LoadError::ConfigDirUnguarded(
            PersistError::DirectoryNotPrivate { path, .. }
        )) if path == open
    ));
    ProfileConfig::load(&p).expect("loads under the boundary");
}

/// The trust overlay is written and read under the boundary; without it
/// the same file is refused both ways.
#[test]
fn the_trust_overlay_persists_under_the_boundary() {
    let (_root, _open, app) = app_under_a_refused_ancestor();
    let p = embedded_paths(&app);
    let path = TrustOverlay::path_for(&p);
    let none = BTreeSet::new();
    assert!(TrustOverlay::default().write(&path).is_err(), "the control");
    TrustOverlay::default()
        .write_within(&path, p.boundary())
        .expect("written under the boundary");
    assert!(path.exists());
    assert!(TrustOverlay::load(&path, &none).is_err(), "the control");
    TrustOverlay::load_within(&path, &none, p.boundary()).expect("read under the boundary");
}
