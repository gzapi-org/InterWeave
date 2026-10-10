// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The first-run profile of an embedded-android host
//! (`provision::provision_embedded`): the document it writes loads and
//! validates as an embedded-android profile, owner-only, under the
//! host's configuration root -- and a second first run is refused,
//! leaving the first document as it was.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(target_os = "linux")]

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use interweave_profile_config::provision::{embedded_document, provision_embedded};
use interweave_profile_config::runtime::Deployment;
use interweave_profile_config::{PersistError, ProfileConfig, ProfilePaths, TrustBoundary};

/// An app data directory, owner-only, under an owner-only temporary root.
fn app() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .expect("tempdir");
    let app = root.path().join("app");
    std::fs::create_dir(&app).expect("mkdir");
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    (root, app)
}

#[test]
fn the_document_validates_as_an_embedded_android_profile() {
    let config: ProfileConfig =
        serde_norway::from_str(&embedded_document("work")).expect("the document parses");
    assert!(config.validate().is_empty(), "{:?}", config.validate());
    assert_eq!(config.runtime.deployment, Deployment::EmbeddedAndroid);
    assert!(
        config
            .transport
            .listen
            .addresses
            .iter()
            .all(|a| a.starts_with("/ip4/0.0.0.0/") || a.starts_with("/ip6/::/")),
        "wildcard listeners only: {:?}",
        config.transport.listen.addresses
    );
    assert!(
        config.trust.allowed_peers.is_empty(),
        "no trust is defaulted"
    );
}

#[test]
fn a_first_run_writes_the_profile_and_a_second_is_refused() {
    let (_root, app) = app();
    let paths =
        ProfilePaths::resolve_embedded("work", TrustBoundary::new(&app).expect("a boundary"))
            .expect("paths");
    provision_embedded(&paths).expect("the first run writes it");
    let meta = std::fs::metadata(paths.config_file()).expect("written");
    assert_eq!(meta.mode() & 0o777, 0o600, "owner-only");
    assert!(
        paths.config_file().starts_with(&app),
        "under the app's own directory"
    );
    let loaded = ProfileConfig::load(&paths).expect("it loads as written");
    assert_eq!(loaded.runtime.deployment, Deployment::EmbeddedAndroid);

    // A second first run is refused, and the first document stands.
    std::fs::write(paths.config_file(), "edited by the person\n").expect("an edit");
    assert!(matches!(
        provision_embedded(&paths),
        Err(PersistError::AlreadyExists)
    ));
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).expect("read"),
        "edited by the person\n",
        "nothing replaced"
    );
}

/// A legal profile name YAML would read as another scalar -- a null, a
/// boolean, a number -- still round-trips: the first-run document writes
/// it plain, and the name's `String` field takes the scalar's text, so
/// the profile loads under its own name. Pinned because a change of YAML
/// reader or of the field's type would break it silently.
#[test]
fn a_name_yaml_reads_as_another_scalar_round_trips() {
    for name in ["null", "true", "no", "on", "123", "0x1F", "1e3", "-1"] {
        let (_root, app) = app();
        let paths =
            ProfilePaths::resolve_embedded(name, TrustBoundary::new(&app).expect("a boundary"))
                .expect("a legal name");
        provision_embedded(&paths).expect("written");
        let config = ProfileConfig::load(&paths)
            .unwrap_or_else(|e| panic!("{name}: the provisioned profile loads: {e}"));
        assert_eq!(
            config.profile.map(|profile| profile.name).as_deref(),
            Some(name),
            "{name}"
        );
    }
}
