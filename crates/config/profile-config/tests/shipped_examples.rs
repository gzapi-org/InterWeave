// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Every shipped example profile must satisfy the validator that reads
//! it.
//!
//! `architecture/config/examples/` is what an operator is handed, and
//! until this existed nothing compared those files to the code. A
//! shipped profile set `max_entries: 4096` against an enforced ceiling
//! of 1024 — schema-correct, and refused by the validator the same
//! commit had tightened — because the examples are prose to every other
//! check.
//!
//! This parses each example, projects the sections `ProfileConfig`
//! models, and runs the real `validate()`. It replaces a grep that could
//! only compare one numeric bound.
//!
//! Two things it deliberately does NOT do. It does not judge the
//! node-level sections (`runtime`, `identity`, `ipc`, `profile`) that no
//! Rust type models yet — a profile document is wider than this crate,
//! and asserting on shapes nothing parses would be inventing a contract.
//! And it does not resolve DNS or reach a network: placeholders become
//! syntactically valid identities, because the question is whether the
//! FORM an operator is shown is one the code accepts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use interweave_profile_config::ProfileConfig;

/// The sections `ProfileConfig` models. A key outside this set belongs
/// to the wider profile document and is not this crate's to judge.
const MODELLED: [&str; 5] = [
    "schema_version",
    "trust",
    "endpoints",
    "discovery",
    "channels",
];

/// Stand-ins for the `<PLACEHOLDER>` peer ids the examples carry.
///
/// Distinct per placeholder, because a profile may forbid duplicates and
/// collapsing them all to one identity would test a document no operator
/// would write.
fn substitute(raw: &str) -> String {
    const IDS: [&str; 6] = [
        "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN",
        "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5",
        "12D3KooWQYV9dGMFoRzNStwpXztXaBUjtPqi6aU76ZgUriHhKust",
        "12D3KooWBhMkjWFbqjmS3PgAXfQ7SSgTNvJFtGCVJDLnDBpJ9SFy",
        "12D3KooWRBhwfeP2Y4TCx1SM6s9rUoHhR5STiGwxBhgFRcw3UERE",
        "12D3KooWSCXaVAtdgqmH3vJdgYnFmvcCxSGVLqLwG7RRFKfMQeYW",
    ];
    let mut out = raw.to_owned();
    let mut assigned: BTreeMap<String, &str> = BTreeMap::new();
    while let Some(start) = out.find('<') {
        let Some(len) = out[start..].find('>') else {
            break;
        };
        let token = out[start..=start + len].to_owned();
        let next = assigned.len();
        let id = *assigned
            .entry(token.clone())
            .or_insert(IDS[next % IDS.len()]);
        out = out.replace(&token, id);
    }
    out
}

/// The shipped examples, or an error naming the directory.
///
/// Fallible rather than panicking: clippy's `panic` lint exempts a
/// `#[test]` body but not a free helper, and an `allow` here would
/// silence the lint for the whole file rather than at the one place a
/// failure is expected.
fn examples() -> Result<Vec<PathBuf>, String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../architecture/config/examples");
    let entries =
        std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .collect();
    found.sort();
    if found.is_empty() {
        return Err(format!("no example profiles found in {}", dir.display()));
    }
    Ok(found)
}

#[test]
fn every_shipped_example_satisfies_the_validator() {
    let mut checked = 0;
    for path in examples().expect("the shipped examples are readable") {
        let raw = substitute(&std::fs::read_to_string(&path).expect("readable"));
        let whole: serde_norway::Value =
            serde_norway::from_str(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()));

        // Project the modelled sections; a document missing them all is
        // not a profile this crate speaks for.
        let mapping = whole
            .as_mapping()
            .unwrap_or_else(|| panic!("{}: the document is not a mapping", path.display()));
        let mut projected = serde_norway::Mapping::new();
        for key in MODELLED {
            if let Some(value) = mapping.get(serde_norway::Value::from(key)) {
                projected.insert(serde_norway::Value::from(key), value.clone());
            }
        }
        if !projected.contains_key(serde_norway::Value::from("endpoints")) {
            continue; // not a node profile this crate models
        }

        let profile: ProfileConfig =
            serde_norway::from_value(serde_norway::Value::Mapping(projected))
                .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
        // A NOT-YET-BUILT PROVIDER IS A STAGE FACT, NOT A BAD PROFILE.
        // The examples describe the target architecture, and this build
        // refuses an enabled `mdns` (multicast backend deferred over the
        // hickory-proto advisories) or `kademlia` (Stage 10). Refusing
        // those is the PROVIDER-CONTRACT rule working, so the test would
        // be asserting the wrong thing if it demanded silence — but
        // every OTHER error means the file an operator is handed is
        // malformed against the code that reads it.
        let errors: Vec<_> = profile
            .validate()
            .into_iter()
            .filter(|e| {
                !matches!(
                    e,
                    interweave_profile_config::ConfigError::DiscoveryProviderNotImplemented { .. }
                )
            })
            .collect();
        assert!(
            errors.is_empty(),
            "{} is shipped to operators and the validator refuses it: {errors:?}",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 8,
        "expected most examples to be node profiles, checked {checked}"
    );
}
