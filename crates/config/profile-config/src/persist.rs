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

    let temp = temp_beside(path);

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

/// Install `contents` at `path` only if nothing is there, owner-only.
///
/// The distinction from [`write_private_atomic`] is WHO decides that the
/// target is free. A caller that checks first and writes second has a
/// window between the two, and two processes initializing the same
/// profile both pass the check before either writes — so the loser
/// silently replaces an identity the winner had already established.
/// Here the filesystem decides, in the operation that installs the file.
///
/// `link` is what makes that one operation: it fails with `EEXIST` if
/// the target exists, and unlike `rename` it never replaces. The content
/// is still written and fsynced to a private temporary first, so the
/// file is whole before it has a name and a crash cannot publish a
/// partial key.
///
/// # Errors
/// Returns [`PersistError::AlreadyExists`] if `path` is taken,
/// [`PersistError::Io`] if any step fails, or
/// [`PersistError::UnsupportedPlatform`] where owner-only permissions
/// cannot be enforced. Nothing at `path` is touched in any case.
pub fn create_private_exclusive(path: &Path, contents: &[u8]) -> Result<(), PersistError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(PersistError::Io)?;

    let temp = temp_beside(path);
    let mut file = open_for_write(&temp, Some(OWNER_ONLY_FILE))?;
    let written = file
        .write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(PersistError::Io);
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }

    let outcome = match fs::hard_link(&temp, path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(PersistError::AlreadyExists),
        Err(e) => Err(PersistError::Io(e)),
    };

    // The temporary is always removed: on success the link is the file,
    // and on failure it is unpublished key material.
    let _ = fs::remove_file(&temp);
    outcome?;

    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

/// A temporary path beside `path`, unique to this writer.
///
/// Beside the target because rename is atomic only within one
/// filesystem and a temp directory may be another.
///
/// UNIQUE because a fixed `<path>.tmp` is shared state between every
/// writer of that file. Two processes writing concurrently both open it,
/// and one renames it into place while the other still holds the same
/// inode open and goes on writing — into the file that is now the
/// installed key. The rename is atomic; the name was not private, so
/// atomicity protected nothing.
///
/// Process id and a per-process counter are enough: the id separates
/// processes and the counter separates writers inside one.
fn temp_beside(path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::path::PathBuf::from(temp)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_writer_gets_its_own_temporary() {
        // A fixed `<path>.tmp` is shared state between every writer of
        // that file. Two writers open the same inode, one renames it into
        // place, and the other goes on writing into what is now the
        // installed file — the rename is atomic, but the name it renamed
        // FROM was not private, so atomicity protected nothing.
        let target = Path::new("/tmp/interweave-test/identity.key");
        let a = temp_beside(target);
        let b = temp_beside(target);
        assert_ne!(a, b, "two writers must not share a temporary");

        for t in [&a, &b] {
            assert_eq!(
                t.parent(),
                target.parent(),
                "the temporary stays beside the target, or rename is not atomic"
            );
            assert!(
                t.extension().is_some_and(|e| e == "tmp"),
                "still recognisable as a temporary: {}",
                t.display()
            );
            assert_ne!(t.as_path(), target, "and is never the target itself");
        }
    }
}
