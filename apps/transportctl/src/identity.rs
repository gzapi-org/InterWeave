// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The offline identity commands (plan §16 (10), IDENTITY-RECOVERY.md):
//! never over IPC, never a socket, never the network (ADR-0033).

use interweave_profile_identity::{IdentityError, ProfileIdentity};

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
        Identity::Backup { .. } | Identity::Restore { .. } => Err(Failure::Refused(
            "backup and restore are not built yet".to_owned(),
        )),
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
