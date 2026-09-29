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
//! node-level sections (`identity`, `ipc`, `profile`) that no Rust type
//! models yet — `runtime` left that list at Stage 12, when its block and
//! the checkable half of its cross-field rules were modelled — nor the
//! sub-blocks of `transport` other than
//! `connectivity`, for the same reason one level down — a profile
//! document is wider than this crate, and asserting on shapes nothing
//! parses would be inventing a contract.
//! And it does not resolve DNS or reach a network: placeholders become
//! syntactically valid identities, because the question is whether the
//! FORM an operator is shown is one the code accepts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use interweave_profile_config::ProfileConfig;

/// The sections `ProfileConfig` models. A key outside this set belongs
/// to the wider profile document and is not this crate's to judge.
const MODELLED: [&str; 11] = [
    "schema_version",
    "trust",
    "endpoints",
    "discovery",
    "channels",
    // `transport` joined the list when `transport.connectivity` landed,
    // and the list is why that block had no coverage against the
    // documents operators are handed until it did: the projection drops
    // every unlisted key, so all ten examples' connectivity blocks were
    // stripped before parsing. A hand-maintained set goes stale the
    // first time `ProfileConfig` grows a section. Review finding on
    // PR #80.
    "transport",
    // Stage 12: the deployment binding, which composition reads.
    "runtime",
    // Stage 13: the IPC boundary, which the deployment rules read.
    "ipc",
    // Stage 13: the last three sections the schema declares.
    "profile",
    "identity",
    "observability",
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

/// `MODELLED` must name every section `ProfileConfig` actually models.
///
/// WITHOUT THIS THE LIST GOES STALE IN SILENCE, which is how the
/// `transport` block went uncovered: the projection drops any key the
/// list omits, so a missing section does not fail anything — it removes
/// coverage, and removing coverage is invisible by construction. That is
/// the one mutation of this file that passed, so this is the test for the
/// test. Review finding on PR #80.
///
/// Serialization is the oracle: every field of `ProfileConfig` is
/// serialized, so the keys of a round-tripped value ARE the sections this
/// crate models. A new section therefore fails here until it is listed.
///
/// TRUE OF THIS TYPE, NOT BY CONSTRUCTION. A future field carrying
/// `skip_serializing_if` would be modelled, absent from a minimal
/// profile's serialization, and projected away with this test still
/// green. No field carries one today and the assertion is
/// two-directional, which is as far as serialization can take it.
#[test]
fn the_modelled_list_names_every_section_the_type_models() {
    let minimal: ProfileConfig = serde_norway::from_str(
        "schema_version: 2\ntrust:\n  policy: static-allowlist\n  allowed_peers: []\nendpoints:\n  entries: []\n",
    )
    .expect("a minimal profile parses");
    let value = serde_norway::to_value(&minimal).expect("a profile serializes");
    let mapping = value.as_mapping().expect("a profile is a mapping");

    let mut modelled: Vec<&str> = MODELLED.to_vec();
    modelled.sort_unstable();
    let mut serialized: Vec<String> = mapping
        .keys()
        .map(|k| k.as_str().expect("a section key is a string").to_owned())
        .collect();
    serialized.sort();

    assert_eq!(
        serialized,
        modelled.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        "MODELLED and the sections ProfileConfig serializes must agree; \
         a section missing from MODELLED is projected away and silently untested"
    );
}

#[test]
fn every_shipped_example_satisfies_the_validator() {
    let mut checked = 0;
    let mut saw_connectivity = false;
    let mut saw_android = false;
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

        // THE BLOCK ACTUALLY SURVIVED THE PROJECTION, checked against a
        // value no default supplies: a stripped `transport` deserializes to
        // the defaults, which are valid by construction and which no
        // example contradicts, so dropping it would fail nothing else.
        // Every example binds at least one listen address, and the
        // default binds none. (Once a two-level projection kept only
        // `connectivity`; review finding on PR #80. Since Stage 13 the
        // whole block is modelled and nothing below `transport` is
        // projected.)
        assert!(
            !profile.transport.listen.addresses.is_empty(),
            "{} lists a listen address; an empty list here means the projection dropped \
             transport rather than that the document changed",
            path.display()
        );
        if path
            .file_name()
            .is_some_and(|n| n == "internet-reachability.yaml")
        {
            assert!(
                !profile
                    .transport
                    .connectivity
                    .relay
                    .client
                    .static_relays
                    .is_empty(),
                "{} carries static relays; an empty list here means the projection dropped \
                 transport.connectivity rather than that the document changed",
                path.display()
            );
            saw_connectivity = true;
        }
        // AND THE RUNTIME BLOCK SURVIVED, on the one example that states a
        // non-default deployment: a stripped block reads as `daemon-ipc`.
        if path.file_name().is_some_and(|n| n == "human-android.yaml") {
            assert_eq!(
                profile.runtime.deployment,
                interweave_profile_config::runtime::Deployment::EmbeddedAndroid,
                "{} is the embedded-android example",
                path.display()
            );
            saw_android = true;
        }
        // NOTHING IS EXCUSED. Stage 12 composes every provider type, so
        // the filter that once excused an enabled `mdns` or `kademlia` as
        // a stage fact went with the refusal it excused; every error now
        // means the file an operator is handed is malformed against the
        // code that reads it. The shipped kademlia entries state
        // `enabled: true` (ADR-0034 item 2's review-clarity rule), so
        // ADR-0034 §7's gate on an implied default does not fire here.
        let errors: Vec<_> = profile.validate();
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
    assert!(
        saw_connectivity,
        "internet-reachability.yaml is the document the connectivity assertion reads; \
         if it is gone or renamed, that assertion silently stops running"
    );
    assert!(
        saw_android,
        "human-android.yaml is the document the runtime assertion reads; \
         if it is gone or renamed, that assertion silently stops running"
    );
}
