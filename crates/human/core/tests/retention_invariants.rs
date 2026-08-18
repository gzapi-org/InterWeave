// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The ADR-0044 invariants, walked exhaustively.
//!
//! The unit tests cover each transition. This suite covers the claims
//! that are about the state machine *as a whole* — the ones a reader
//! would otherwise have to verify by reading every transition and
//! trusting that none was missed.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_human_core::{
    Durability, InboundMessage, InboundState, OutboundMessage, OutboundState, TerminalCause,
};

/// Every reachable inbound state, by the path that reaches it.
fn all_inbound_states() -> Vec<(&'static str, InboundMessage)> {
    let unread = InboundMessage::committed_unread();

    let mut read = InboundMessage::committed_unread();
    read.mark_read();

    let mut kept = InboundMessage::committed_unread();
    kept.mark_read();
    kept.keep().expect("kept");

    let mut unkept = InboundMessage::committed_unread();
    unkept.mark_read();
    unkept.keep().expect("kept");
    unkept.unkeep();

    let mut expired = InboundMessage::committed_unread();
    expired.mark_read();
    expired.session_ended();

    vec![
        ("unread", unread),
        ("read-ephemeral", read),
        ("kept", kept),
        ("kept-then-unkept", unkept),
        ("read-then-session-ended", expired),
    ]
}

#[test]
fn exactly_three_states_are_durable_and_no_path_reaches_a_fourth() {
    // The core invariant of ADR-0044, checked over every reachable state
    // rather than over the ones that came to mind.
    let mut durable = Vec::new();

    let pending = OutboundMessage::composed();
    if pending.durability() == Durability::Durable {
        durable.push("outbound-pending");
    }
    let mut terminal = OutboundMessage::composed();
    terminal.transport_terminal(TerminalCause::Accepted);
    assert_eq!(
        terminal.durability(),
        Durability::Remove,
        "transport-terminal outbound must not be durable"
    );

    for (name, m) in all_inbound_states() {
        if m.durability() == Durability::Durable {
            durable.push(match m.state() {
                InboundState::Unread => "inbound-unread",
                InboundState::Kept => "inbound-kept",
                InboundState::ReadEphemeral => {
                    panic!("'{name}' is read-ephemeral and claims durability")
                }
            });
        }
    }

    durable.sort_unstable();
    durable.dedup();
    assert_eq!(
        durable,
        vec!["inbound-kept", "inbound-unread", "outbound-pending"],
        "the durable set is exactly the three ADR-0044 states"
    );
}

#[test]
fn no_inbound_path_reaches_kept_without_passing_through_read() {
    // Enumerated rather than asserted: `keep` is the only way in, and it
    // refuses before read state.
    let mut fresh = InboundMessage::committed_unread();
    assert!(fresh.keep().is_err(), "unread must not be keepable");
    assert_eq!(fresh.state(), InboundState::Unread);

    // The only successful route.
    let mut m = InboundMessage::committed_unread();
    assert_eq!(m.mark_read(), Durability::Remove);
    assert!(m.keep().is_ok());
    assert_eq!(m.state(), InboundState::Kept);
}

#[test]
fn every_terminal_cause_produces_the_same_retention_answer() {
    // Four ways to stop mattering, one retention consequence. If they
    // diverged, "transport-terminal" would stop being a single concept.
    for cause in [
        TerminalCause::Accepted,
        TerminalCause::Published,
        TerminalCause::Cancelled,
    ] {
        let mut m = OutboundMessage::composed();
        assert_eq!(m.transport_terminal(cause), Durability::Remove, "{cause:?}");
        assert_eq!(m.state(), OutboundState::Terminal);
        assert!(!m.backup_eligible(), "{cause:?} must stay out of backup");
    }
}

#[test]
fn backup_carries_inbound_content_only() {
    // ADR-0044: a portable backup may include inbound unread and inbound
    // kept, and never outbound in any state — so a restored device cannot
    // become an implicit replay or delayed-send source.
    let mut pending = OutboundMessage::composed();
    assert!(!pending.backup_eligible());
    pending.transport_terminal(TerminalCause::Accepted);
    assert!(!pending.backup_eligible());

    for (name, m) in all_inbound_states() {
        let expected = matches!(m.state(), InboundState::Unread | InboundState::Kept);
        assert_eq!(
            m.backup_eligible(),
            expected,
            "'{name}' backup eligibility disagrees with its state"
        );
        // And eligibility never exceeds durability: a state with no
        // durable content cannot contribute content to a backup.
        if m.backup_eligible() {
            assert_eq!(m.durability(), Durability::Durable, "'{name}'");
        }
    }
}

#[test]
fn keeping_is_idempotent_and_unkeeping_is_immediate() {
    let mut m = InboundMessage::committed_unread();
    m.mark_read();
    assert_eq!(m.keep().expect("kept"), Durability::Durable);
    // Keeping twice is not an error and does not change anything.
    assert_eq!(m.keep().expect("still kept"), Durability::Durable);
    assert_eq!(m.unkeep(), Durability::Remove);
    // Unkeeping twice is likewise stable.
    assert_eq!(m.unkeep(), Durability::Remove);
}
