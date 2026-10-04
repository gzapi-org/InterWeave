// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `human-desktop --profile <name>`: the desktop human client.

#![forbid(unsafe_code)]

use std::process::ExitCode;

#[cfg(unix)]
fn main() -> ExitCode {
    interweave_human_desktop::run::run(std::env::args().skip(1))
}

/// The desktop IPC binding is Unix-only until the Windows named pipe lands
/// (plan section 18, carried; architect-cto's Q12 ruling): elsewhere the
/// app says so and stops.
#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!(
        "human-desktop: this platform is not supported yet: the client reaches the transport \
         over a Unix socket, and the Windows named pipe is not built"
    );
    ExitCode::from(69)
}
