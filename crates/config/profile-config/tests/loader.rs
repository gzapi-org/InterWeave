// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The production loader (plan §16 (13), precondition P5): each of its
//! refusals beside the document it accepts, through the path the daemon
//! takes -- `ProfilePaths` to `config.yaml` to `ProfileConfig`.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;

use interweave_profile_config::{
    ConfigError, LoadError, MAX_PROFILE_BYTES, ProfileConfig, ProfilePaths, XdgRoots,
    create_private_dir,
};

fn paths(base: &Path, profile: &str) -> ProfilePaths {
    let roots = XdgRoots {
        config_home: base.join("config"),
        data_home: base.join("data"),
        state_home: base.join("state"),
        cache_home: base.join("cache"),
        runtime_dir: None,
    };
    ProfilePaths::resolve_offline(profile, &roots).expect("paths")
}

fn document(name_line: &str, extra: &str) -> String {
    format!(
        "schema_version: 2
{name_line}
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  entries:
    - id: human
      enabled: true
      advertise: false
{extra}"
    )
}

fn write(paths: &ProfilePaths, text: &str) {
    create_private_dir(paths.config_dir()).expect("config dir");
    std::fs::write(paths.config_file(), text).expect("write");
}

#[test]
fn a_valid_document_loads_as_its_own_profile() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path(), "work");
    write(&p, &document("profile:\n  name: work", ""));
    let loaded = ProfileConfig::load(&p).expect("loads");
    assert_eq!(loaded.profile.expect("named").name, "work");
}

/// The name is the document's claim about which profile it is; loaded as
/// another, or claiming none, it is refused rather than trusted.
#[test]
fn a_document_naming_another_profile_or_none_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path(), "work");
    write(&p, &document("profile:\n  name: home", ""));
    match ProfileConfig::load(&p) {
        Err(LoadError::NameMismatch { expected, found }) => {
            assert_eq!(
                (expected.as_str(), found.as_deref()),
                ("work", Some("home"))
            );
        }
        other => panic!("expected NameMismatch, got {other:?}"),
    }
    write(&p, &document("", ""));
    assert!(matches!(
        ProfileConfig::load(&p),
        Err(LoadError::NameMismatch { found: None, .. })
    ));
}

#[test]
fn a_document_that_breaks_a_rule_is_refused_with_every_rule_it_breaks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path(), "work");
    write(
        &p,
        &document(
            "profile:\n  name: work",
            "observability:\n  payload_logging: true\nipc:\n  max_clients: 2\n  max_admin_clients: 3\n",
        ),
    );
    match ProfileConfig::load(&p) {
        Err(LoadError::Invalid(errors)) => {
            assert!(errors.contains(&ConfigError::LiteralViolated {
                field: "observability.payload_logging"
            }));
            assert!(
                errors
                    .iter()
                    .any(|e| matches!(e, ConfigError::OrderViolated { .. })),
                "{errors:?}"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn an_unknown_key_is_a_parse_refusal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path(), "work");
    write(
        &p,
        &document("profile:\n  name: work", "telemetry:\n  enabled: true\n"),
    );
    match ProfileConfig::load(&p) {
        Err(LoadError::Parse(e)) => assert!(e.contains("telemetry"), "{e}"),
        other => panic!("expected Parse, got {other:?}"),
    }
}

/// A document past the ceiling is refused before it is parsed; the
/// ceiling itself is not.
#[test]
fn a_document_past_the_ceiling_is_refused_unparsed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path(), "work");
    let base = document("profile:\n  name: work", "");
    let pad = |total: u64| {
        let comment = usize::try_from(total).expect("fits") - base.len() - 1;
        format!("{base}#{}", "x".repeat(comment))
    };
    write(&p, &pad(MAX_PROFILE_BYTES));
    ProfileConfig::load(&p).expect("exactly the ceiling loads");
    write(&p, &pad(MAX_PROFILE_BYTES + 1));
    assert!(matches!(
        ProfileConfig::load(&p),
        Err(LoadError::TooLarge { limit }) if limit == MAX_PROFILE_BYTES
    ));
}

#[test]
fn a_missing_document_is_a_read_refusal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path(), "work");
    assert!(matches!(ProfileConfig::load(&p), Err(LoadError::Read(_))));
}

/// R4: the transport key is never kept in the human client's directory.
/// A key file configured there -- by absolute path, or by a relative one
/// climbing out of the configuration with `..` -- is refused at load; the
/// control, an absolute key file elsewhere, loads.
#[test]
fn a_key_file_inside_the_human_dir_or_climbing_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path(), "work");

    write(
        &p,
        &document(
            "profile:\n  name: work",
            "identity:\n  key_file: keys/work.key\n",
        ),
    );
    ProfileConfig::load(&p).expect("a relative key file under the configuration loads");

    let elsewhere = dir.path().join("keys").join("work.key");
    write(
        &p,
        &document(
            "profile:\n  name: work",
            &format!("identity:\n  key_file: {}\n", elsewhere.display()),
        ),
    );
    ProfileConfig::load(&p).expect("a key file elsewhere loads");

    let inside = p.human_dir().join("identity.key");
    write(
        &p,
        &document(
            "profile:\n  name: work",
            &format!("identity:\n  key_file: {}\n", inside.display()),
        ),
    );
    match ProfileConfig::load(&p) {
        Err(e @ LoadError::KeyFileInHumanDir { .. }) => assert!(
            e.to_string().contains(&inside.display().to_string()),
            "the message names the key file: {e}"
        ),
        other => panic!("refused as inside the human dir: {other:?}"),
    }

    write(
        &p,
        &document(
            "profile:\n  name: work",
            "identity:\n  key_file: ../../../../state/interweave/profiles/work/human/identity.key\n",
        ),
    );
    match ProfileConfig::load(&p) {
        Err(LoadError::Invalid(errors)) => {
            let climbs = errors
                .iter()
                .find(|e| matches!(e, ConfigError::KeyFileClimbs { .. }))
                .expect("refused as climbing");
            assert!(
                climbs.to_string().contains("../../../../state"),
                "the message names the path as written: {climbs}"
            );
        }
        other => panic!("refused as climbing: {other:?}"),
    }
}

/// R4 judged on disk (the external review of 2026-10-04, P2-1): a key
/// file reached through a link into the human client's directory is
/// refused, though its path's text lies outside it; so is one under a
/// human directory that is itself a link, and one through a dangling
/// link. The control beside each: the same layout with a real directory
/// where the link was loads.
#[cfg(unix)]
#[test]
fn a_key_file_reached_through_a_link_into_the_human_dir_is_refused() {
    use std::os::unix::fs::symlink;

    fn key_at(p: &ProfilePaths, key: &Path) -> Result<ProfileConfig, LoadError> {
        write(
            p,
            &document(
                "profile:\n  name: work",
                &format!("identity:\n  key_file: {}\n", key.display()),
            ),
        );
        ProfileConfig::load(p)
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let p = paths(dir.path(), "work");
    let vault = p.human_dir().join("vault");
    std::fs::create_dir_all(&vault).expect("vault");
    let external = dir.path().join("external");
    std::fs::create_dir_all(external.join("real")).expect("external");

    key_at(&p, &external.join("real").join("keys").join("work.key"))
        .expect("the control: a real directory outside the human dir loads");

    symlink(&vault, external.join("link")).expect("link");
    let through = external.join("link").join("keys").join("work.key");
    match key_at(&p, &through) {
        Err(LoadError::KeyFileInHumanDir { path }) => assert_eq!(path, through),
        other => panic!("refused as inside the human dir through the link: {other:?}"),
    }

    // The human directory itself a link to where the key is.
    let other = tempfile::tempdir().expect("tempdir");
    let q = paths(other.path(), "work");
    let elsewhere = other.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    let key = elsewhere.join("work.key");
    key_at(&q, &key).expect("the control: no human dir yet, the key elsewhere loads");
    std::fs::create_dir_all(q.human_dir().parent().expect("state dir")).expect("state dir");
    symlink(&elsewhere, q.human_dir()).expect("human link");
    assert!(
        matches!(key_at(&q, &key), Err(LoadError::KeyFileInHumanDir { .. })),
        "refused once the human dir leads to the key's directory"
    );

    // A dangling link: what it names may appear later.
    symlink(dir.path().join("nowhere"), external.join("dangling")).expect("dangling");
    match key_at(&p, &external.join("dangling").join("work.key")) {
        Err(e @ LoadError::KeyFileUnresolved { .. }) => assert!(
            e.to_string().contains("dangling"),
            "the message names the link: {e}"
        ),
        other => panic!("refused as unresolved: {other:?}"),
    }
}
