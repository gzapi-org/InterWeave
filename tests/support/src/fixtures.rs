// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Reading the frozen conformance vectors under `fixtures/`.
//!
//! A vector file is a JSON object declaring the algorithm it freezes and a
//! `vectors` array of cases. The layout is described in `fixtures/README.md`
//! and enforced by `tools/checks/verify_fixture_vectors.py`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::repo_root;

/// Absolute path of `fixtures/`.
#[must_use]
pub fn dir() -> PathBuf {
    repo_root().join("fixtures")
}

/// Parse one fixture file, named relative to `fixtures/`.
///
/// # Panics
///
/// If the file is missing or is not valid JSON. Both mean the test cannot ask
/// its question.
#[must_use]
pub fn load(relative: &str) -> Value {
    let path = dir().join(relative);
    parse(&path)
}

fn parse(path: &Path) -> Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("fixture {} is not valid JSON: {e}", path.display()))
}

/// One frozen vector file: where it came from, and what it holds.
#[derive(Debug, Clone)]
pub struct VectorFile {
    /// Path relative to the repository root, as the checkers report it.
    pub relative_path: String,
    /// The parsed document.
    pub document: Value,
}

impl VectorFile {
    /// The `algorithm.id` this file declares, if it declares one.
    #[must_use]
    pub fn algorithm_id(&self) -> Option<&str> {
        self.document.get("algorithm")?.get("id")?.as_str()
    }

    /// The cases in `vectors`, or an empty slice.
    #[must_use]
    pub fn vectors(&self) -> &[Value] {
        self.document
            .get("vectors")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    /// The named case, by its `name` field.
    #[must_use]
    pub fn vector(&self, name: &str) -> Option<&Value> {
        self.vectors()
            .iter()
            .find(|v| v.get("name").and_then(Value::as_str) == Some(name))
    }
}

/// Every vector file under `fixtures/`, sorted by path.
///
/// A JSON file with no `vectors` array is not one — `fixtures/` also holds
/// ordinary data — so it is skipped rather than reported.
///
/// # Panics
///
/// If `fixtures/` cannot be walked, or a file in it is not valid JSON.
#[must_use]
pub fn vector_files() -> Vec<VectorFile> {
    let root = repo_root();
    let mut found = Vec::new();

    for path in json_files(&dir()) {
        let document = parse(&path);
        if document.get("vectors").is_none() {
            continue;
        }
        let relative_path = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        found.push(VectorFile {
            relative_path,
            document,
        });
    }

    found.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    found
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read fixture directory {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("cannot read entry in {}: {e}", dir.display()));
        let path = entry.path();
        if path.is_dir() {
            out.extend(json_files(&path));
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
    out
}
