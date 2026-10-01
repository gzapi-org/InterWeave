// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The offline identity commands (plan §16 (10), IDENTITY-RECOVERY.md):
//! never over IPC, never a socket, never the network (ADR-0033).

use std::io::{IsTerminal as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use interweave_profile_config::{ProfileConfig, ProfileLock, ProfilePaths, XdgRoots};
use interweave_profile_identity::{IdentityError, ProfileIdentity, RecoveryRecord};
use zeroize::Zeroizing;

use crate::Failure;
use crate::cli::Identity;
use crate::phrase;

/// Run an identity command; what to print on success.
///
/// # Errors
/// [`Failure::Refused`], naming why -- and never a word of the phrase.
pub(crate) fn run(command: Identity) -> Result<String, Failure> {
    match command {
        Identity::Verify { expected } => verify(&phrase::read(expected)?),
        Identity::Backup { profile, to_file } => backup(&profile, to_file.as_deref()),
        Identity::Restore { .. } => Err(Failure::Refused("restore is not built yet".to_owned())),
    }
}

/// The profile's paths, offline (no runtime directory needed), and its
/// key file as its configuration names it.
fn profile(name: &str) -> Result<(ProfilePaths, PathBuf), Failure> {
    let refused = |what: &str, e: &dyn std::fmt::Display| Failure::Refused(format!("{what}: {e}"));
    let roots = XdgRoots::from_env().map_err(|e| refused("the XDG directories", &e))?;
    let paths = ProfilePaths::resolve_offline(name, &roots)
        .map_err(|e| refused("the profile's paths", &e))?;
    let config = ProfileConfig::load(&paths).map_err(|e| refused("the profile", &e))?;
    let key_file = config.identity.key_file_in(&paths);
    Ok((paths, key_file))
}

/// The profile lock, taken at once or not at all: held, it is a running
/// daemon or another identity command, and the key is not touched under
/// it (plan §16 (6), (10)).
fn lock(paths: &ProfilePaths) -> Result<ProfileLock, Failure> {
    ProfileLock::acquire(paths, Duration::ZERO).map_err(|e| {
        Failure::Refused(format!(
            "profile {:?}'s lock is held -- stop its daemon first ({e})",
            paths.profile()
        ))
    })
}

/// Emit the profile's recovery record (IDENTITY-RECOVERY.md §Export):
/// under the lock; to stdout only when it is a terminal, otherwise only
/// to a NEW file created owner-only. Never to a pipe, where it would end
/// up in whatever reads it.
fn backup(name: &str, to_file: Option<&Path>) -> Result<String, Failure> {
    let refused = |what: &str, e: &dyn std::fmt::Display| Failure::Refused(format!("{what}: {e}"));
    if to_file.is_none() && !std::io::stdout().is_terminal() {
        return Err(Failure::Refused(
            "stdout is not a terminal: the record is a secret, and goes to a terminal or to \
             --to-file <new path>"
                .to_owned(),
        ));
    }
    let (paths, key_file) = profile(name)?;
    let _lock = lock(&paths)?;
    let identity = ProfileIdentity::load(&key_file).map_err(|e| refused("the identity key", &e))?;
    let record = RecoveryRecord::of(&identity).map_err(|e| refused("the recovery record", &e))?;
    let text = Zeroizing::new(
        serde_json::to_string_pretty(&record).map_err(|e| refused("the recovery record", &e))?
            + "\n",
    );
    let peer = record.expected_peer_id.clone().unwrap_or_default();
    if let Some(path) = to_file {
        // create_new: never over an existing file; 0600 at creation,
        // so the record is never readable by anyone else, umask or not.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| refused(&format!("creating {}", path.display()), &e))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| refused(&format!("writing {}", path.display()), &e))?;
        Ok(format!(
            "recovery record for {peer} written to {}\n",
            path.display()
        ))
    } else {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush())
            .map_err(|e| refused("writing the record", &e))?;
        Ok(String::new())
    }
}

/// The drill: no lock, no key read, no write, no profile, no network
/// (IDENTITY-RECOVERY.md §Verify-only) -- only the comparison.
fn verify(recovery: &phrase::Recovery) -> Result<String, Failure> {
    ProfileIdentity::verify_phrase(&recovery.phrase, &recovery.expected).map_err(mismatch)?;
    Ok(format!(
        "verified: the phrase restores {}\n",
        recovery.expected.as_str()
    ))
}

/// A refusal from the identity crate. A mismatch names both `PeerId`s,
/// which are public; nothing it names is the phrase.
fn mismatch(e: IdentityError) -> Failure {
    match e {
        IdentityError::PeerIdMismatch { got, expected } => Failure::Refused(format!(
            "the phrase restores {got}, not {expected}: a different identity"
        )),
        other => Failure::Refused(other.to_string()),
    }
}
