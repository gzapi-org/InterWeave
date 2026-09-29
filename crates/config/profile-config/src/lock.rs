// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The profile lock (plan §16 (6), precondition P5): one holder per
//! profile, so "the daemon is stopped" is a mechanical fact rather than a
//! belief. The daemon holds it for its lifetime; the offline identity
//! commands that write the key hold it while they do.
//!
//! `<state_dir>/profile.lock`, mode `0600`, exclusive through
//! `std::fs::File::try_lock` (flock; no unsafe code, no libc). RELEASED,
//! NEVER UNLINKED: unlinking a flock file lets a second process create and
//! lock a new inode while the first still holds the old one, and both
//! believe they are alone. The pid and start time written into it are
//! diagnostic text for a person reading the file, never read back as a
//! claim.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{PersistError, ProfilePaths, create_private_dir, require_private_dir};

/// The lock file's name inside the profile's state directory.
pub const LOCK_FILE: &str = "profile.lock";

/// How long the daemon retries its own acquisition before failing: long
/// enough that a probe's momentary hold cannot fail a starting daemon
/// (plan §16 (10)), short enough that a running one is reported at once.
pub const DAEMON_LOCK_WAIT: Duration = Duration::from_secs(1);

/// Between attempts while waiting.
const RETRY: Duration = Duration::from_millis(20);

/// A held profile lock. Dropping it releases the lock; the file stays.
#[derive(Debug)]
pub struct ProfileLock {
    // Held for its lock: flock is released when the file is closed.
    _file: File,
    path: PathBuf,
}

impl ProfileLock {
    /// The lock file for `paths`' profile.
    #[must_use]
    pub fn path_for(paths: &ProfilePaths) -> PathBuf {
        paths.state_dir().join(LOCK_FILE)
    }

    /// Take the profile's lock, retrying for up to `wait`.
    ///
    /// Creates the state directory owner-only if it is missing, and
    /// refuses one that is not owner-only, and a lock file that is not a
    /// single-link, owner-only regular file of the directory's owner --
    /// judged on the opened file before it is written (`open_lock_file`).
    ///
    /// # Errors
    /// [`PersistError::ProfileLocked`] if another holder keeps it past
    /// `wait`; [`PersistError::DirectoryNotPrivate`] or
    /// [`PersistError::FileNotPrivate`] for a state directory or lock
    /// file wider than owner-only; [`PersistError::Io`] otherwise.
    pub fn acquire(paths: &ProfilePaths, wait: Duration) -> Result<Self, PersistError> {
        let path = Self::path_for(paths);
        let file = open_lock_file(paths, &path, true)?;
        let deadline = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(RETRY);
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(PersistError::ProfileLocked { path });
                }
                Err(TryLockError::Error(e)) => return Err(PersistError::Io(e)),
            }
        }
        write_diagnostics(&file).map_err(PersistError::Io)?;
        Ok(Self { _file: file, path })
    }

    /// Whether another process holds the profile's lock right now: a
    /// `try_lock` released at once, which is how the admin tool tells "the
    /// daemon is not running" from "its socket is missing". A missing
    /// lock file is not held.
    ///
    /// # Errors
    /// As [`ProfileLock::acquire`], without creating anything.
    pub fn is_held(paths: &ProfilePaths) -> Result<bool, PersistError> {
        let path = Self::path_for(paths);
        if !path.exists() {
            return Ok(false);
        }
        let file = open_lock_file(paths, &path, false)?;
        match file.try_lock() {
            Ok(()) => Ok(false),
            Err(TryLockError::WouldBlock) => Ok(true),
            Err(TryLockError::Error(e)) => Err(PersistError::Io(e)),
        }
    }

    /// Where the lock lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Open the lock file, judging it by the OPENED HANDLE before anything
/// is written (#145 review F1).
///
/// A path check alone was the defect: `is_owner_only` read the path's
/// metadata, which follows a link, and the diagnostics then truncated
/// whatever the path reached. A privileged process whose state
/// directory belongs to another account (root with that account's
/// environment) would follow a planted `profile.lock -> /etc/shadow`
/// (mode `0000`, "owner-only") and truncate it. So: a lock path that is
/// a link or not a regular file is refused before opening, and the
/// opened file must be a regular file with one link, owner-only, and
/// owned by the state directory's owner -- a link the pre-check raced
/// with lands on a file of another owner, or with another link count,
/// and is refused there (`tests/lock.rs`: a planted symlink, a hard
/// link, a wide mode).
fn open_lock_file(paths: &ProfilePaths, path: &Path, create: bool) -> Result<File, PersistError> {
    if create {
        create_private_dir(paths.state_dir())?;
    }
    require_private_dir(paths.state_dir())?;
    let not_private = || PersistError::FileNotPrivate {
        path: path.to_path_buf(),
    };
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => return Err(not_private()),
        Ok(_) => {}
        Err(e) if create && e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(PersistError::Io(e)),
    }
    let mut options = OpenOptions::new();
    // Write access for the diagnostics; never truncate on open: the
    // current holder's text is not the opener's to erase.
    options.read(true).write(true).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Set at CREATION, as every owner-only file in this crate is.
        options.mode(crate::OWNER_ONLY_FILE);
    }
    let file = options.open(path).map_err(PersistError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = file.metadata().map_err(PersistError::Io)?;
        let dir = std::fs::symlink_metadata(paths.state_dir()).map_err(PersistError::Io)?;
        if !opened.file_type().is_file()
            || opened.nlink() != 1
            || opened.mode() & 0o077 != 0
            || opened.uid() != dir.uid()
        {
            return Err(not_private());
        }
    }
    #[cfg(not(unix))]
    {
        let _ = &file;
        return Err(PersistError::UnsupportedPlatform);
    }
    Ok(file)
}

fn write_diagnostics(mut file: &File) -> std::io::Result<()> {
    let started_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    file.set_len(0)?;
    write!(
        file,
        "pid {}\nstarted_unix_ms {started_ms}\n",
        std::process::id()
    )?;
    file.flush()
}
