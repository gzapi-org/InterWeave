// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Atomic, owner-only writes for configuration, state, and identity.
//!
//! # Atomic means the reader never sees a partial file
//!
//! Write to a temporary in the SAME directory, fsync it, rename over the
//! target. Same directory because rename is only atomic within a
//! filesystem; fsync before rename because otherwise the rename can land
//! before the bytes and a crash leaves a correctly-named empty file,
//! which is worse than a missing one — it looks valid.
//!
//! # Owner-only means owner-only from creation
//!
//! The mode is set when the file is CREATED, not chmod'd afterwards.
//! Creating a key file world-readable and narrowing it a moment later
//! leaves a window in which any local process can read it, and that
//! window is exactly what an attacker on a shared machine waits for.
//!
//! Standard v1 relies on filesystem and OS-account protection at rest;
//! ADR-0038's passphrase-encrypted envelope is a v2.x direction gated by
//! SPIKE-007 and is deliberately not invented here.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use crate::PersistError;

/// Mode for files nobody but the owner may read: `0600`.
pub const OWNER_ONLY_FILE: u32 = 0o600;

/// Mode for directories nobody but the owner may traverse: `0700`.
///
/// The directory matters as much as the file. A `0600` key inside a
/// world-executable directory still leaks its existence, its size, and
/// its modification time — and a directory an attacker can write to lets
/// them replace the key outright.
pub const OWNER_ONLY_DIR: u32 = 0o700;

/// Create `dir` and every missing parent, owner-only.
///
/// # Errors
/// Returns [`PersistError::Io`] if creation fails, or
/// [`PersistError::UnsupportedPlatform`] where owner-only permissions
/// cannot be enforced.
pub fn create_private_dir(dir: &Path) -> Result<(), PersistError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(OWNER_ONLY_DIR)
            .create(dir)
            .map_err(PersistError::Io)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Err(PersistError::UnsupportedPlatform)
    }
}

/// Write `contents` to `path` atomically, readable only by the owner.
///
/// Used for the identity key and for any state whose exposure matters.
///
/// # Errors
/// Returns [`PersistError::Io`] if any step fails, or
/// [`PersistError::UnsupportedPlatform`] where owner-only permissions
/// cannot be enforced. A failure leaves the previous file untouched:
/// nothing is removed until the replacement is fully on disk.
pub fn write_private_atomic(path: &Path, contents: &[u8]) -> Result<(), PersistError> {
    write_atomic_with_mode(path, contents, Some(OWNER_ONLY_FILE))
}

/// Write `contents` to `path` atomically with default permissions.
///
/// For configuration, which is not secret. The identity key must use
/// [`write_private_atomic`] instead.
///
/// # Errors
/// Returns [`PersistError::Io`] if any step fails.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), PersistError> {
    write_atomic_with_mode(path, contents, None)
}

fn write_atomic_with_mode(
    path: &Path,
    contents: &[u8],
    mode: Option<u32>,
) -> Result<(), PersistError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(PersistError::Io)?;

    // The temporary lives beside the target, because rename is atomic
    // only within one filesystem and a temp directory may be another.
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = std::path::PathBuf::from(temp);

    let mut file = open_for_write(&temp, mode)?;
    file.write_all(contents).map_err(PersistError::Io)?;
    file.sync_all().map_err(PersistError::Io)?;
    drop(file);

    fs::rename(&temp, path).map_err(|e| {
        // Do not leave the temporary behind on a failed rename; a
        // half-finished `identity.key.tmp` is still key material.
        let _ = fs::remove_file(&temp);
        PersistError::Io(e)
    })?;

    // fsync the DIRECTORY too, so the rename itself survives a crash.
    // Without this the file contents are durable but the name they were
    // renamed to may not be.
    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

fn open_for_write(path: &Path, mode: Option<u32>) -> Result<fs::File, PersistError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Set at CREATION. A chmod after the fact leaves a window in
        // which the key is world-readable.
        options.mode(mode);
    }
    #[cfg(not(unix))]
    if mode.is_some() {
        return Err(PersistError::UnsupportedPlatform);
    }

    options.open(path).map_err(PersistError::Io)
}

/// Whether `path` is readable by nobody but its owner.
///
/// Checked rather than assumed, because a key file written correctly by
/// this build may have been created by an older one, restored from a
/// backup, or copied with `cp -p` from somewhere less careful. A caller
/// that finds this false should refuse to load the key.
///
/// # Errors
/// Returns [`PersistError::Io`] if the file cannot be inspected, or
/// [`PersistError::UnsupportedPlatform`] where permissions cannot be
/// checked.
pub fn is_owner_only(path: &Path) -> Result<bool, PersistError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(path)
            .map_err(PersistError::Io)?
            .permissions()
            .mode();
        Ok(mode & 0o077 == 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(PersistError::UnsupportedPlatform)
    }
}
