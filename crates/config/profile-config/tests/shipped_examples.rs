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
//! This parses each example WHOLE through the production parser and
//! runs the real `validate()`. It replaces a grep that could only
//! compare one numeric bound, and since Stage 13 it projects nothing:
//! the type models every section the schema declares (plan §16 (13)), so
//! an example key the type does not know is a refusal, not a key dropped
//! before parsing. The projection it once needed went stale twice in
//! silence (review findings on PR #80), which is why it is gone rather
//! than kept in step.
//!
//! And it does not resolve DNS or reach a network: placeholders become
//! syntactically valid identities, because the question is whether the
//! FORM an operator is shown is one the code accepts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use interweave_profile_config::ProfileConfig;

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
    let examples = examples().expect("the shipped examples are readable");
    let mut checked = 0;
    let mut saw_connectivity = false;
    let mut saw_android = false;
    for path in &examples {
        let raw = substitute(&std::fs::read_to_string(path).expect("readable"));
        let profile = ProfileConfig::parse_yaml(&raw)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));

        // THE DOCUMENT'S OWN VALUES REACHED THE MODEL, checked against
        // values no default supplies: every example binds a listen
        // address and names its profile, and the defaults do neither.
        assert!(
            !profile.transport.listen.addresses.is_empty(),
            "{} lists a listen address",
            path.display()
        );
        assert!(
            profile.profile.is_some(),
            "{} names its profile",
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
                "{} carries static relays",
                path.display()
            );
            saw_connectivity = true;
        }
        // AND THE RUNTIME BLOCK, on the one example that states a
        // non-default deployment: a dropped block reads as `daemon-ipc`.
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
    assert_eq!(
        checked,
        examples.len(),
        "every example is a profile and is checked"
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
