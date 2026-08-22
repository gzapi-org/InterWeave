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
    // PRIVATE MATERIAL GETS A PRIVATE PARENT, created as one and then
    // checked. `create_dir_all` produces a `0755` directory when the
    // path does not exist yet, and does nothing at all when it does --
    // so the module's own statement that "the directory matters as much
    // as the file" was an argument the code did not make.
    if mode.is_some() {
        create_private_dir(parent)?;
        require_private_dir(parent)?;
    } else {
        fs::create_dir_all(parent).map_err(PersistError::Io)?;
    }

    let temp = temp_beside(path);

    let mut file = open_for_write(&temp, mode)?;
    // The owner check needs a file this process made; the temporary is
    // the first one there is. Done before any content is written, so a
    // parent belonging to someone else never receives bytes.
    if mode.is_some()
        && let Err(e) = require_same_owner(parent, &file)
    {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
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
    create_private_dir(parent)?;
    require_private_dir(parent)?;

    let temp = temp_beside(path);
    let mut file = open_for_write(&temp, Some(OWNER_ONLY_FILE))?;
    if let Err(e) = require_same_owner(parent, &file) {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
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
/// Process id and a per-process counter separate writers. They do not
/// make the name UNGUESSABLE, and the temporary is opened with
/// `create_new`, so a name another account can predict is a name they
/// can occupy first and turn every write into a failure. The parent is
/// required to be owner-only before any of this runs, which is the real
/// defence; the random component means the predictable-name attack does
/// not become live the moment someone relaxes that requirement.
///
/// The entropy is `RandomState`, which std seeds per process from the
/// OS. It is not a CSPRNG and does not need to be: the requirement is
/// that another account cannot compute the name, not that the name
/// resists cryptanalysis.
fn temp_beside(path: &Path) -> std::path::PathBuf {
    use std::hash::{BuildHasher as _, Hasher as _};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    static SEED: std::sync::OnceLock<std::collections::hash_map::RandomState> =
        std::sync::OnceLock::new();

    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut h = SEED
        .get_or_init(std::collections::hash_map::RandomState::new)
        .build_hasher();
    h.write_u64(n);
    h.write_u32(std::process::id());

    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(
        ".{}.{n}.{:016x}.tmp",
        std::process::id(),
        h.finish()
    ));
    std::path::PathBuf::from(temp)
}

/// Refuse a directory that is not owner-only.
///
/// Ownership is a separate question and is answered by
/// [`require_same_owner`], which needs a file this process created to
/// compare against.
///
/// # Errors
/// Returns [`PersistError::DirectoryNotPrivate`] if the mode is wider
/// than [`OWNER_ONLY_DIR`] or the path is a symbolic link,
/// [`PersistError::Io`] if it cannot be inspected, or
/// [`PersistError::UnsupportedPlatform`] where this cannot be checked.
pub fn require_private_dir(dir: &Path) -> Result<(), PersistError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        // `symlink_metadata`, not `metadata`: a symlink pointing at a
        // directory that IS `0700` says nothing about who can move the
        // link, and following it is how this check gets satisfied by
        // somewhere other than where the write lands.
        let meta = fs::symlink_metadata(dir).map_err(PersistError::Io)?;
        if meta.file_type().is_symlink() {
            return Err(PersistError::DirectoryNotPrivate {
                path: dir.to_path_buf(),
                detail: "it is a symbolic link".to_owned(),
            });
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(PersistError::DirectoryNotPrivate {
                path: dir.to_path_buf(),
                detail: format!("mode is {mode:04o}, wider than {OWNER_ONLY_DIR:04o}"),
            });
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Err(PersistError::UnsupportedPlatform)
    }
}

/// Refuse a directory owned by somebody else.
///
/// # Why the comparison is against a file we just made
///
/// The obvious spelling is `getuid()`, and this crate is
/// `forbid(unsafe_code)` with no libc dependency -- so the effective
/// uid is not directly reachable. It does not need to be: `ours` was
/// created by this process moments ago, so its owner IS the identity
/// the kernel would have returned, read through a safe API. A parent
/// whose uid differs is a directory somebody else can rewrite,
/// whatever its mode says.
///
/// # Errors
/// Returns [`PersistError::DirectoryNotPrivate`] on a mismatch and
/// [`PersistError::Io`] if either cannot be inspected.
fn require_same_owner(dir: &Path, ours: &fs::File) -> Result<(), PersistError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let mine = ours.metadata().map_err(PersistError::Io)?.uid();
        let theirs = fs::symlink_metadata(dir).map_err(PersistError::Io)?.uid();
        if mine != theirs {
            return Err(PersistError::DirectoryNotPrivate {
                path: dir.to_path_buf(),
                detail: format!("owned by uid {theirs}, not {mine}"),
            });
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let (_, _) = (dir, ours);
        Err(PersistError::UnsupportedPlatform)
    }
}

fn open_for_write(path: &Path, mode: Option<u32>) -> Result<fs::File, PersistError> {
    let mut options = fs::OpenOptions::new();
    // `create_new`, not `create().truncate()`. `O_CREAT|O_EXCL` refuses
    // to follow a symlink and refuses an existing file, so a temporary
    // name someone pre-created -- as a link into a file they want
    // overwritten, or simply to be in the way -- is an error here
    // instead of a write somewhere else.
    options.write(true).create_new(true);

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
