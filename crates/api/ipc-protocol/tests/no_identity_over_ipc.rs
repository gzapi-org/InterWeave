// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Identity export and import are not IPC operations (ADR-0033,
//! IDENTITY-RECOVERY.md §Export, §Restore, required test "export/import
//! unavailable through IPC"): no method of the closed catalogue, and no
//! event, names the identity, its key or its recovery. They are
//! `transportctl`'s offline commands, which never open a socket.

use interweave_ipc_protocol::{EventType, Method};

/// Words an identity operation would carry in its name. `peer` is not
/// one: methods name a peer as a destination, never its key.
const IDENTITY_WORDS: [&str; 8] = [
    "identity", "recovery", "recover", "backup", "restore", "phrase", "mnemonic", "key",
];

fn names_the_identity(name: &str) -> bool {
    name.split(['.', '_'])
        .any(|part| IDENTITY_WORDS.contains(&part))
}

#[test]
fn no_method_reaches_the_identity() {
    assert!(
        names_the_identity("admin.identity.backup"),
        "the control: such a name is caught"
    );
    for method in Method::ALL {
        assert!(
            !names_the_identity(method.as_str()),
            "{} names the identity",
            method.as_str()
        );
    }
}

#[test]
fn no_event_carries_the_identity() {
    for event in EventType::ALL {
        assert!(
            !names_the_identity(event.as_str()),
            "{} names the identity",
            event.as_str()
        );
    }
}
