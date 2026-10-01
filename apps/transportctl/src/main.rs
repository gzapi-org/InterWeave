// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `transportctl`: the admin client of a running daemon, over its admin
//! socket, and the offline identity commands, which never touch a socket
//! (plan §16 (10), ADR-0033).
//!
//! Exit codes (plan §16 (11)): 0 done, 1 refused (the reason or the
//! daemon's error code printed), 2 a usage error, 3 the daemon
//! unreachable.

#![cfg(unix)]
#![forbid(unsafe_code)]

mod admin;
mod cli;

use std::io::Write as _;
use std::process::ExitCode;

/// Why a command did not complete.
#[derive(Debug)]
pub(crate) enum Failure {
    /// Refused: by this tool, or by the daemon with its error code.
    Refused(String),
    /// No daemon answered on the admin socket.
    Unreachable(String),
}

fn main() -> ExitCode {
    let command = match cli::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("transportctl: {message}\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let outcome = match command {
        cli::Command::Help => {
            println!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        cli::Command::Admin {
            profile,
            action,
            json,
        } => match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(admin::run(&profile, action, json)),
            Err(e) => Err(Failure::Refused(format!(
                "cannot start the async runtime: {e}"
            ))),
        },
        cli::Command::Identity(_) => Err(Failure::Refused(
            "the identity commands are not built yet".to_owned(),
        )),
    };
    match outcome {
        Ok(out) => {
            let mut stdout = std::io::stdout().lock();
            if stdout
                .write_all(out.as_bytes())
                .and_then(|()| stdout.flush())
                .is_err()
            {
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Err(Failure::Refused(why)) => {
            eprintln!("transportctl: {why}");
            ExitCode::from(1)
        }
        Err(Failure::Unreachable(why)) => {
            eprintln!("transportctl: {why}");
            ExitCode::from(3)
        }
    }
}
