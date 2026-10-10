// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The contract schema tree as a validator, included by each suite that
//! needs it (`#[path]`) rather than compiled into `interweave-test-support`:
//! `jsonschema` pulls in an MIT-0 crate (`borrow-or-share`), which the
//! licence policy admits only as a dev-dependency -- `deny.toml` judges a
//! test-support crate's normal dependencies as it judges any crate's --
//! so the one definition lives here and each including package carries
//! `jsonschema` and `serde_json` among its dev-dependencies.

/// The validator for the schema at `relative` under
/// `architecture/contracts/schemas`, its `urn:`
/// references resolved against the whole tree.
pub(crate) fn schema_validator(relative: &str) -> jsonschema::Validator {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("a test package sits two below the root")
        .join("architecture/contracts/schemas");
    let mut docs = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a schema directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "json")
                && path.file_name().is_some_and(|n| n != "manifest.json")
            {
                let text = std::fs::read_to_string(&path).expect("read");
                docs.push(serde_json::from_str::<serde_json::Value>(&text).expect("json"));
            }
        }
    }
    let pairs: Vec<(String, jsonschema::Resource)> = docs
        .into_iter()
        .filter_map(|doc| {
            let id = doc.get("$id")?.as_str()?.to_owned();
            Some((id, jsonschema::Resource::from_contents(doc)))
        })
        .collect();
    let registry = jsonschema::Registry::new()
        .extend(pairs)
        .expect("register")
        .prepare()
        .expect("prepare");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(relative)).expect("the schema"))
            .expect("json");
    jsonschema::options()
        .with_registry(&registry)
        .build(&schema)
        .expect("compiles")
}
