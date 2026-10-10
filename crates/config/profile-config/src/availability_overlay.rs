// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The availability overlay: the person's Android Stay-reachable choice,
//! kept in the state directory beside the trust overlay (ADR-0041 A
//! 2026-10-10, ADR-0028 A 2026-10-10).
//!
//! `config.yaml`'s `runtime.android.availability_mode` is the AUTHORED
//! default and is never written. The person's choice is
//! `<state>/availability-overlay.json`, which holds exactly one shape,
//! `{"availability_mode": "stay-reachable"}`: turning the choice off
//! REMOVES the file, so `foreground-only` is never written into it --
//! absence is the default, as in trust -- and a file saying anything
//! else is refused, not read as off. [`StayReachable`] is the only
//! value a write can carry, so a caller cannot express the forbidden
//! one.
//!
//! The one effective mode -- the overlay when present, else the
//! authored field -- is [`effective_availability_mode`], readable before
//! a host starts: what the Service's start decision,
//! `background_restart_requires_user_authentication` and the diagnostic
//! all read.
//!
//! Like the trust overlay, a present file that cannot be trusted --
//! someone else's, readable by anyone else, a link, not the shape --
//! is an error, never skipped: skipping it would read a person's
//! explicit opt-in as off, or a stranger's file as their opt-in.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::private_read::{self, PrivateReadError};
use crate::runtime::AvailabilityMode;
use crate::{LoadError, PersistError, ProfileConfig, ProfilePaths, TrustBoundary, persist};

/// The overlay's file name in the profile's state directory.
pub const AVAILABILITY_OVERLAY_FILE: &str = "availability-overlay.json";

/// The most an availability overlay may hold on disk; its one legal
/// shape is a few dozen bytes, and anything past this is refused before
/// it is parsed.
pub const MAX_AVAILABILITY_OVERLAY_BYTES: u64 = 4096;

/// The person chose to stay reachable: the only choice the overlay
/// records. Its absence is the authored default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StayReachable;

/// The file's one shape.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OnDisk {
    availability_mode: OnlyStayReachable,
}

/// The one value the file may name: `foreground-only` does not parse.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum OnlyStayReachable {
    StayReachable,
}

/// Why the effective availability mode cannot be read, or a choice
/// could not be kept.
#[derive(Debug)]
pub enum AvailabilityError {
    /// The file is present and could not be read.
    Read(std::io::Error),
    /// The file, or its directory, is not private to this uid, or the
    /// file is a symbolic link or not a regular file.
    NotPrivate {
        /// What is wrong with it.
        detail: String,
    },
    /// The file is larger than [`MAX_AVAILABILITY_OVERLAY_BYTES`].
    TooLarge,
    /// The file is not the overlay's one shape.
    Parse(String),
    /// `config.yaml`, which holds the authored default, cannot be loaded.
    Config(LoadError),
    /// The choice could not be written or removed.
    Write(PersistError),
}

impl core::fmt::Display for AvailabilityError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "the availability overlay cannot be read: {e}"),
            Self::NotPrivate { detail } => {
                write!(f, "the availability overlay is not private: {detail}")
            }
            Self::TooLarge => write!(
                f,
                "the availability overlay is larger than {MAX_AVAILABILITY_OVERLAY_BYTES} bytes"
            ),
            Self::Parse(e) => write!(f, "the availability overlay does not parse: {e}"),
            Self::Config(e) => write!(f, "the authored availability mode cannot be read: {e}"),
            Self::Write(e) => write!(f, "the availability overlay cannot be written: {e}"),
        }
    }
}

impl core::error::Error for AvailabilityError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Read(e) => Some(e),
            Self::Config(e) => Some(e),
            Self::Write(e) => Some(e),
            _ => None,
        }
    }
}

impl From<PrivateReadError> for AvailabilityError {
    fn from(e: PrivateReadError) -> Self {
        match e {
            PrivateReadError::Read(e) => Self::Read(e),
            PrivateReadError::NotPrivate { detail } => Self::NotPrivate { detail },
            PrivateReadError::TooLarge => Self::TooLarge,
        }
    }
}

/// The overlay's path for `paths`' profile.
#[must_use]
pub fn path_for(paths: &ProfilePaths) -> PathBuf {
    paths.state_dir().join(AVAILABILITY_OVERLAY_FILE)
}

/// The choice the overlay at `path` records: `None` when absent, its
/// state directory included.
///
/// # Errors
/// Every [`AvailabilityError`] but `Config` and `Write`.
pub fn read_within(
    path: &Path,
    boundary: &TrustBoundary,
) -> Result<Option<StayReachable>, AvailabilityError> {
    let Some(text) =
        private_read::read_private_within(path, boundary, MAX_AVAILABILITY_OVERLAY_BYTES)?
    else {
        return Ok(None);
    };
    let OnDisk {
        availability_mode: OnlyStayReachable::StayReachable,
    } = serde_json::from_str(&text).map_err(|e| AvailabilityError::Parse(e.to_string()))?;
    Ok(Some(StayReachable))
}

/// Record `choice` at `path`: `Some` writes the overlay whole (a
/// temporary file, owner-only from creation, renamed into place), `None`
/// removes it. The caller holds the profile lock, so it is the only
/// writer.
///
/// # Errors
/// [`AvailabilityError::Write`]. A failure before the rename or the
/// unlink leaves the previous state as it was; a
/// [`PersistError::Unsynced`] leaves the new state in place, its name
/// perhaps not durable.
pub fn write_within(
    path: &Path,
    choice: Option<StayReachable>,
    boundary: &TrustBoundary,
) -> Result<(), AvailabilityError> {
    match choice {
        Some(StayReachable) => {
            let text = serde_json::to_vec_pretty(&OnDisk {
                availability_mode: OnlyStayReachable::StayReachable,
            })
            .map_err(|e| AvailabilityError::Write(PersistError::Io(std::io::Error::other(e))))?;
            persist::write_private_atomic_within(path, &text, boundary)
        }
        None => persist::remove_private_within(path, boundary).map(|_| ()),
    }
    .map_err(AvailabilityError::Write)
}

/// The effective mode: the overlay's choice when there is one, else the
/// authored field.
#[must_use]
pub const fn effective(
    authored: AvailabilityMode,
    overlay: Option<StayReachable>,
) -> AvailabilityMode {
    match overlay {
        Some(StayReachable) => AvailabilityMode::StayReachable,
        None => authored,
    }
}

/// The profile's one effective availability mode, read from disk with
/// no host running: `config.yaml`'s authored field, overridden by the
/// overlay when present.
///
/// # Errors
/// [`AvailabilityError::Config`] when `config.yaml` cannot be loaded;
/// the overlay's errors as [`read_within`].
pub fn effective_availability_mode(
    paths: &ProfilePaths,
) -> Result<AvailabilityMode, AvailabilityError> {
    let config = ProfileConfig::load(paths).map_err(AvailabilityError::Config)?;
    let overlay = read_within(&path_for(paths), paths.boundary())?;
    Ok(effective(config.runtime.android.availability_mode, overlay))
}
