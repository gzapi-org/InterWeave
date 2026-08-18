// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Commit retention rows and then die without unwinding.
//!
//! `std::process::abort()` runs no destructors, no `Drop`, no atexit
//! handler, and gives SQLite no chance to close the database. That is
//! the difference between proving a store survives a CRASH and proving
//! it survives a clean shutdown — and only the first is what
//! `RETENTION.md` §7 claims.
//!
//! Invoked by the conformance suite as
//! `crash_writer <db-path> <scenario>`; not part of any product.

use std::path::PathBuf;
use std::process::{ExitCode, abort};

use interweave_human_retention_tests::{pending_outbound, unread_inbound};
use interweave_human_store::{HumanStore, StoreOptions};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(path), Some(scenario)) = (args.next(), args.next()) else {
        eprintln!("usage: crash_writer <db-path> <scenario>");
        return ExitCode::FAILURE;
    };

    let Ok(mut store) = HumanStore::open(&PathBuf::from(&path), StoreOptions::default()) else {
        eprintln!("crash_writer: cannot open {path}");
        return ExitCode::FAILURE;
    };

    match scenario.as_str() {
        // One durable row of each kind, then die.
        "durable" => {
            if store.commit_pending_outbound(&pending_outbound()).is_err()
                || store.commit_unread_inbound(&unread_inbound()).is_err()
            {
                eprintln!("crash_writer: commit failed");
                return ExitCode::FAILURE;
            }
        }
        // Commit both, then reach the states whose content must NOT
        // survive: outbound transport-terminal, inbound read-and-unkept.
        "ephemeral" => {
            let (Ok(out), Ok(inb)) = (
                store.commit_pending_outbound(&pending_outbound()),
                store.commit_unread_inbound(&unread_inbound()),
            ) else {
                eprintln!("crash_writer: commit failed");
                return ExitCode::FAILURE;
            };
            if store
                .transport_terminal(out, interweave_human_store::TerminalCause::Accepted)
                .is_err()
            {
                eprintln!("crash_writer: terminal failed");
                return ExitCode::FAILURE;
            }
            // Read it and deliberately DO NOT keep it. The ephemeral copy
            // exists only in this process, which is about to stop existing.
            if store.mark_read(inb, 1_700_000_002_000).is_err() {
                eprintln!("crash_writer: read failed");
                return ExitCode::FAILURE;
            }
        }
        other => {
            eprintln!("crash_writer: unknown scenario {other}");
            return ExitCode::FAILURE;
        }
    }

    // No unwinding, no Drop, no SQLite close. Anything the store has not
    // already fsynced is lost — which is exactly the question.
    abort();
}
