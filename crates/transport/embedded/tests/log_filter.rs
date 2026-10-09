// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The embedded host's log filter against the daemon's (plan §20, carried
//! from §18): the same `Targets` the daemon's `init_logging` builds is the
//! oracle, asked of every profile level, every record level and a target
//! of each class -- the audit target, a first-party one, a third party's.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_profile_config::sections::LogLevel;
use interweave_transport_composition::AUDIT_TARGET;
use interweave_transport_embedded::log_admits;
use tracing::Level;
use tracing_subscriber::filter::Targets;

const PROFILES: [(LogLevel, Level); 4] = [
    (LogLevel::Error, Level::ERROR),
    (LogLevel::Warn, Level::WARN),
    (LogLevel::Info, Level::INFO),
    (LogLevel::Debug, Level::DEBUG),
];

const LEVELS: [Level; 5] = [
    Level::ERROR,
    Level::WARN,
    Level::INFO,
    Level::DEBUG,
    Level::TRACE,
];

const TARGETS: [&str; 5] = [
    AUDIT_TARGET,
    "interweave::connectivity",
    "interweave_transport_composition::runtime",
    "libp2p_swarm",
    "tokio::runtime",
];

/// The daemon's filter, as `apps/transport-daemon`'s `init_logging`
/// composes it (its binary's own target aside, which an embedded host
/// does not have): the writer's ceiling and the per-target levels.
fn daemons(level: Level) -> impl Fn(&str, &Level) -> bool {
    let widest = if level == Level::DEBUG {
        Level::DEBUG
    } else {
        Level::INFO
    };
    let targets = Targets::new()
        .with_default(std::cmp::min(level, Level::WARN))
        .with_target("interweave", level)
        .with_target(AUDIT_TARGET, Level::INFO);
    move |target, at| *at <= widest && targets.would_enable(target, at)
}

#[test]
fn the_filter_admits_what_the_daemons_does() {
    for (profile, level) in PROFILES {
        let oracle = daemons(level);
        for target in TARGETS {
            for at in LEVELS {
                assert_eq!(
                    log_admits(target, at, profile),
                    oracle(target, &at),
                    "{target} at {at} under {profile:?}"
                );
            }
        }
    }
}

/// The clause the stage's gate (g) proves on a device, here by name: a
/// trust change's record at INFO passes a filter stricter than INFO, and
/// the same level from any other target does not -- the control.
#[test]
fn the_audit_target_passes_a_stricter_filter() {
    assert!(log_admits(AUDIT_TARGET, Level::INFO, LogLevel::Error));
    assert!(!log_admits(
        "interweave::connectivity",
        Level::INFO,
        LogLevel::Error
    ));
    assert!(!log_admits(AUDIT_TARGET, Level::DEBUG, LogLevel::Error));
}

/// The oracle above is a copy; this holds the copy to the daemon's
/// source, so a change to the daemon's filter fails here until the copy
/// -- and `log_admits` -- follow it.
#[test]
fn the_copy_is_the_daemons_construction() {
    let daemon = include_str!("../../../../apps/transport-daemon/src/daemon.rs");
    for line in [
        "const FIRST_PARTY: &str = \"interweave\";",
        "let widest = if level == tracing::Level::DEBUG {",
        ".with_default(std::cmp::min(level, tracing::Level::WARN))",
        ".with_target(FIRST_PARTY, level)",
        ".with_target(AUDIT_TARGET, tracing::Level::INFO),",
    ] {
        assert!(
            daemon.contains(line),
            "the daemon's filter no longer has: {line}"
        );
    }
}
