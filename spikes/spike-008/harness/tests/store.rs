// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The host run of what the device repeats: seed, transitions, census,
//! and a reopen standing in for the restart. The device's S1 and E1 rows
//! compare against exactly these sets.

#![allow(clippy::expect_used)]

use spike008::store;

const AFTER_SEED: &str = "{\"pending\":[\"P1\",\"P2\",\"P3\"],\"unread\":[\"U1\",\"U2\",\"U3\"],\"kept\":[\"K1\",\"K2\",\"K3\"],\"eligible\":[\"K1\",\"K2\",\"K3\",\"U1\",\"U2\",\"U3\"]}";
const AFTER_TRANSITIONS: &str = "{\"pending\":[\"P2\",\"P3\"],\"unread\":[\"U2\",\"U3\"],\"kept\":[\"K2\",\"K3\"],\"eligible\":[\"K2\",\"K3\",\"U2\",\"U3\"]}";

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("spike008-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    // The store refuses a directory others can read (message content is
    // owner-only); the device harness makes its store directory 0700 too.
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .expect("owner-only");
    dir.join("human.sqlite")
}

#[test]
fn seed_transitions_and_a_reopen_give_the_expected_sets() {
    let db = scratch("cycle");
    assert_eq!(store::seed(&db).expect("seeds"), AFTER_SEED);
    assert_eq!(
        store::transitions(&db).expect("transitions"),
        AFTER_TRANSITIONS
    );
    // A reopen (the restart): what was released stays released.
    assert_eq!(store::census(&db).expect("census"), AFTER_TRANSITIONS);
    let _ = std::fs::remove_dir_all(db.parent().expect("dir"));
}

#[test]
fn a_store_already_holding_rows_is_not_seeded_again() {
    let db = scratch("twice");
    store::seed(&db).expect("seeds");
    assert!(
        store::seed(&db)
            .expect_err("refuses")
            .starts_with("not empty")
    );
    let _ = std::fs::remove_dir_all(db.parent().expect("dir"));
}

#[test]
fn a_message_read_and_not_kept_is_not_unread_again_even_after_a_reopen() {
    let db = scratch("again");
    store::seed(&db).expect("seeds");
    store::transitions(&db).expect("transitions");
    assert_eq!(
        store::redeliver(&db).expect("redelivers"),
        "{\"u1_again\":\"AlreadyRead\",\"control_c1\":\"committed\"}"
    );
    let _ = std::fs::remove_dir_all(db.parent().expect("dir"));
}

#[test]
fn a_full_store_degrades_and_refuses_unread_while_what_it_holds_stays_readable() {
    let db = scratch("full");
    let out = store::fill(&db).expect("fills");
    assert!(out.contains("\"health\":\"Degraded\""), "{out}");
    assert!(out.contains("\"then\":\"Degraded\""), "{out}");
    assert!(
        !out.contains("\"committed\":0,"),
        "a quota too small to hold one message proves nothing: {out}"
    );
    let _ = std::fs::remove_dir_all(db.parent().expect("dir"));
}
