// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The health probe against a medium that refuses the WRITE, not the
//! page: a fault that lets a transaction be built in the page cache and
//! rolled back, and fails only when a commit reaches the file.
//!
//! The fault is `RLIMIT_FSIZE`, which std does not expose and this
//! workspace will not reach through `unsafe`; so the test re-runs
//! itself as a child under `sh -c 'ulimit -f ...'` and reads the
//! child's verdict. The limit is per process: the parent, the other
//! tests and the rest of the workspace are untouched.
//!
//! What it pins: with the fault active after the store degraded,
//! `recheck_health` stays degraded -- a probe that wrote and rolled
//! back reported recovery here, since nothing it did reached the
//! medium (review finding, 2026-09-18) -- and with the fault lifted a
//! real committed probe clears it and a maximal message commits.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

use interweave_human_core::retention::StorageHealth;
use interweave_human_store::{AppMessageId, HumanStore, InboundOrigin, NewInbound, StoreOptions};
use interweave_transport_api::TransportIdentity;

const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
const CHILD_ENV: &str = "INTERWEAVE_DURABLE_WRITE_FAULT_DB";
const CHILD_OK: &str = "DURABLE-WRITE-FAULT-CHILD-OK";
/// `ulimit -f` counts 512-byte blocks: 40 KiB. Enough for the WAL index
/// (32 KiB, which SQLite must create) and too little for the WAL frames
/// of one maximal message or of the probe (about 60 KiB each), so the
/// commit fails at the write and nowhere earlier.
const LIMIT_BLOCKS: u32 = 80;

fn inbound(id: u32, payload: Vec<u8>) -> NewInbound {
    NewInbound {
        app_message_id: AppMessageId::parse(format!("{id:032x}")).expect("test id is canonical"),
        origin: InboundOrigin {
            peer: TransportIdentity::parse(PEER).expect("canonical"),
            endpoint: None,
            channel: None,
        },
        media_type: None,
        payload,
        received_at: 2_000,
    }
}

fn maximal() -> Vec<u8> {
    vec![0_u8; interweave_transport_api::MAX_PAYLOAD_BYTES]
}

/// The half that runs under the limit: the medium refuses the commit,
/// the store degrades, and the probe must not say otherwise.
fn under_the_fault(path: &Path) {
    let mut store = HumanStore::open(path, StoreOptions::default()).expect("opens");
    assert_eq!(store.health(), StorageHealth::Healthy);
    let err = store
        .commit_unread_inbound(&inbound(1, maximal()))
        .expect_err("a medium that cannot grow refuses the commit");
    assert_eq!(
        store.health(),
        StorageHealth::Degraded,
        "an I/O failure at the write degrades the store: {err}"
    );
    // THE CLAIM: the fault is still active, so the probe must not
    // report recovery. A probe that writes and rolls back passed here.
    let verdict = store.recheck_health();
    assert!(
        verdict.is_err(),
        "the probe must fail while durable writes fail: {verdict:?}"
    );
    assert_eq!(
        store.health(),
        StorageHealth::Degraded,
        "and the store stays degraded"
    );
    println!("{CHILD_OK}");
}

#[test]
fn recheck_health_stays_degraded_while_durable_writes_fail() {
    if let Some(path) = std::env::var_os(CHILD_ENV) {
        under_the_fault(Path::new(&path));
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    // THE CONTROL: with no fault, a maximal message commits, and the
    // store is closed so the child starts with an empty WAL.
    {
        let mut store = HumanStore::open(&path, StoreOptions::default()).expect("opens");
        store
            .commit_unread_inbound(&inbound(0, maximal()))
            .expect("a healthy medium commits");
    }

    let me = std::env::current_exe().expect("this test binary");
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "trap '' XFSZ; ulimit -f {LIMIT_BLOCKS} && exec \"$0\" \"$@\""
        ))
        .arg(&me)
        .args([
            "--exact",
            "recheck_health_stays_degraded_while_durable_writes_fail",
            "--nocapture",
        ])
        .env(CHILD_ENV, &path)
        .output()
        .expect("the child runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains(CHILD_OK),
        "the child under the fault failed:\n--- stdout\n{stdout}\n--- stderr\n{stderr}"
    );

    // THE RECOVERY: no fault here, and the medium holds what the child
    // was refused: a real committed probe says healthy and a maximal
    // message commits.
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("reopens");
    assert_eq!(
        store.recheck_health().expect("the medium is fine"),
        StorageHealth::Healthy
    );
    assert_eq!(
        store.unread_inbound().expect("read").len(),
        1,
        "the child's refused commit left nothing behind"
    );
    store
        .commit_unread_inbound(&inbound(2, maximal()))
        .expect("a recovered store commits");
}
