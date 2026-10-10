// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The availability overlay (ADR-0041 A 2026-10-10): the person's
//! Stay-reachable choice in the state directory, the one effective mode
//! read over a real provisioned embedded profile, `config.yaml` never
//! written, off removing the entry, and a file that says anything but
//! the choice -- or that is not private -- refused rather than read as
//! off.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(target_os = "linux")]

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use interweave_profile_config::availability_overlay::{
    AVAILABILITY_OVERLAY_FILE, AvailabilityError, MAX_AVAILABILITY_OVERLAY_BYTES, StayReachable,
    effective_availability_mode, path_for, read_within, write_within,
};
use interweave_profile_config::provision::{embedded_document, provision_embedded};
use interweave_profile_config::runtime::AvailabilityMode;
use interweave_profile_config::{ProfilePaths, TrustBoundary, persist};

/// A provisioned embedded profile under an owner-only app data directory.
fn profile() -> (tempfile::TempDir, ProfilePaths) {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .expect("tempdir");
    let app = root.path().join("app");
    std::fs::create_dir(&app).expect("mkdir");
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let paths =
        ProfilePaths::resolve_embedded("work", TrustBoundary::new(&app).expect("a boundary"))
            .expect("paths");
    provision_embedded(&paths).expect("provisioned");
    (root, paths)
}

/// Put `text` where the overlay lives, mode `mode`, as something other
/// than the overlay's own writer would.
fn plant(paths: &ProfilePaths, text: &[u8], mode: u32) {
    let path = path_for(paths);
    persist::write_private_atomic_within(&path, text, paths.boundary()).expect("planted");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

#[test]
fn the_choice_overrides_the_authored_mode_and_off_removes_it() {
    let (_root, paths) = profile();
    let path = path_for(&paths);
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some(AVAILABILITY_OVERLAY_FILE)
    );
    assert!(
        path.starts_with(paths.state_dir()),
        "in the state directory"
    );
    let authored = std::fs::read(paths.config_file()).expect("config.yaml");

    // Absent: the authored default, foreground-only in the provisioned
    // profile.
    assert_eq!(read_within(&path, paths.boundary()).expect("reads"), None);
    assert_eq!(
        effective_availability_mode(&paths).expect("effective"),
        AvailabilityMode::ForegroundOnly
    );

    // On: written owner-only, and it wins over the authored field.
    write_within(&path, Some(StayReachable), paths.boundary()).expect("written");
    assert_eq!(
        std::fs::metadata(&path).expect("present").mode() & 0o777,
        0o600
    );
    assert_eq!(
        read_within(&path, paths.boundary()).expect("reads"),
        Some(StayReachable)
    );
    assert_eq!(
        effective_availability_mode(&paths).expect("effective"),
        AvailabilityMode::StayReachable
    );

    // Off: the entry is REMOVED, never rewritten as foreground-only.
    write_within(&path, None, paths.boundary()).expect("removed");
    assert!(!path.exists(), "off removes the file");
    assert_eq!(
        effective_availability_mode(&paths).expect("effective"),
        AvailabilityMode::ForegroundOnly
    );
    // A second off finds nothing to remove and is no error.
    write_within(&path, None, paths.boundary()).expect("nothing to remove");

    assert_eq!(
        std::fs::read(paths.config_file()).expect("config.yaml"),
        authored,
        "config.yaml is never written"
    );
}

#[test]
fn without_a_choice_the_authored_mode_is_the_effective_one() {
    let (_root, paths) = profile();
    // An operator who authored stay-reachable gets it with no overlay at
    // all -- the overlay overrides, it does not replace the field.
    let document = embedded_document("work").replacen(
        "  android:\n    endpoint: human\n",
        "  android:\n    endpoint: human\n    availability_mode: stay-reachable\n",
        1,
    );
    assert!(document.contains("availability_mode: stay-reachable"));
    persist::write_private_atomic_within(
        &paths.config_file(),
        document.as_bytes(),
        paths.boundary(),
    )
    .expect("authored");
    assert_eq!(
        effective_availability_mode(&paths).expect("effective"),
        AvailabilityMode::StayReachable
    );
}

#[test]
fn a_file_naming_anything_but_the_choice_is_refused() {
    let (_root, paths) = profile();
    let path = path_for(&paths);
    // THE CONTROL: the one shape, planted by hand, reads.
    plant(&paths, br#"{"availability_mode":"stay-reachable"}"#, 0o600);
    assert_eq!(
        read_within(&path, paths.boundary()).expect("the shape reads"),
        Some(StayReachable)
    );
    for text in [
        &br#"{"availability_mode":"foreground-only"}"#[..],
        br#"{"availability_mode":"stay-reachable","pinned":true}"#,
        br"{}",
        br"not json",
        // A derived struct also deserialises from a sequence.
        br#"["stay-reachable"]"#,
    ] {
        plant(&paths, text, 0o600);
        let read = read_within(&path, paths.boundary());
        assert!(
            matches!(read, Err(AvailabilityError::Parse(_))),
            "{}: {read:?}",
            String::from_utf8_lossy(text)
        );
        assert!(
            effective_availability_mode(&paths).is_err(),
            "never read as off: {}",
            String::from_utf8_lossy(text)
        );
    }
}

#[test]
fn a_file_that_is_not_private_or_too_large_is_refused() {
    let (_root, paths) = profile();
    let path = path_for(&paths);
    plant(&paths, br#"{"availability_mode":"stay-reachable"}"#, 0o644);
    let read = read_within(&path, paths.boundary());
    assert!(
        matches!(read, Err(AvailabilityError::NotPrivate { .. })),
        "{read:?}"
    );

    let padded = format!(
        "{{\"availability_mode\":\"stay-reachable\"}}{}",
        " ".repeat(usize::try_from(MAX_AVAILABILITY_OVERLAY_BYTES).expect("small"))
    );
    plant(&paths, padded.as_bytes(), 0o600);
    let read = read_within(&path, paths.boundary());
    assert!(matches!(read, Err(AvailabilityError::TooLarge)), "{read:?}");
}
