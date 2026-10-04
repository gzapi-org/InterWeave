// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Start-up, in the order that keeps a refusal cheap and the store safe:
//! the command line, the platform, the profile, the single-instance lock,
//! the store -- and only then a window.
//!
//! Exit codes are sysexits(3)'s, so a launcher or a test can tell why.

use std::process::ExitCode;
use std::time::Duration;

use interweave_human_store::StoreOptions;
use interweave_human_ui_slint::{PlatformProblem, View, platform_check};
use interweave_profile_config::{HumanClientLock, PersistError, XdgRoots};

use crate::startup::{Blocked, Opened, StartError, open_store, parse_args, resolve};

/// The command line was wrong.
pub const EX_USAGE: u8 = 64;
/// The store needs recovery.
pub const EX_DATAERR: u8 = 65;
/// A platform service is missing: fontconfig, or a window.
pub const EX_UNAVAILABLE: u8 = 69;
/// The store could not be opened now.
pub const EX_IOERR: u8 = 74;
/// Another window holds this profile's store.
pub const EX_TEMPFAIL: u8 = 75;
/// The store or its directory is not private to this user.
pub const EX_NOPERM: u8 = 77;
/// The profile's directories or configuration are wrong.
pub const EX_CONFIG: u8 = 78;

/// Run the client with `args` (the command line after the program name).
pub fn run(args: impl IntoIterator<Item = String>) -> ExitCode {
    let refuse = |code: u8, what: &dyn std::fmt::Display| {
        eprintln!("human-desktop: {what}");
        ExitCode::from(code)
    };

    let launch = match parse_args(args) {
        Ok(launch) => launch,
        Err(usage) => return refuse(EX_USAGE, &usage),
    };
    if let Err(PlatformProblem::NoFontconfig) = platform_check() {
        return refuse(
            EX_UNAVAILABLE,
            &"the fontconfig library could not be loaded: without it no text can be shown. \
              Install fontconfig and start again.",
        );
    }
    let profile = match XdgRoots::from_env()
        .map_err(StartError::Directories)
        .and_then(|roots| resolve(&launch, &roots))
    {
        Ok(profile) => profile,
        Err(e) => return refuse(EX_CONFIG, &e),
    };
    // Before the store: two windows on one store would fight over the
    // endpoint lease and the rows. Held until the process ends.
    let _instance = match HumanClientLock::acquire(&profile.paths, Duration::ZERO) {
        Ok(lock) => lock,
        // What, not why: the lock's path and holder are nothing a person
        // can act on (human-client-ui.md section 12, as EndpointInUse).
        Err(PersistError::InstanceLocked { .. }) => {
            return refuse(EX_TEMPFAIL, &"this profile's window is already open");
        }
        Err(
            e @ (PersistError::DirectoryNotPrivate { .. } | PersistError::FileNotPrivate { .. }),
        ) => {
            return refuse(EX_NOPERM, &e);
        }
        Err(e @ PersistError::UnsupportedPlatform) => return refuse(EX_UNAVAILABLE, &e),
        Err(e) => return refuse(EX_IOERR, &e),
    };
    let _store = match open_store(&profile.store_path(), StoreOptions::default()) {
        Opened::Ready(store) => store,
        Opened::Blocked(blocked) => {
            let code = match blocked {
                Blocked::Recovery { .. } => EX_DATAERR,
                Blocked::NotPrivate { .. } => EX_NOPERM,
                Blocked::Unavailable { .. } => EX_IOERR,
            };
            return refuse(code, &blocked);
        }
    };
    match View::new() {
        // This build has no windowing backend: the views are reached
        // through ui-slint, which names none until plan section 18's
        // batch 4. Everything before a window has run, and nothing is
        // left held: the lock and the store close as this returns.
        Err(e) => refuse(
            EX_UNAVAILABLE,
            &format_args!("no window can be opened in this build: {e}"),
        ),
        Ok(_) => refuse(
            EX_UNAVAILABLE,
            &"this build has no event loop for its window yet",
        ),
    }
}
