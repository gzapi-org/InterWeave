// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The profile lock (plan §16 (6), precondition P5): one holder per
//! profile, so "the daemon is stopped" is a mechanical fact rather than a
//! belief. The daemon holds it for its lifetime; the offline identity
//! commands that write the key hold it while they do. And the human
//! client's lock (Stage 15, R5): held in `human_dir()` beside the
//! client's store, refusing a second acquirer, which is what a desktop
//! client needs to be the profile's only one. Two locks on one
//! mechanism, and neither excludes the other.
//!
//! `<state_dir>/profile.lock` and `<human_dir>/human-desktop.lock`, mode
//! `0600`, exclusive through
//! `std::fs::File::try_lock` (flock; no unsafe code). RELEASED,
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

/// The human client's lock file's name inside `human_dir()`.
pub const HUMAN_CLIENT_LOCK_FILE: &str = "human-desktop.lock";

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
    /// refuses one that is not owner-only or not owned by this process's
    /// effective uid, and a lock file that is not a single-link,
    /// owner-only regular file of that uid -- judged on the opened file
    /// before it is written (`open_lock_file`).
    ///
    /// # Errors
    /// [`PersistError::ProfileLocked`] if another holder keeps it past
    /// `wait`; [`PersistError::DirectoryNotPrivate`] for a state
    /// directory wider than owner-only or owned by another uid;
    /// [`PersistError::FileNotPrivate`] for a lock file that is a link,
    /// wider than owner-only or another uid's; [`PersistError::Io`]
    /// otherwise.
    pub fn acquire(paths: &ProfilePaths, wait: Duration) -> Result<Self, PersistError> {
        let path = Self::path_for(paths);
        let file = acquire_in(&[paths.state_dir()], &path, wait, |path| {
            PersistError::ProfileLocked { path }
        })?;
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
        held_in(&[paths.state_dir()], &Self::path_for(paths))
    }

    /// Where the lock lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A held human-client lock: this process is the profile's one desktop
/// human client. Dropping it releases the lock; the file stays.
#[derive(Debug)]
pub struct HumanClientLock {
    // Held for its lock: flock is released when the file is closed.
    _file: File,
    path: PathBuf,
}

impl HumanClientLock {
    /// The lock file for `paths`' human client.
    #[must_use]
    pub fn path_for(paths: &ProfilePaths) -> PathBuf {
        paths.human_dir().join(HUMAN_CLIENT_LOCK_FILE)
    }

    /// Take the human client's lock, retrying for up to `wait`, under
    /// the same rules as [`ProfileLock::acquire`] for BOTH directories it
    /// lives under -- the state directory and `human_dir()` within it:
    /// created owner-only if missing, and refused when either is wider
    /// than owner-only or another uid's. The state directory is judged
    /// too because whoever can write it can rename `human_dir()` away
    /// and let a second client lock a fresh one
    /// (`a_wide_state_directory_is_refused_for_the_client_too`).
    ///
    /// # Errors
    /// [`PersistError::InstanceLocked`] if another holder keeps it past
    /// `wait`; otherwise as [`ProfileLock::acquire`].
    pub fn acquire(paths: &ProfilePaths, wait: Duration) -> Result<Self, PersistError> {
        let path = Self::path_for(paths);
        let human = paths.human_dir();
        let file = acquire_in(&[paths.state_dir(), &human], &path, wait, |path| {
            PersistError::InstanceLocked { path }
        })?;
        Ok(Self { _file: file, path })
    }

    /// Whether another process holds the human client's lock right now:
    /// a `try_lock` released at once. A missing lock file is not held.
    ///
    /// # Errors
    /// As [`HumanClientLock::acquire`], without creating anything.
    pub fn is_held(paths: &ProfilePaths) -> Result<bool, PersistError> {
        held_in(
            &[paths.state_dir(), &paths.human_dir()],
            &Self::path_for(paths),
        )
    }

    /// Where the lock lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Open and take the lock at `path`, under `dirs` (outermost first, the
/// last holding the file), retrying for up to `wait`; `locked` names the
/// refusal when another holder keeps it.
fn acquire_in(
    dirs: &[&Path],
    path: &Path,
    wait: Duration,
    locked: fn(PathBuf) -> PersistError,
) -> Result<File, PersistError> {
    let file = open_lock_file(dirs, path, true)?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(RETRY);
            }
            Err(TryLockError::WouldBlock) => return Err(locked(path.to_path_buf())),
            Err(TryLockError::Error(e)) => return Err(PersistError::Io(e)),
        }
    }
    write_diagnostics(&file).map_err(PersistError::Io)?;
    Ok(file)
}

/// Whether another process holds the lock at `path`, under `dirs`.
///
/// The directories are judged BEFORE an absent file is read as "not
/// held": a file removed through a directory others can write leaves
/// its holder holding the old inode, so "no file" says nothing there
/// (`a_wide_human_dir_is_refused_with_or_without_the_file`). Only an
/// absent directory, which holds no file and no holder's file, is not
/// held as it stands.
fn held_in(dirs: &[&Path], path: &Path) -> Result<bool, PersistError> {
    for dir in dirs {
        match std::fs::symlink_metadata(dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(PersistError::Io(e)),
            Ok(_) => require_owned_private_dir(dir)?,
        }
    }
    // `symlink_metadata`, not `exists`: `exists` follows a link, and a
    // dangling one read as "no lock file", i.e. "not running" (#145
    // re-review 2). Only a path that is genuinely absent is not held.
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(PersistError::Io(e)),
        Ok(_) => {}
    }
    let file = open_lock_file(dirs, path, false)?;
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(e)) => Err(PersistError::Io(e)),
    }
}

/// `O_NOFOLLOW`, taken from `libc` rather than spelled per architecture:
/// the value differs by ABI (0o400000 on `x86_64` and riscv64, 0o100000
/// on aarch64, arm and powerpc), a hand-typed table once gave aarch64 the
/// `x86_64` value, and CI runs one architecture, so no test here could
/// have caught it. `None` off Linux refuses the lock there before
/// anything is touched.
#[cfg(target_os = "linux")]
const O_NOFOLLOW: Option<i32> = Some(libc::O_NOFOLLOW);
#[cfg(not(target_os = "linux"))]
const O_NOFOLLOW: Option<i32> = None;

/// This process's effective uid, from `/proc/self/status`: `geteuid`
/// would be the crate's one unsafe call.
///
/// # Errors
/// [`PersistError::UnsupportedPlatform`] where it cannot be read.
fn effective_uid() -> Result<u32, PersistError> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|_| PersistError::UnsupportedPlatform)?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().nth(1))
        .and_then(|id| id.parse().ok())
        .ok_or(PersistError::UnsupportedPlatform)
}

/// Open the lock file, deciding on its directories' owner and the
/// OPENED HANDLE before anything is written (#145 review F1; re-review 2).
///
/// 1. Each of `dirs` -- the state directory, and for the human client's
///    lock `human_dir()` within it -- must be owner-only AND owned by
///    this process's effective uid: a process working in another
///    account's directory (root with that account's environment) is
///    refused before any file is touched, which is the case a planted
///    link was built for.
/// 2. A lock path that is a link or not a regular file is refused.
/// 3. The open carries `O_NOFOLLOW`, so a link raced in after (2) makes
///    the open fail rather than create or open through it.
/// 4. The opened file must be a regular file with one link, owner-only,
///    and owned by this process (`tests/lock.rs`: a planted symlink, a
///    dangling one, a hard link, a wide mode).
///
/// NOT CLOSED: swapping the OUTERMOST directory -- the state directory --
/// between (1) and (3) needs write access to its parent, and without
/// `openat` the path is resolved twice. That parent is
/// `<XDG state root>/interweave/profiles`, shared by every profile and
/// judged by nothing here (created owner-only when this crate makes it),
/// and a directory whose parent another account can write is the
/// operator's to avoid. Every directory inside it is
/// judged here, so a swap there needs a directory (1) refused.
fn open_lock_file(dirs: &[&Path], path: &Path, create: bool) -> Result<File, PersistError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        let no_follow = O_NOFOLLOW.ok_or(PersistError::UnsupportedPlatform)?;
        let uid = effective_uid()?;
        if create && let Some(innermost) = dirs.last() {
            create_private_dir(innermost)?;
        }
        for dir in dirs {
            require_owned_private_dir(dir)?;
        }
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
        // current holder's text is not the opener's to erase. Mode set at
        // CREATION, as every owner-only file in this crate is.
        options
            .read(true)
            .write(true)
            .create(create)
            .mode(crate::OWNER_ONLY_FILE)
            .custom_flags(no_follow);
        let file = options.open(path).map_err(PersistError::Io)?;
        let opened = file.metadata().map_err(PersistError::Io)?;
        if !opened.file_type().is_file()
            || opened.nlink() != 1
            || opened.mode() & 0o077 != 0
            || opened.uid() != uid
        {
            return Err(not_private());
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        let _ = (dirs, path, create);
        Err(PersistError::UnsupportedPlatform)
    }
}

/// `dir` is owner-only and owned by this process's effective uid.
fn require_owned_private_dir(dir: &Path) -> Result<(), PersistError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        require_private_dir(dir)?;
        let uid = effective_uid()?;
        let owner = std::fs::symlink_metadata(dir)
            .map_err(PersistError::Io)?
            .uid();
        if owner != uid {
            return Err(PersistError::DirectoryNotPrivate {
                path: dir.to_path_buf(),
                detail: format!("owned by uid {owner}, not this process's {uid}"),
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

// Linux only, as the lock is: `O_NOFOLLOW` is `None` on every other
// target, Android included, and the lock refuses there before it reads
// `/proc/self/status`.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    #![allow(clippy::expect_used)]
    use super::{O_NOFOLLOW, effective_uid};

    /// The flag the lock opens with refuses a link: opening one with it
    /// fails with ELOOP, and the same open without it succeeds (the
    /// control).
    #[test]
    fn the_no_follow_flag_refuses_a_link() {
        use std::os::unix::fs::OpenOptionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        std::fs::write(&target, b"x").expect("write");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("link");
        let flag = O_NOFOLLOW.expect("a supported target");
        // ELOOP, not merely an error: another flag that fails the same
        // open for its own reason -- O_DIRECTORY's ENOTDIR -- would pass a
        // bare check.
        let refused = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flag)
            .open(&link)
            .expect_err("the flag refuses a link");
        assert_eq!(
            refused.raw_os_error(),
            Some(libc::ELOOP),
            "ELOOP: {refused}"
        );
        assert!(
            std::fs::OpenOptions::new().read(true).open(&link).is_ok(),
            "the control: without it the link is followed"
        );
    }

    /// The uid read from /proc is the one this process creates files as.
    #[test]
    fn the_effective_uid_is_the_owner_of_what_this_process_creates() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("mine");
        std::fs::write(&file, b"").expect("write");
        assert_eq!(
            effective_uid().expect("readable"),
            std::fs::metadata(&file).expect("meta").uid()
        );
    }
}
