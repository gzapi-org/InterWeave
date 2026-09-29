// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Every shipped example profile composes into a running runtime.
//!
//! `profile-config`'s `tests/shipped_examples.rs` proves each example
//! VALIDATES; this proves each one STARTS -- every block it enables
//! translated and switched on, every provider it names constructed and
//! started, the task running -- and then shuts down cleanly. Each is
//! parsed whole, through the production parser, with its
//! `<PLACEHOLDER>` peers made concrete: since Stage 13 the type models
//! every section the examples carry, so nothing is projected away.
//!
//! What it does not prove: that the six naming `/dns4` hosts reach them
//! (the names are placeholders that resolve nowhere), or anything over
//! the network -- `tests/composition.rs` is where two nodes connect.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::Path;

use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportRuntime;
use interweave_transport_composition::{ComposedRuntime, CompositionOptions};

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

fn parse(path: &Path) -> ProfileConfig {
    let raw = substitute(&std::fs::read_to_string(path).expect("readable"));
    ProfileConfig::parse_yaml(&raw)
        .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_shipped_example_composes_and_starts() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../architecture/config/examples");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("the examples directory")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .collect();
    paths.sort();
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let mut composed = Vec::new();
    for path in &paths {
        let profile = parse(path);
        let name = path
            .file_name()
            .expect("a file")
            .to_string_lossy()
            .to_string();
        // FROM ITS OWN BLOCKS (plan §16 (13)), under scratch paths, with
        // one override: the examples bind 0.0.0.0:4001, which ten runtimes
        // on one host cannot share, so each listens on a free loopback
        // port instead.
        let roots = interweave_profile_config::XdgRoots {
            config_home: scratch.path().join(&name).join("config"),
            data_home: scratch.path().join(&name).join("data"),
            state_home: scratch.path().join(&name).join("state"),
            cache_home: scratch.path().join(&name).join("cache"),
            runtime_dir: None,
        };
        let paths = interweave_profile_config::ProfilePaths::resolve_offline("example", &roots)
            .expect("scratch paths");
        let options = CompositionOptions {
            listen: vec!["/ip4/127.0.0.1/tcp/0".to_owned()],
            ..CompositionOptions::from_profile(&profile, &paths)
        };
        let runtime = ComposedRuntime::start(&ProfileIdentity::generate(), &profile, options)
            .await
            .unwrap_or_else(|e| panic!("{name} does not compose: {e}"));
        let health = runtime.health().await.expect("answered");
        assert!(
            !health.components.is_empty(),
            "{name}: the runtime answers health"
        );
        runtime.shutdown().await.expect("clean shutdown");
        composed.push(name);
    }
    assert_eq!(
        composed.len(),
        paths.len(),
        "every example composes, composed {composed:?}"
    );
}
