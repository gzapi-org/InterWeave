// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `transport-daemon`: the desktop composition root for one profile
//! (plan §16 (1), `lifecycle.md` §Daemon lifecycle). Argv, the profile,
//! the lock, the key, the composed runtime, the IPC server, signals and
//! shutdown -- and nothing else: every behaviour is a crate's.
//!
//! Exit codes (plan §16 (11)): 0 a clean shutdown, 1 a refused start or
//! a failed run (the reason printed), 2 a usage error.

#![cfg(unix)]
#![forbid(unsafe_code)]

mod cli;
mod daemon;

use std::process::ExitCode;

fn main() -> ExitCode {
    let command = match cli::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("transport-daemon: {message}\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let args = match command {
        cli::Command::Help => {
            println!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        cli::Command::Run(args) => args,
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("transport-daemon: cannot start the async runtime: {e}");
            return ExitCode::from(1);
        }
    };
    match runtime.block_on(daemon::run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(refused) => {
            eprintln!("transport-daemon: {refused}");
            ExitCode::from(1)
        }
    }
}
