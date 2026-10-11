// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The endpoint overlay on disk (ADR-0028 A 2026-10-11): each
//! normalisation at load with the file it rewrites, byte for byte; the
//! default/disabled predicate; the set moves; and every reason a present
//! overlay stops the start -- each beside the control that the same
//! file, made right, loads.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use interweave_profile_config::endpoint_overlay::{
    ENDPOINT_OVERLAY_FILE, EndpointOverlay, EndpointOverlayError, MAX_ENDPOINT_OVERLAY_BYTES,
    Normalised,
};
use interweave_profile_config::{EndpointsConfig, MAX_ENDPOINTS, TrustBoundary};
use interweave_transport_api::EndpointId;

fn id(s: &str) -> EndpointId {
    EndpointId::parse(s).expect("a valid endpoint id")
}

/// `a` and `b` enabled, `c` disabled; the default `default`.
fn config(default: Option<&str>) -> EndpointsConfig {
    serde_json::from_value(serde_json::json!({
        "entries": [
            {"id": "a", "enabled": true, "advertise": false},
            {"id": "b", "enabled": true, "advertise": false},
            {"id": "c", "enabled": false, "advertise": false},
        ],
        "default_direct_endpoint": default,
    }))
    .expect("an endpoints block")
}

/// A private state directory and the overlay's path in it.
fn state() -> (tempfile::TempDir, PathBuf) {
    let dir = private_tempdir().expect("a temporary directory");
    let path = dir.path().join(ENDPOINT_OVERLAY_FILE);
    (dir, path)
}

/// Write `text` at `path` as the daemon would: owner-only.
fn put(path: &Path, text: &str) {
    std::fs::write(path, text).expect("written");
    chmod(path, 0o600);
}

fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

fn load(
    path: &Path,
    config: &EndpointsConfig,
) -> Result<(EndpointOverlay, Vec<Normalised>), EndpointOverlayError> {
    EndpointOverlay::load_within(path, config, &TrustBoundary::root())
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read")
}

/// One normalisation case: what it is, the configured default, the file
/// loaded, what normalising reports, the file left, `a`/`b`/`c` enabled
/// and the effective default.
type Case = (
    &'static str,
    Option<&'static str>,
    &'static str,
    Vec<Normalised>,
    &'static str,
    [bool; 3],
    Option<&'static str>,
);

/// Each normalisation of (4) and the two default/disabled cases: the
/// file loaded, what normalising reports, the file it leaves -- byte for
/// byte -- and the effective state the runtime would start with.
#[test]
fn each_normalisation_rewrites_the_file_exactly() {
    let cases: [Case; 9] = [
        (
            "an enabled entry equal to config.yaml's",
            Some("a"),
            r#"{"enabled":{"a":true,"c":true}}"#,
            vec![Normalised::EnabledEqualsConfig(id("a"))],
            "{\n  \"enabled\": {\n    \"c\": true\n  }\n}",
            [true, true, true],
            Some("a"),
        ),
        (
            "an enabled entry naming an endpoint config.yaml lacks",
            Some("a"),
            r#"{"enabled":{"gone":false,"b":false}}"#,
            vec![Normalised::UnknownEndpointDropped(id("gone"))],
            "{\n  \"enabled\": {\n    \"b\": false\n  }\n}",
            [true, false, false],
            Some("a"),
        ),
        (
            "a default equal to the configured default",
            Some("a"),
            r#"{"enabled":{},"default":"a"}"#,
            vec![Normalised::DefaultEqualsConfig(Some(id("a")))],
            "{\n  \"enabled\": {}\n}",
            [true, true, false],
            Some("a"),
        ),
        (
            "a null default where config.yaml names none",
            None,
            r#"{"enabled":{},"default":null}"#,
            vec![Normalised::DefaultEqualsConfig(None)],
            "{\n  \"enabled\": {}\n}",
            [true, true, false],
            None,
        ),
        (
            "a default naming an endpoint config.yaml lacks",
            Some("a"),
            r#"{"enabled":{},"default":"gone"}"#,
            vec![Normalised::UnknownDefaultDropped(id("gone"))],
            "{\n  \"enabled\": {}\n}",
            [true, true, false],
            Some("a"),
        ),
        (
            "the configured default disabled by the overlay",
            Some("a"),
            r#"{"enabled":{"a":false}}"#,
            vec![Normalised::ConfiguredDefaultCleared(id("a"))],
            "{\n  \"enabled\": {\n    \"a\": false\n  },\n  \"default\": null\n}",
            [false, true, false],
            None,
        ),
        (
            "a default naming an endpoint config.yaml disables",
            Some("a"),
            r#"{"enabled":{},"default":"c"}"#,
            vec![Normalised::DisabledDefaultDropped(id("c"))],
            "{\n  \"enabled\": {}\n}",
            [true, true, false],
            Some("a"),
        ),
        (
            "a default naming an endpoint the same overlay disables",
            Some("a"),
            r#"{"enabled":{"b":false},"default":"b"}"#,
            vec![Normalised::DisabledDefaultDropped(id("b"))],
            "{\n  \"enabled\": {\n    \"b\": false\n  }\n}",
            [true, false, false],
            Some("a"),
        ),
        (
            "that, with the configured default disabled too",
            Some("a"),
            r#"{"enabled":{"a":false,"b":false},"default":"b"}"#,
            vec![
                Normalised::DisabledDefaultDropped(id("b")),
                Normalised::ConfiguredDefaultCleared(id("a")),
            ],
            "{\n  \"enabled\": {\n    \"a\": false,\n    \"b\": false\n  },\n  \"default\": null\n}",
            [false, false, false],
            None,
        ),
    ];
    for (what, default, text, changes, rewritten, enabled, effective_default) in cases {
        let (_dir, path) = state();
        put(&path, text);
        let config = config(default);
        let (overlay, got) = load(&path, &config).expect(what);
        assert_eq!(got, changes, "{what}");
        assert_eq!(read(&path), rewritten, "{what}: the rewrite");
        let effective = overlay.effective(&config);
        let on: Vec<bool> = ["a", "b", "c"]
            .iter()
            .map(|e| effective.enabled[&id(e)])
            .collect();
        assert_eq!(on, enabled, "{what}");
        assert_eq!(effective.default, effective_default.map(id), "{what}");
        // The rewrite is itself normalised: loading it changes nothing.
        let (again, none) = load(&path, &config).expect(what);
        assert_eq!(again, overlay, "{what}");
        assert!(none.is_empty(), "{what}: {none:?}");
    }
}

/// A normalised overlay is not rewritten: the file loaded is the file
/// left, byte for byte, in whatever layout it was written.
#[test]
fn a_normalised_overlay_is_not_rewritten() {
    let (_dir, path) = state();
    let text = r#"{"enabled":{"a":false,"c":true},"default":"c"}"#;
    put(&path, text);
    let (overlay, changes) = load(&path, &config(Some("a"))).expect("loads");
    assert!(changes.is_empty(), "{changes:?}");
    assert_eq!(read(&path), text);
    let effective = overlay.effective(&config(Some("a")));
    assert_eq!(effective.default, Some(id("c")));
}

/// An absent overlay is the empty one and is not created.
#[test]
fn an_absent_overlay_is_empty_and_is_not_created() {
    let (_dir, path) = state();
    let (overlay, changes) = load(&path, &config(Some("a"))).expect("loads");
    assert_eq!(overlay, EndpointOverlay::default());
    assert!(changes.is_empty());
    assert!(!path.exists());
    assert_eq!(overlay.apply(&config(Some("a"))), config(Some("a")));
}

/// The set moves: disabling the default carries both deltas in one
/// overlay; enabling it again restores nothing; a set back to the
/// configuration leaves the overlay empty; a set that changes nothing is
/// `None`; an unknown or disabled endpoint is refused as the runtime
/// refuses it.
#[test]
fn the_set_moves_record_only_what_differs() {
    let config = config(Some("a"));
    let empty = EndpointOverlay::default();

    let off = empty
        .set_enabled(&config, &id("a"), false)
        .expect("known")
        .expect("a change");
    let effective = off.effective(&config);
    assert!(!effective.enabled[&id("a")]);
    assert_eq!(effective.default, None, "disabling the default clears it");

    let on = off
        .set_enabled(&config, &id("a"), true)
        .expect("known")
        .expect("a change");
    assert_eq!(
        on.effective(&config).default,
        None,
        "enabling restores nothing"
    );
    let back = on
        .set_default(&config, Some(&id("a")))
        .expect("enabled")
        .expect("a change");
    assert_eq!(
        back, empty,
        "back to the configuration is the empty overlay"
    );

    assert!(
        empty
            .set_enabled(&config, &id("b"), true)
            .expect("known")
            .is_none()
    );
    assert!(
        empty
            .set_default(&config, Some(&id("a")))
            .expect("enabled")
            .is_none()
    );

    assert!(matches!(
        empty.set_enabled(&config, &id("gone"), false),
        Err(EndpointOverlayError::Unknown)
    ));
    assert!(matches!(
        empty.set_default(&config, Some(&id("gone"))),
        Err(EndpointOverlayError::Unknown)
    ));
    assert!(matches!(
        empty.set_default(&config, Some(&id("c"))),
        Err(EndpointOverlayError::Disabled)
    ));
    // The control: enabled by the overlay, `c` may be the default.
    let c_on = empty
        .set_enabled(&config, &id("c"), true)
        .expect("known")
        .expect("a change");
    assert!(c_on.set_default(&config, Some(&id("c"))).is_ok());

    let none = empty
        .set_default(&config, None)
        .expect("none")
        .expect("a change");
    assert_eq!(none.effective(&config).default, None);
}

/// A written overlay is owner-only and loads back as itself, unchanged.
#[test]
fn a_written_overlay_is_owner_only_and_round_trips() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_dir, path) = state();
    let config = config(Some("a"));
    let overlay = EndpointOverlay::default()
        .set_enabled(&config, &id("a"), false)
        .expect("known")
        .expect("a change");
    overlay
        .write_within(&path, &TrustBoundary::root())
        .expect("written");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let (back, changes) = load(&path, &config).expect("loads");
    assert_eq!(back, overlay);
    assert!(changes.is_empty(), "{changes:?}");
}

/// Each reason a present overlay stops the start, beside the control
/// that a well-formed private file of the same contents loads.
#[test]
fn a_present_overlay_that_cannot_be_trusted_stops_the_load() {
    let config = config(Some("a"));
    let good = r#"{"enabled":{"b":false}}"#;

    let (_dir, path) = state();
    put(&path, good);
    assert!(load(&path, &config).is_ok(), "the control");

    for (what, text) in [
        ("not json", "enabled: {}"),
        ("an unknown member", r#"{"enabled":{},"extra":1}"#),
        ("enabled missing", r#"{"default":"a"}"#),
        ("enabled not an object", r#"{"enabled":["a"]}"#),
        ("not an endpoint id", r#"{"enabled":{"Not An Id":false}}"#),
        ("a state not a bool", r#"{"enabled":{"b":"off"}}"#),
        ("a default not an id", r#"{"enabled":{},"default":7}"#),
        ("an array, not the object", r"[{}]"),
        // Last-key-wins would drop the first map silently.
        (
            "a member named twice",
            r#"{"enabled":{},"enabled":{"b":false}}"#,
        ),
    ] {
        let (_dir, path) = state();
        put(&path, text);
        let loaded = load(&path, &config);
        assert!(
            matches!(loaded, Err(EndpointOverlayError::Parse(_))),
            "{what}: {loaded:?}"
        );
        assert_eq!(
            read(&path),
            text,
            "{what}: a refused file is left as it was"
        );
    }

    for mode in [0o644, 0o620, 0o602, 0o640] {
        let (_dir, path) = state();
        put(&path, good);
        chmod(&path, mode);
        assert!(
            matches!(
                load(&path, &config),
                Err(EndpointOverlayError::NotPrivate { .. })
            ),
            "mode {mode:o}"
        );
    }

    {
        let (dir, path) = state();
        let real = dir.path().join("real.json");
        put(&real, good);
        std::os::unix::fs::symlink(&real, &path).expect("a link");
        assert!(matches!(
            load(&path, &config),
            Err(EndpointOverlayError::NotPrivate { .. })
        ));
    }

    {
        let (_dir, path) = state();
        let bound = usize::try_from(MAX_ENDPOINT_OVERLAY_BYTES).expect("fits");
        // At the bound it loads (the control); one byte past, refused.
        put(&path, &format!("{good}{}", " ".repeat(bound - good.len())));
        assert!(load(&path, &config).is_ok(), "at the bound");
        put(
            &path,
            &format!("{good}{}", " ".repeat(bound - good.len() + 1)),
        );
        assert!(matches!(
            load(&path, &config),
            Err(EndpointOverlayError::TooLarge)
        ));
    }
}

/// A normalisation rewrite that fails stops the load, and the file is
/// left as it was: the stale entry could undo the operator's next
/// `config.yaml` edit. The same file in a writable directory is the
/// control.
#[test]
fn a_failed_normalisation_rewrite_stops_the_load() {
    let config = config(Some("a"));
    let text = r#"{"enabled":{"a":true}}"#;
    for writable in [true, false] {
        let (dir, path) = state();
        put(&path, text);
        if !writable {
            chmod(dir.path(), 0o500);
        }
        let loaded = load(&path, &config);
        chmod(dir.path(), 0o700);
        if writable {
            assert!(loaded.is_ok(), "the control: {loaded:?}");
        } else {
            assert!(
                matches!(loaded, Err(EndpointOverlayError::Write(_))),
                "{loaded:?}"
            );
            assert_eq!(read(&path), text);
        }
    }
}

/// The largest overlay a normalised file can hold -- every configured
/// endpoint at the id bound, each flipped, the default `null` -- fits
/// well inside the size bound and loads as itself, so the bound refuses
/// only what no set could have written.
#[test]
fn the_largest_overlay_fits_its_bound() {
    let ids: Vec<String> = (0..MAX_ENDPOINTS).map(|i| format!("e{i:0>63}")).collect();
    assert!(ids.iter().all(|s| s.len() == EndpointId::MAX_BYTES));
    let entries: Vec<_> = ids
        .iter()
        .map(|s| serde_json::json!({"id": s, "enabled": true, "advertise": false}))
        .collect();
    let config: EndpointsConfig = serde_json::from_value(serde_json::json!({
        "entries": entries,
        "default_direct_endpoint": ids[0],
    }))
    .expect("an endpoints block");
    let mut overlay = EndpointOverlay::default();
    for s in &ids {
        overlay = overlay
            .set_enabled(&config, &id(s), false)
            .expect("known")
            .expect("a change");
    }
    let (_dir, path) = state();
    overlay
        .write_within(&path, &TrustBoundary::root())
        .expect("written");
    let size = std::fs::metadata(&path).expect("meta").len();
    assert!(size < 7 * 1024, "{size} bytes");
    assert!(size < MAX_ENDPOINT_OVERLAY_BYTES);
    let (back, changes) = load(&path, &config).expect("loads");
    assert_eq!(back, overlay);
    assert!(changes.is_empty(), "{changes:?}");
}

fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt as _;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
}
