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
use std::path::{Path, PathBuf};

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
/// JUDGED BEFORE ANYTHING IS CREATED: the nearest directory on `dir`'s
/// path that exists is held to [`resolve_guarded_dir`]'s rule first, and
/// the missing components are made beneath it as that judgement resolved
/// it, so a refusal leaves the tree as it found it. A recursive create
/// made the whole tree first and left it under the ancestor the caller's
/// check then refused (`a_refused_ancestor_gets_nothing_created_under_it`).
/// A `dir` that already exists creates nothing and is judged by nothing
/// here: whether it is private is the caller's question. It must be a
/// directory, or one through a link, as the recursive create required:
/// anything else is [`std::io::ErrorKind::AlreadyExists`]
/// (`an_existing_path_that_is_not_a_directory_is_refused`).
///
/// # Errors
/// [`PersistError::DirectoryNotPrivate`] naming the existing ancestor,
/// link or appeared component that breaks the rule; [`PersistError::Io`]
/// if creation fails, or a missing part of the path is `..`;
/// [`PersistError::UnsupportedPlatform`] where owner-only permissions
/// cannot be enforced -- every target but Linux, the uid being read from
/// `/proc`.
pub fn create_private_dir(dir: &Path) -> Result<(), PersistError> {
    #[cfg(unix)]
    {
        let uid = effective_uid()?;
        // The nearest component there is -- `/`, or for a relative `dir`
        // the working directory, at worst -- and what is missing below it.
        let mut existing = None;
        for ancestor in dir.ancestors() {
            let probe = if ancestor.as_os_str().is_empty() {
                Path::new(".")
            } else {
                ancestor
            };
            match fs::symlink_metadata(probe) {
                Ok(_) => {
                    existing = Some((ancestor, probe));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(PersistError::Io(e)),
            }
        }
        let Some((ancestor, probe)) = existing else {
            return Err(PersistError::Io(std::io::ErrorKind::NotFound.into()));
        };
        let missing = dir.strip_prefix(ancestor).map_err(|_| {
            PersistError::Io(std::io::Error::other(
                "a path that is not under its ancestor",
            ))
        })?;
        if missing.as_os_str().is_empty() {
            // Followed, as `create_dir_all`'s `is_dir` follows: a link is
            // the caller's to judge, and every caller judges links.
            return match fs::metadata(probe) {
                Ok(meta) if meta.is_dir() => Ok(()),
                _ => Err(PersistError::Io(std::io::ErrorKind::AlreadyExists.into())),
            };
        }
        let base = resolve_guarded_dir_as(probe, uid)?;
        create_each_as(&base, missing, uid)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Err(PersistError::UnsupportedPlatform)
    }
}

/// Create `missing`'s components under `base`, outermost first, each
/// owner-only and each by itself rather than in one recursive call.
///
/// ONE AT A TIME because `base` may be a sticky directory others can
/// create in: a component that appears between the judgement of `base`
/// and its own creation is ADOPTED only if it is an owner-only directory
/// of `uid`'s -- another of our processes making it -- and refused before
/// anything is made inside it otherwise
/// (`a_component_that_appears_unprivate_is_refused_before_anything_is_made_in_it`,
/// `a_component_another_uid_owns_is_refused_before_anything_is_made_in_it`).
#[cfg(unix)]
fn create_each_as(base: &Path, missing: &Path, uid: u32) -> Result<(), PersistError> {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::Component;
    // Every name checked before the first is made: `..` under a
    // directory that does not exist yet names nothing the kernel could
    // resolve, and following it by text would leave the judged base.
    let mut names = Vec::new();
    for component in missing.components() {
        match component {
            Component::Normal(name) => names.push(name),
            Component::CurDir => {}
            _ => {
                return Err(PersistError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a missing directory's path climbs with `..`",
                )));
            }
        }
    }
    let mut at = base.to_path_buf();
    for name in names {
        at.push(name);
        match fs::DirBuilder::new().mode(OWNER_ONLY_DIR).create(&at) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                at = resolve_owned_private_dir_as(&at, uid)?;
            }
            Err(e) => return Err(PersistError::Io(e)),
        }
    }
    Ok(())
}

/// Write `contents` to `path` atomically, readable only by the owner.
///
/// Used for the identity key and for any state whose exposure matters.
///
/// # Errors
/// Returns [`PersistError::Io`] if any step fails, or
/// [`PersistError::UnsupportedPlatform`] where owner-only permissions
/// cannot be enforced. A failure BEFORE the rename leaves the previous
/// file untouched: nothing is removed until the replacement is fully on
/// disk. The one error that arrives after it is the directory fsync,
/// [`PersistError::Unsynced`], which reports that the new file is in
/// place and its NAME may not survive a crash — a different fact, and the
/// reason it is reported rather than swallowed.
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
    // PRIVATE MATERIAL GETS A PRIVATE PARENT, created as one and then
    // checked. `create_dir_all` produces a `0755` directory when the
    // path does not exist yet, and does nothing at all when it does --
    // so the module's own statement that "the directory matters as much
    // as the file" was an argument the code did not make. Everything
    // after happens under the parent AS RESOLVED by that check
    // (ADR-0028 A 2026-10-08).
    let (parent, path) = if mode.is_some() {
        create_private_dir(parent_dir(path))?;
        let parent = resolve_private_dir(parent_dir(path))?;
        let path = parent.join(file_name(path)?);
        (parent, path)
    } else {
        fs::create_dir_all(parent_dir(path)).map_err(PersistError::Io)?;
        (parent_dir(path).to_path_buf(), path.to_path_buf())
    };
    let (parent, path) = (parent.as_path(), path.as_path());

    let temp = temp_beside(path);

    let mut file = open_for_write(&temp, mode)?;
    // EVERY EXIT BEFORE PUBLICATION REMOVES THE TEMPORARY, whichever
    // step failed: the guard drops on any return, and is disarmed only
    // once the rename has made the temporary the destination. Before
    // it, the owner check and the rename cleaned up and the write and
    // the sync did not -- a partial write or a failed sync returned
    // with `?` and left a uniquely named file holding the attempted
    // contents, which for an identity replacement is key material, and
    // which no retry ever removed since each attempt names its own.
    // Pinned by `a_failed_write_leaves_no_temporary_behind`.
    let unpublished = Unpublished(&temp);
    // The owner check needs a file this process made; the temporary is
    // the first one there is. Done before any content is written, so a
    // parent belonging to someone else never receives bytes.
    if mode.is_some()
        && let Err(e) = require_same_owner(parent, &file)
    {
        drop(file);
        return Err(e);
    }
    file.write_all(contents).map_err(PersistError::Io)?;
    file.sync_all().map_err(PersistError::Io)?;
    drop(file);

    // A failed rename is a failure before publication too: a
    // half-finished `identity.key.tmp` is still key material.
    fs::rename(&temp, path).map_err(PersistError::Io)?;
    unpublished.disarm();

    // fsync the DIRECTORY too, so the rename itself survives a crash.
    // Without this the file contents are durable but the name they were
    // renamed to may not be.
    //
    // REPORTED, not discarded. `let _ =` here meant a failure to make
    // the rename durable was indistinguishable from success, and this
    // function writes a profile's configuration and its identity key —
    // state whose name failing to survive a reboot is the loss of the
    // identity itself. The bytes may well be on disk and the caller
    // cannot know that, which is exactly why it has to be told: an error
    // after the rename says "possibly not durable", and reporting it is
    // the only honest answer available.
    #[cfg(unix)]
    fsync_dir(parent)?;

    Ok(())
}

/// A temporary file that has not been published: removed on drop
/// unless [`Unpublished::disarm`] said the rename made it the
/// destination. Removal failure is ignored -- the file may already be
/// gone, and there is no better report than the error being returned.
struct Unpublished<'a>(&'a Path);

impl Unpublished<'_> {
    /// The temporary is now the destination; leave it.
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for Unpublished<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
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
/// cannot be enforced. Nothing at `path` is touched in any of those
/// cases. The exception is the directory fsync, which runs after the
/// link is published: that error means `path` EXISTS and its name may
/// not survive a crash.
pub fn create_private_exclusive(path: &Path, contents: &[u8]) -> Result<(), PersistError> {
    create_private_dir(parent_dir(path))?;
    // Under the parent as resolved, as `write_atomic_with_mode` works.
    let parent = resolve_private_dir(parent_dir(path))?;
    let path = parent.join(file_name(path)?);
    let (parent, path) = (parent.as_path(), path.as_path());

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

    // As in `save`: the link is published, and whether that name
    // survives a crash is reported rather than assumed.
    #[cfg(unix)]
    fsync_dir(parent)?;

    Ok(())
}

/// fsync a directory, so a rename or link into it survives a crash.
///
/// # Errors
/// Returns [`PersistError::Unsynced`] if the directory cannot be opened
/// or synced. Both are real answers: this is called after the entry is
/// published, so a failure means the name may not be durable, and the
/// caller is the only party that can decide what to do about it -- told
/// apart from a failure before publication, after which nothing changed.
#[cfg(unix)]
fn fsync_dir(parent: &Path) -> Result<(), PersistError> {
    let dir = fs::File::open(parent).map_err(PersistError::Unsynced)?;
    dir.sync_all().map_err(PersistError::Unsynced)
}

/// The directory `path` names a file in, as a path that can be opened.
///
/// `Path::parent` returns `Some("")` for a bare relative filename --
/// NOT `None` -- so `unwrap_or_else(|| Path::new("."))` never fired for
/// exactly the case it was written for. The empty path cannot be
/// inspected, so `symlink_metadata("")` failed with ENOENT and
/// `save(Path::new("identity.key"))` reported a missing directory
/// instead of checking the one it was about to write into.
///
/// Normalising fixes the wrong error, not the refusal: with the current
/// directory as the parent, a private write still requires that
/// directory to be owner-only, which for key material is the answer.
/// The difference is that it now says so.
pub(crate) fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// The name `path` gives its file, to join to the directory as resolved.
///
/// # Errors
/// [`PersistError::Io`] (`InvalidInput`) for a path naming no file --
/// one ending in `..` or `/`.
pub(crate) fn file_name(path: &Path) -> Result<&std::ffi::OsStr, PersistError> {
    path.file_name().ok_or_else(|| {
        PersistError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the path names no file",
        ))
    })
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

/// Refuse a directory that is not owner-only, or whose place on disk
/// another account could change; answer where it is on disk.
///
/// THE DIRECTORY ITSELF: a directory, not a symbolic link, mode within
/// [`OWNER_ONLY_DIR`] -- an owner-only file passed as one, and the open
/// under it failed later as `ENOTDIR`
/// (`a_file_is_not_a_private_directory`). Ownership of the directory itself is a separate
/// question, answered by [`require_same_owner`] for a writer (which
/// compares against a file it just made) and by
/// [`resolve_owned_private_dir_as`] for a caller holding none.
///
/// ITS ANCESTORS AND THE LINKS ON ITS PATH (ADR-0028 A 2026-10-08):
/// see [`resolve_judged_as`]. A directory with a sound mode under an
/// ancestor another account can write is a directory that account can
/// rename away and replace, so the mode alone promised nothing.
///
/// The callers that open -- the private writers, the identity loader,
/// both locks, the trust overlay's read -- open under the RETURNED path:
/// the rule makes that path's components unchangeable by anyone
/// but root and this uid, which is what a check before an open needs to
/// mean -- it is the precondition under which the kernel refuses the
/// swap, so no check-then-open race is left to argue about.
///
/// # Errors
/// Returns [`PersistError::DirectoryNotPrivate`] naming the directory,
/// ancestor or link that broke a rule and which; [`PersistError::Io`] if
/// the directory itself cannot be inspected (`NotFound` among them), or
/// [`PersistError::UnsupportedPlatform`] where this cannot be checked --
/// every target but Linux, the uid being read from `/proc`.
pub fn resolve_private_dir(dir: &Path) -> Result<PathBuf, PersistError> {
    resolve_private_dir_as(dir, effective_uid()?)
}

/// [`resolve_private_dir`] for `uid` -- apart so a test can name a uid
/// that is not this process's, since staging a directory another
/// account owns needs that account.
///
/// # Errors
/// As [`resolve_private_dir`].
pub fn resolve_private_dir_as(dir: &Path, uid: u32) -> Result<PathBuf, PersistError> {
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
        if !meta.is_dir() {
            return Err(PersistError::DirectoryNotPrivate {
                path: dir.to_path_buf(),
                detail: "it is not a directory".to_owned(),
            });
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(PersistError::DirectoryNotPrivate {
                path: dir.to_path_buf(),
                detail: format!("mode is {mode:04o}, wider than {OWNER_ONLY_DIR:04o}"),
            });
        }
        let resolved = resolve_judged_as(dir, uid)?;
        if let Some(parent) = resolved.parent() {
            judge_ancestors(parent, uid)?;
        }
        Ok(resolved)
    }
    #[cfg(not(unix))]
    {
        let (_, _) = (dir, uid);
        Err(PersistError::UnsupportedPlatform)
    }
}

/// [`resolve_private_dir`], for a caller that only asks.
///
/// # Errors
/// As [`resolve_private_dir`].
pub fn require_private_dir(dir: &Path) -> Result<(), PersistError> {
    resolve_private_dir(dir).map(drop)
}

/// Refuse a directory that is not owner-only or not owned by this
/// process's effective uid, and answer where it is on disk:
/// [`resolve_private_dir`] and the ownership question together, for a
/// caller holding no file of its own to compare against -- the profile
/// lock before it creates one, and the identity loader, which only reads.
///
/// LINUX ONLY: the uid is read from `/proc/self/status`
/// ([`effective_uid`]), so every other target answers
/// [`PersistError::UnsupportedPlatform`] -- as the profile lock already
/// does there.
///
/// # Errors
/// [`PersistError::DirectoryNotPrivate`] for a link, a mode wider than
/// [`OWNER_ONLY_DIR`], another owner, or an ancestor or link breaking
/// [`resolve_judged_as`]'s rule; [`PersistError::Io`] if it cannot be
/// inspected; [`PersistError::UnsupportedPlatform`] where the uid cannot
/// be read.
pub fn resolve_owned_private_dir(dir: &Path) -> Result<PathBuf, PersistError> {
    resolve_owned_private_dir_as(dir, effective_uid()?)
}

/// [`resolve_owned_private_dir`] for `uid`, apart so a test can name a
/// uid that is not this process's -- in this crate and in a caller that
/// must show it asks the owner, not only the mode (the identity loader's
/// `a_key_directory_owned_by_another_uid_is_refused`).
///
/// # Errors
/// As [`resolve_owned_private_dir`].
pub fn resolve_owned_private_dir_as(dir: &Path, uid: u32) -> Result<PathBuf, PersistError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let resolved = resolve_private_dir_as(dir, uid)?;
        let owner = std::fs::symlink_metadata(&resolved)
            .map_err(PersistError::Io)?
            .uid();
        if owner != uid {
            return Err(PersistError::DirectoryNotPrivate {
                path: dir.to_path_buf(),
                detail: format!("owned by uid {owner}, not {uid}"),
            });
        }
        Ok(resolved)
    }
    #[cfg(not(unix))]
    {
        let (_, _) = (dir, uid);
        Err(PersistError::UnsupportedPlatform)
    }
}

/// [`resolve_owned_private_dir`], for a caller that only asks.
///
/// # Errors
/// As [`resolve_owned_private_dir`].
pub fn require_owned_private_dir(dir: &Path) -> Result<(), PersistError> {
    resolve_owned_private_dir(dir).map(drop)
}

/// [`require_owned_private_dir`] for `uid`.
///
/// # Errors
/// As [`resolve_owned_private_dir`].
pub fn require_owned_private_dir_as(dir: &Path, uid: u32) -> Result<(), PersistError> {
    resolve_owned_private_dir_as(dir, uid).map(drop)
}

/// A directory that is not private but whose contents decide what is --
/// the configuration directory, whose `config.yaml` names the key path
/// and the allowlist -- judged for who can change it, not for who can
/// read it: the directory itself, its ancestors and the links on its
/// path meet [`resolve_judged_as`]'s rule, with no owner-only mode asked
/// (ADR-0028 A 2026-10-08). Answers where it is on disk, to open under.
///
/// # Errors
/// [`PersistError::DirectoryNotPrivate`] naming what broke the rule;
/// [`PersistError::Io`] if the directory cannot be inspected;
/// [`PersistError::UnsupportedPlatform`] where the uid cannot be read.
pub fn resolve_guarded_dir(dir: &Path) -> Result<PathBuf, PersistError> {
    resolve_guarded_dir_as(dir, effective_uid()?)
}

/// [`resolve_guarded_dir`] for `uid`.
///
/// # Errors
/// As [`resolve_guarded_dir`].
pub fn resolve_guarded_dir_as(dir: &Path, uid: u32) -> Result<PathBuf, PersistError> {
    #[cfg(unix)]
    {
        fs::symlink_metadata(dir).map_err(PersistError::Io)?;
        let resolved = resolve_judged_as(dir, uid)?;
        judge_ancestors(&resolved, uid)?;
        // Asked of the resolved path, which holds no link: a file passed
        // as the directory, and the open under it failed later as
        // `ENOTDIR` (`a_guarded_directory_that_is_a_file_is_refused`).
        if !fs::symlink_metadata(&resolved)
            .map_err(PersistError::Io)?
            .is_dir()
        {
            return Err(PersistError::DirectoryNotPrivate {
                path: dir.to_path_buf(),
                detail: "it is not a directory".to_owned(),
            });
        }
        Ok(resolved)
    }
    #[cfg(not(unix))]
    {
        let (_, _) = (dir, uid);
        Err(PersistError::UnsupportedPlatform)
    }
}

/// How many symbolic links one resolution follows before it refuses, as
/// the kernel's own `ELOOP` limit does.
#[cfg(unix)]
const MAX_LINK_HOPS: u32 = 40;

/// `dir` resolved on disk, component by component, judging every
/// symbolic link it traverses (ADR-0028 A 2026-10-08): the link is owned
/// by root or `uid`, and the directory holding it, its ancestors with it,
/// meet [`judge_ancestor`]'s rule. A link another account could repoint
/// -- one it owns, or one in a directory it can write -- would change
/// where the path leads between the check and the open, out of sight of
/// a walk over the resolved path alone.
///
/// The resolution is the kernel's, done by hand so each link is seen: a
/// relative path starts at the current directory (which `getcwd`
/// answers resolved), `..` takes the parent of what is resolved so far
/// -- links already followed, as the kernel takes it -- and a link's
/// target is resolved in its place.
///
/// # Errors
/// [`PersistError::Io`] for a component that is absent (`NotFound`, which
/// callers read as "not there yet"); [`PersistError::DirectoryNotPrivate`]
/// for a link or an ancestor that breaks the rule, cannot be inspected,
/// or past [`MAX_LINK_HOPS`].
#[cfg(unix)]
fn resolve_judged_as(dir: &Path, uid: u32) -> Result<PathBuf, PersistError> {
    use std::collections::VecDeque;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::Component;

    let start = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir().map_err(PersistError::Io)?.join(dir)
    };
    let mut pending: VecDeque<std::ffi::OsString> = VecDeque::new();
    let mut resolved = PathBuf::from("/");
    for component in start.components() {
        if let Component::Normal(name) = component {
            pending.push_back(name.to_owned());
        } else if component == Component::ParentDir {
            pending.push_back("..".into());
        }
    }
    let mut hops = 0;
    while let Some(name) = pending.pop_front() {
        if name == ".." {
            resolved.pop();
            continue;
        }
        let next = resolved.join(&name);
        let meta = match fs::symlink_metadata(&next) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(PersistError::Io(e));
            }
            Err(e) => return judge_ancestor(&next, Err(e), uid).map(|()| next),
        };
        if !meta.file_type().is_symlink() {
            resolved = next;
            continue;
        }
        hops += 1;
        if hops > MAX_LINK_HOPS {
            return Err(PersistError::DirectoryNotPrivate {
                path: dir.to_path_buf(),
                detail: format!("more than {MAX_LINK_HOPS} symbolic links on its path"),
            });
        }
        judge_link(&next, meta.uid(), meta.permissions().mode(), uid)?;
        judge_ancestors(&resolved, uid)?;
        let target = fs::read_link(&next).map_err(PersistError::Io)?;
        if target.is_absolute() {
            resolved = PathBuf::from("/");
        }
        let mut names: Vec<std::ffi::OsString> = Vec::new();
        for component in target.components() {
            if let Component::Normal(name) = component {
                names.push(name.to_owned());
            } else if component == Component::ParentDir {
                names.push("..".into());
            }
        }
        for name in names.into_iter().rev() {
            pending.push_front(name);
        }
    }
    Ok(resolved)
}

/// `dir` and every directory above it, up to and including `/`, each
/// meeting [`judge_ancestor`]'s rule.
#[cfg(unix)]
fn judge_ancestors(dir: &Path, uid: u32) -> Result<(), PersistError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    for ancestor in dir.ancestors() {
        let seen =
            fs::symlink_metadata(ancestor).map(|m| (m.uid(), m.gid(), m.permissions().mode()));
        judge_ancestor(ancestor, seen, uid)?;
    }
    Ok(())
}

/// The rule for a directory above a private one, or on the path to one
/// (ADR-0028 A 2026-10-08): owned by root or `uid` -- any other owner can
/// rename its entries whatever the mode says -- and carrying no
/// other-write bit, and no group-write bit unless it passes
/// [`owners_private_group`], unless the sticky bit is set: in a sticky
/// directory an entry is renamed only by its owner, the directory's owner
/// or root, and the owner rule above already makes the directory root's
/// or ours. One that cannot be inspected is refused, not judged on its
/// name. `seen` is `(owner, gid, mode)`.
fn judge_ancestor(
    path: &Path,
    seen: std::io::Result<(u32, u32, u32)>,
    uid: u32,
) -> Result<(), PersistError> {
    judge_ancestor_with(path, seen, uid, &HostNames, || access_acl_at(path))
}

/// [`judge_ancestor`] reading `names`, and `acl` for whether the
/// directory carries an access ACL -- asked only of a group-writable one
/// (`a_group_writable_ancestor_needs_the_owners_private_group`) -- apart
/// so a test can stand in for both.
fn judge_ancestor_with(
    path: &Path,
    seen: std::io::Result<(u32, u32, u32)>,
    uid: u32,
    names: &impl NameService,
    acl: impl FnOnce() -> std::io::Result<bool>,
) -> Result<(), PersistError> {
    let refuse = |detail: String| {
        Err(PersistError::DirectoryNotPrivate {
            path: path.to_path_buf(),
            detail,
        })
    };
    let (owner, gid, mode) = match seen {
        Ok(seen) => seen,
        Err(e) => {
            return refuse(format!(
                "an ancestor that cannot be inspected ({e}) is refused, not judged on its name"
            ));
        }
    };
    let mode = mode & 0o7777;
    if owner != 0 && owner != uid {
        return refuse(format!(
            "an ancestor owned by uid {owner}, mode {mode:04o}: owned by neither root nor uid {uid}"
        ));
    }
    if mode & 0o1000 == 0 {
        if mode & 0o002 != 0 {
            return refuse(format!(
                "an ancestor owned by uid {owner}, mode {mode:04o}: other-writable and not sticky"
            ));
        }
        if mode & 0o020 != 0
            && let Err(detail) = owners_private_group(names, uid, gid, acl())
        {
            return refuse(format!(
                "an ancestor owned by uid {owner}, mode {mode:04o}: {detail}"
            ));
        }
    }
    Ok(())
}

/// What the name service answers, as [`owners_private_group`] reads it:
/// a user's name, and a group's name with its listed members. `Ok(None)`
/// is "no such entry".
pub(crate) trait NameService {
    /// The name of the account `uid`.
    fn user_name(&self, uid: u32) -> std::io::Result<Option<String>>;
    /// The name and listed members of the group `gid`.
    fn group(&self, gid: u32) -> std::io::Result<Option<(String, Vec<String>)>>;
}

/// The host's name service: `getpwuid_r` and `getgrgid_r`, so NSS
/// sources answer as well as `/etc/passwd` and `/etc/group`. Off Linux
/// nothing is read, and the predicate refuses as unreadable.
pub(crate) struct HostNames;

impl NameService for HostNames {
    fn user_name(&self, uid: u32) -> std::io::Result<Option<String>> {
        #[cfg(target_os = "linux")]
        {
            nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
                .map(|user| user.map(|user| user.name))
                .map_err(std::io::Error::from)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = uid;
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }

    fn group(&self, gid: u32) -> std::io::Result<Option<(String, Vec<String>)>> {
        #[cfg(target_os = "linux")]
        {
            nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(gid))
                .map(|group| group.map(|group| (group.name, group.mem)))
                .map_err(std::io::Error::from)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = gid;
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }
}

/// ADR-0028 A 2026-10-08, "a group of one is the owner's own" (#231): a
/// group-write bit is accepted when the group is the PRIVATE group of
/// `euid`, the account this process runs as -- never the directory's
/// owner, or a root-owned directory in group `root` would pass while
/// accounts with primary gid 0 exist -- that is, its name is that
/// account's user name and it lists no member, as the name service
/// answers both; and when the directory or file carries no POSIX access
/// ACL (`acl`), since with one its group bits are the ACL's mask, not
/// the group's grant. The user-private group scheme gives such accounts
/// umask `002`, so their own directories and files are `0775` and
/// `0664`. The name, not the gid, is compared: a scheme where every
/// account's primary group is `users` would pass a gid comparison. A read
/// that fails or finds no entry refuses.
///
/// What it does NOT see, named in the amendment as root's acts the rule
/// accepts: another account given this group as its PRIMARY group (never
/// listed in the member list, and no name service enumerates passwd
/// reliably), and a name service that answers falsely.
///
/// Applied by the directory walk and by `config.yaml`'s own clause
/// (`load.rs`), so the two cannot diverge.
///
/// # Errors
/// The refusal's detail, naming the group, its gid or the ACL.
pub(crate) fn owners_private_group(
    names: &impl NameService,
    euid: u32,
    gid: u32,
    acl: std::io::Result<bool>,
) -> Result<(), String> {
    match acl {
        Ok(false) => {}
        Ok(true) => {
            return Err(format!(
                "group-writable, and it carries an access ACL ({ACCESS_ACL}): its group bits are the ACL's mask"
            ));
        }
        Err(e) => {
            return Err(format!(
                "group-writable; whether it carries an access ACL could not be read ({e})"
            ));
        }
    }
    // The amendment's wording, "the owner" meaning this process's
    // account, with which read failed and why after it.
    let unread = |why: String| {
        format!(
            "group-writable; whether group {gid} is the owner's private group could not be read: {why}"
        )
    };
    let user = match names.user_name(euid) {
        Ok(Some(user)) => user,
        Ok(None) => return Err(unread(format!("uid {euid} has no account entry"))),
        Err(e) => return Err(unread(format!("the account of uid {euid}: {e}"))),
    };
    let (group, members) = match names.group(gid) {
        Ok(Some(group)) => group,
        Ok(None) => return Err(unread(format!("group {gid} has no entry"))),
        Err(e) => return Err(unread(format!("group {gid}: {e}"))),
    };
    if group == user && members.is_empty() {
        return Ok(());
    }
    Err(format!(
        "group-writable by group {group} (gid {gid}), not the owner's private group (the owner being this process's account, {user}, uid {euid})"
    ))
}

/// The extended attribute holding a POSIX access ACL. A default ACL
/// (`system.posix_acl_default`) is not one: a child it gives an access
/// ACL is refused at its own turn.
const ACCESS_ACL: &str = "system.posix_acl_access";

/// Whether `path` itself -- not a link's target -- carries an access ACL.
/// A filesystem without extended attributes carries none.
pub(crate) fn access_acl_at(path: &Path) -> std::io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        acl_answer(rustix::fs::lgetxattr(path, ACCESS_ACL, &mut [0u8; 0][..]))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// Whether the opened `file` carries an access ACL, asked of the handle
/// so the file judged is the file read.
pub(crate) fn access_acl_of(file: &fs::File) -> std::io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        acl_answer(rustix::fs::fgetxattr(file, ACCESS_ACL, &mut [0u8; 0][..]))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// A size query's answer read as presence: a size is an ACL, no such
/// attribute or no attribute support is none, anything else is an error.
#[cfg(target_os = "linux")]
fn acl_answer(answer: rustix::io::Result<usize>) -> std::io::Result<bool> {
    match answer {
        Ok(_) => Ok(true),
        Err(e) if e == rustix::io::Errno::NODATA || e == rustix::io::Errno::NOTSUP => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// The rule for a symbolic link on the path to a private directory: owned
/// by root or `uid`. (Its holding directory is judged as an ancestor.)
fn judge_link(path: &Path, owner: u32, mode: u32, uid: u32) -> Result<(), PersistError> {
    if owner != 0 && owner != uid {
        return Err(PersistError::DirectoryNotPrivate {
            path: path.to_path_buf(),
            detail: format!(
                "a symbolic link owned by uid {owner}, mode {:04o}: owned by neither root nor uid {uid}",
                mode & 0o7777
            ),
        });
    }
    Ok(())
}

/// This process's effective uid, from `/proc/self/status`: `geteuid`
/// would be the crate's one unsafe call.
///
/// # Errors
/// [`PersistError::UnsupportedPlatform`] where it cannot be read.
pub fn effective_uid() -> Result<u32, PersistError> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|_| PersistError::UnsupportedPlatform)?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().nth(1))
        .and_then(|id| id.parse().ok())
        .ok_or(PersistError::UnsupportedPlatform)
}

/// Refuse a directory owned by somebody else.
///
/// # Why the comparison is against a file we just made
///
/// The obvious spelling is `geteuid()`, an unsafe call, and this crate
/// is `forbid(unsafe_code)` -- so the effective uid is not reachable
/// through a call (it is read from `/proc` on Linux by
/// [`effective_uid`], where no file of ours exists yet). It
/// does not need to be here: `ours` was
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
        // Group and other hold no permission bit: read as the mask it is
        // rather than as `trailing_zeros() >= 6`.
        #[expect(clippy::verbose_bit_mask, reason = "a permission mask reads as one")]
        let private = mode & 0o077 == 0;
        Ok(private)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(PersistError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    /// A temporary directory made `0700` at creation, whatever the umask: the
    /// ancestor rule judges it, and `tempfile::tempdir()` under umask `002`
    /// with a shared primary group is `0775`, refused (j37).
    fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
        use std::os::unix::fs::PermissionsExt as _;
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
    }

    use super::*;

    /// The uid read from /proc is the one this process creates files as.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_effective_uid_is_the_owner_of_what_this_process_creates() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = private_tempdir().expect("tempdir");
        let file = dir.path().join("mine");
        std::fs::write(&file, b"").expect("write");
        assert_eq!(
            effective_uid().expect("readable"),
            std::fs::metadata(&file).expect("meta").uid()
        );
    }

    /// The identity loader's directory check: this process's own `0700`
    /// directory passes (the control), and the same directory widened or
    /// reached through a link is refused. Another owner is staged by
    /// asking the same check for a uid that is not this process's.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_owned_private_dir_passes_and_a_wide_or_linked_one_does_not() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = private_tempdir().expect("tempdir");
        let dir = root.path().join("keys");
        create_private_dir(&dir).expect("create");
        require_owned_private_dir(&dir).expect("our own 0700 directory passes");

        let link = root.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).expect("link");
        assert!(matches!(
            require_owned_private_dir(&link),
            Err(PersistError::DirectoryNotPrivate { .. })
        ));

        // The DIRECTORY'S OWN owner, asked of a directory straight under
        // `/tmp`: every ancestor there is root's, so another uid is
        // refused at the directory itself and not at an ancestor of ours
        // (#224 review A F1: under our own tempdir the ancestor walk
        // answered first and the owner check went untested).
        let alone = owned_private_dir_under_tmp();
        let uid = effective_uid().expect("readable");
        require_owned_private_dir_as(alone.path(), uid).expect("the control: ours");
        match require_owned_private_dir_as(alone.path(), uid.wrapping_add(1)) {
            Err(PersistError::DirectoryNotPrivate { path, detail }) => {
                assert_eq!(path, alone.path(), "refused at the directory itself");
                assert!(detail.starts_with("owned by uid"), "{detail}");
            }
            other => panic!("refused as another's: {other:?}"),
        }

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).expect("chmod");
        assert!(matches!(
            require_owned_private_dir(&dir),
            Err(PersistError::DirectoryNotPrivate { .. })
        ));
    }

    /// An owner-only file of ours is not a private directory, refused
    /// naming it as one; the owner-only directory beside it is the
    /// control.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_file_is_not_a_private_directory() {
        let root = private_tempdir().expect("tempdir");
        chmod(root.path(), 0o700);
        let file = root.path().join("file");
        fs::write(&file, b"").expect("write");
        chmod(&file, 0o600);
        let detail = refused_at(resolve_owned_private_dir(&file), &file);
        assert_eq!(detail, "it is not a directory");
        let dir = root.path().join("dir");
        create_private_dir(&dir).expect("made");
        resolve_owned_private_dir(&dir).expect("the control");
    }

    /// An owner-only directory of ours directly under `/tmp`, whose every
    /// ancestor is root's: `/tmp` is asserted root's and sticky, the
    /// precondition that lets another uid be refused at the directory
    /// alone.
    #[cfg(target_os = "linux")]
    fn owned_private_dir_under_tmp() -> tempfile::TempDir {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let tmp = fs::symlink_metadata("/tmp").expect("/tmp");
        assert!(
            tmp.uid() == 0 && tmp.permissions().mode() & 0o1000 != 0,
            "the precondition: /tmp is root's and sticky"
        );
        let dir = tempfile::Builder::new()
            .tempdir_in("/tmp")
            .expect("a directory under /tmp");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod");
        dir
    }

    /// `mode` on `dir`, whatever the umask gave it.
    #[cfg(unix)]
    fn chmod(dir: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(dir, fs::Permissions::from_mode(mode)).expect("chmod");
    }

    /// `root/<names...>/p`: each named directory at `mode`, `p` owner-only
    /// -- a private directory under ancestors the test chooses.
    #[cfg(unix)]
    fn private_under(root: &Path, names: &[&str], mode: u32) -> PathBuf {
        let mut dir = root.to_path_buf();
        for name in names {
            dir = dir.join(name);
            fs::create_dir(&dir).expect("mkdir");
            chmod(&dir, mode);
        }
        let private = dir.join("p");
        fs::create_dir(&private).expect("mkdir");
        chmod(&private, 0o700);
        private
    }

    /// The detail of a refusal, which must name `path`.
    #[cfg(unix)]
    fn refused_at(result: Result<PathBuf, PersistError>, path: &Path) -> String {
        match result {
            Err(PersistError::DirectoryNotPrivate { path: at, detail }) => {
                assert_eq!(
                    at, path,
                    "the refusal names the failing component: {detail}"
                );
                detail
            }
            other => panic!("refused at {}: {other:?}", path.display()),
        }
    }

    /// A name service a test stages: users and groups by id, or a read
    /// that fails.
    #[cfg(unix)]
    struct FakeNames {
        users: Vec<(u32, &'static str)>,
        groups: Vec<(u32, &'static str, Vec<&'static str>)>,
        fails: bool,
    }

    #[cfg(unix)]
    impl NameService for FakeNames {
        fn user_name(&self, uid: u32) -> std::io::Result<Option<String>> {
            if self.fails {
                return Err(std::io::ErrorKind::Other.into());
            }
            Ok(self
                .users
                .iter()
                .find(|(id, _)| *id == uid)
                .map(|(_, name)| (*name).to_owned()))
        }
        fn group(&self, gid: u32) -> std::io::Result<Option<(String, Vec<String>)>> {
            if self.fails {
                return Err(std::io::ErrorKind::Other.into());
            }
            Ok(self
                .groups
                .iter()
                .find(|(id, _, _)| *id == gid)
                .map(|(_, name, members)| {
                    (
                        (*name).to_owned(),
                        members.iter().map(|m| (*m).to_owned()).collect(),
                    )
                }))
        }
    }

    /// ADR-0028 A 2026-10-08, "a group of one is the owner's own" (#231),
    /// on staged entries: a group-writable ancestor passes when its
    /// group's name is that of the account this process runs as, it lists
    /// no member, and the directory carries no access ACL. A shared group,
    /// a gid equal to the uid under another name, a group with a member, a
    /// root-owned directory in group `root` (named after its OWNER, not
    /// ours), an access ACL, a missing entry and a failed read -- of the
    /// names or of the ACL -- are each refused; other-write is refused
    /// whatever the group; sticky is unchanged.
    #[cfg(unix)]
    #[test]
    fn a_group_writable_ancestor_needs_the_owners_private_group() {
        let path = Path::new("/home/alice");
        let names = FakeNames {
            users: vec![(1000, "alice"), (0, "root")],
            groups: vec![
                (0, "root", vec![]),
                (1001, "alice", vec![]),
                (100, "users", vec![]),
                (1000, "staff", vec![]),
                (1002, "alice", vec!["bob"]),
            ],
            fails: false,
        };
        let judge = |gid: u32, mode: u32, names: &FakeNames| {
            judge_ancestor_with(path, Ok((1000, gid, mode)), 1000, names, || Ok(false))
        };
        judge(1001, 0o40775, &names).expect("the owner's private group, gid apart from the uid");
        let refused = |result: Result<(), PersistError>| match result {
            Err(PersistError::DirectoryNotPrivate { path: at, detail }) => {
                assert_eq!(at, path);
                detail
            }
            other => panic!("refused: {other:?}"),
        };
        let shared = refused(judge(100, 0o40775, &names));
        assert!(shared.contains("group users (gid 100)"), "{shared}");
        let same_id = refused(judge(1000, 0o40775, &names));
        assert!(same_id.contains("group staff (gid 1000)"), "{same_id}");
        let member = refused(judge(1002, 0o40775, &names));
        assert!(member.contains("group alice (gid 1002)"), "{member}");
        let roots = refused(judge_ancestor_with(
            path,
            Ok((0, 0, 0o40775)),
            1000,
            &names,
            || Ok(false),
        ));
        assert!(roots.contains("group root (gid 0)"), "{roots}");
        assert!(
            roots.contains("alice, uid 1000"),
            "names our account: {roots}"
        );
        let acl = refused(judge_ancestor_with(
            path,
            Ok((1000, 1001, 0o40775)),
            1000,
            &names,
            || Ok(true),
        ));
        assert!(acl.contains("access ACL"), "{acl}");
        let acl_unread = refused(judge_ancestor_with(
            path,
            Ok((1000, 1001, 0o40775)),
            1000,
            &names,
            || Err(std::io::ErrorKind::PermissionDenied.into()),
        ));
        assert!(
            acl_unread.contains("access ACL could not be read"),
            "{acl_unread}"
        );
        let unknown = refused(judge(4242, 0o40775, &names));
        assert!(unknown.contains("whether group 4242 is"), "{unknown}");
        assert!(unknown.contains("group 4242 has no entry"), "{unknown}");
        let no_account = refused(judge(
            1001,
            0o40775,
            &FakeNames {
                users: vec![],
                ..names_clone(&names)
            },
        ));
        assert!(
            no_account.contains("uid 1000 has no account entry"),
            "the user read is named, not the group: {no_account}"
        );
        let failed = refused(judge(
            1001,
            0o40775,
            &FakeNames {
                fails: true,
                ..names_clone(&names)
            },
        ));
        assert!(
            failed.contains("the account of uid 1000: other error"),
            "the io error is kept: {failed}"
        );
        // The ACL is asked of a group-writable directory only: a 0755
        // one passes without the read, whether it would answer or fail.
        judge_ancestor_with(path, Ok((1000, 100, 0o40755)), 1000, &names, || {
            panic!("the ACL is not asked of a directory without group write")
        })
        .expect("0755, no ACL read");
        judge_ancestor_with(path, Ok((1000, 100, 0o40755)), 1000, &names, || {
            Err(std::io::ErrorKind::PermissionDenied.into())
        })
        .expect("0755, an unreadable ACL never consulted");
        for detail in [
            refused(judge(4242, 0o40775, &names)),
            refused(judge(
                1001,
                0o40775,
                &FakeNames {
                    users: vec![],
                    ..names_clone(&names)
                },
            )),
            refused(judge(
                1001,
                0o40775,
                &FakeNames {
                    fails: true,
                    ..names_clone(&names)
                },
            )),
        ] {
            assert!(detail.contains("could not be read"), "{detail}");
        }
        let other = refused(judge(1001, 0o40757, &names));
        assert!(other.contains("other-writable"), "{other}");
        judge(100, 0o41775, &names).expect("sticky, as before");
    }

    /// The access-ACL readers on a real directory and file: none until
    /// `setfacl` grants another account write, then one -- by path, not
    /// following a link, and by handle. A default ACL alone, on a
    /// directory, is not an access ACL.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_access_acl_readers_see_an_acl_setfacl_adds() {
        let root = private_tempdir().expect("tempdir");
        let dir = root.path().join("d");
        fs::create_dir(&dir).expect("mkdir");
        let file = root.path().join("f");
        fs::write(&file, b"").expect("write");
        let setfacl = |args: &[&str], at: &Path| {
            let ran = std::process::Command::new("setfacl")
                .args(args)
                .arg(at)
                .status()
                .expect("setfacl runs");
            assert!(ran.success(), "setfacl {args:?}");
        };
        assert!(!access_acl_at(&dir).expect("read"), "the control: no ACL");
        setfacl(&["-d", "-m", "u:nobody:rwx"], &dir);
        assert!(!access_acl_at(&dir).expect("read"), "a default ACL only");
        setfacl(&["-m", "u:nobody:rwx"], &dir);
        assert!(access_acl_at(&dir).expect("read"), "an access ACL");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).expect("link");
        assert!(
            !access_acl_at(&link).expect("read"),
            "the link, not its target"
        );

        let opened = || fs::File::open(&file).expect("open");
        assert!(
            !access_acl_of(&opened()).expect("read"),
            "the control: no ACL"
        );
        setfacl(&["-m", "u:nobody:rw"], &file);
        assert!(access_acl_of(&opened()).expect("read"), "an access ACL");
    }

    #[cfg(unix)]
    fn names_clone(names: &FakeNames) -> FakeNames {
        FakeNames {
            users: names.users.clone(),
            groups: names.groups.clone(),
            fails: names.fails,
        }
    }

    /// The host's name service reads the entries `id` reports: this
    /// process's user name and its primary group's name, and no entry
    /// for a gid nothing allocates. Then a `0775` directory in our
    /// primary group is judged as `getent` says that group is: accepted
    /// when its name is ours and it lists no member, refused naming it
    /// otherwise -- an oracle apart from the code under test.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_host_name_service_answers_as_id_and_getent_do() {
        use std::os::unix::fs::MetadataExt as _;
        let id = |flag: &str| {
            let out = std::process::Command::new("id")
                .arg(flag)
                .output()
                .expect("id");
            String::from_utf8(out.stdout)
                .expect("utf-8")
                .trim()
                .to_owned()
        };
        let uid = effective_uid().expect("readable");
        assert_eq!(
            HostNames.user_name(uid).expect("read"),
            Some(id("-un")),
            "the user"
        );
        let root = private_tempdir().expect("tempdir");
        chmod(root.path(), 0o700);
        let dir = root.path().join("shared");
        fs::create_dir(&dir).expect("mkdir");
        let gid = fs::metadata(&dir).expect("meta").gid();
        let (group, members) = HostNames.group(gid).expect("read").expect("an entry");
        assert_eq!(group, id("-gn"), "the primary group");
        assert!(
            HostNames.group(4_000_000_000).expect("read").is_none(),
            "no entry"
        );

        let getent = std::process::Command::new("getent")
            .args(["group", &gid.to_string()])
            .output()
            .expect("getent");
        let line = String::from_utf8(getent.stdout).expect("utf-8");
        let listed = line
            .trim()
            .rsplit(':')
            .next()
            .unwrap_or_default()
            .to_owned();
        assert_eq!(listed.is_empty(), members.is_empty(), "members: {line}");
        let private = group == id("-un") && listed.is_empty();
        chmod(&dir, 0o775);
        match resolve_guarded_dir(&dir) {
            Ok(_) => assert!(private, "accepted, so {group} is our private group"),
            Err(PersistError::DirectoryNotPrivate { detail, .. }) => {
                assert!(!private, "refused, so {group} is not: {detail}");
                assert!(detail.contains(&format!("group {group}")), "{detail}");
            }
            Err(other) => panic!("judged: {other:?}"),
        }
    }

    /// An other-writable ancestor without the sticky bit is refused; with
    /// it, ours, accepted -- a `/tmp`-shaped directory.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_other_writable_ancestor_needs_the_sticky_bit() {
        let root = private_tempdir().expect("tempdir");
        chmod(root.path(), 0o700);
        let private = private_under(root.path(), &["a"], 0o757);
        let a = root.path().join("a");
        refused_at(resolve_private_dir(&private), &a);
        chmod(&a, 0o1777);
        resolve_private_dir(&private).expect("sticky and ours");
    }

    /// An ancestor another account owns is refused at `0755` and when
    /// sticky: its owner can rename its entries whatever the mode. Staged
    /// by asking for a uid that is not this process's; this process's
    /// own uid on the same tree is the control.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_ancestor_another_uid_owns_is_refused_sticky_or_not() {
        let root = private_tempdir().expect("tempdir");
        chmod(root.path(), 0o700);
        let private = private_under(root.path(), &["a"], 0o755);
        let a = root.path().join("a");
        let uid = effective_uid().expect("uid");
        resolve_private_dir_as(&private, uid).expect("the control: ours");
        let other = uid.wrapping_add(1);
        let detail = refused_at(resolve_private_dir_as(&private, other), &a);
        assert!(detail.contains("neither root nor uid"), "{detail}");
        chmod(&a, 0o1777);
        refused_at(resolve_private_dir_as(&private, other), &a);
    }

    /// A link on the path, ours, in a passing directory and leading to a
    /// passing directory, is followed -- the answer is where it leads;
    /// held in an other-writable directory, or owned by another uid, it
    /// is refused and named.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_link_on_the_path_is_judged_and_followed() {
        let root = private_tempdir().expect("tempdir");
        chmod(root.path(), 0o700);
        let target = private_under(root.path(), &["a"], 0o755);
        std::os::unix::fs::symlink(root.path().join("a"), root.path().join("via")).expect("link");
        let through = root.path().join("via").join("p");
        assert_eq!(
            resolve_private_dir(&through).expect("a sound link is followed"),
            fs::canonicalize(&target).expect("canonical")
        );

        let uid = effective_uid().expect("uid");
        let detail = refused_at(
            resolve_private_dir_as(&through, uid.wrapping_add(1)),
            &root.path().join("via"),
        );
        assert!(detail.contains("symbolic link"), "{detail}");

        let open = root.path().join("open");
        fs::create_dir(&open).expect("mkdir");
        chmod(&open, 0o777);
        std::os::unix::fs::symlink(root.path().join("a"), open.join("via")).expect("link");
        refused_at(resolve_private_dir(&open.join("via").join("p")), &open);
    }

    /// The rule itself on what a tree cannot stage: a root-owned `0755`
    /// ancestor passes, one that cannot be inspected is refused rather
    /// than judged on its name, and a link owned by root passes.
    #[test]
    fn the_ancestor_rule_on_root_and_the_uninspectable() {
        let path = Path::new("/srv");
        judge_ancestor(path, Ok((0, 0, 0o40755)), 1000).expect("root's, 0755");
        judge_ancestor(path, Ok((1000, 1000, 0o41777)), 1000).expect("ours, sticky");
        match judge_ancestor(
            path,
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            1000,
        ) {
            Err(PersistError::DirectoryNotPrivate { detail, .. }) => {
                assert!(detail.contains("cannot be inspected"), "{detail}");
            }
            other => panic!("refused: {other:?}"),
        }
        judge_link(path, 0, 0o120_777, 1000).expect("root's link");
        assert!(judge_link(path, 1001, 0o120_777, 1000).is_err());
    }

    /// The plain per-user layout -- the state root and every directory to
    /// the profile ours at `0755`, the profile owner-only -- passes: the
    /// control for every refusal above.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_plain_xdg_layout_passes() {
        let home = private_tempdir().expect("tempdir");
        chmod(home.path(), 0o700);
        let private = private_under(
            home.path(),
            &[".local", "state", "interweave", "profiles"],
            0o755,
        );
        resolve_owned_private_dir(&private).expect("the plain layout");
    }

    /// The configuration directory is judged for who can change it, not
    /// who can read it: ours at `0755` passes, other-writable or under an
    /// other-writable ancestor it is refused. (Group-write is the
    /// private-group predicate's, tested on staged entries.)
    #[cfg(target_os = "linux")]
    #[test]
    fn a_guarded_directory_is_judged_for_writers_not_readers() {
        let root = private_tempdir().expect("tempdir");
        chmod(root.path(), 0o700);
        let config = root.path().join("a").join("config");
        fs::create_dir_all(&config).expect("mkdir");
        chmod(&root.path().join("a"), 0o755);
        chmod(&config, 0o755);
        resolve_guarded_dir(&config).expect("readable by all, written by us");
        chmod(&config, 0o757);
        refused_at(resolve_guarded_dir(&config), &config);
        chmod(&config, 0o755);
        chmod(&root.path().join("a"), 0o757);
        refused_at(resolve_guarded_dir(&config), &root.path().join("a"));
    }

    /// A file is not a guarded directory, refused naming it; the
    /// directory beside it is the control. (`create_private_dir` under a
    /// file never reaches this: its walk meets `ENOTDIR` first.)
    #[cfg(target_os = "linux")]
    #[test]
    fn a_guarded_directory_that_is_a_file_is_refused() {
        let root = private_tempdir().expect("tempdir");
        chmod(root.path(), 0o700);
        let file = root.path().join("file");
        fs::write(&file, b"").expect("write");
        // Its mode set, so it is refused for its type and not for a
        // group-write bit the umask gave it.
        chmod(&file, 0o600);
        let detail = refused_at(resolve_guarded_dir(&file), &file);
        assert_eq!(detail, "it is not a directory");
        resolve_guarded_dir(root.path()).expect("the control");
    }

    /// The entries of `dir`, by name.
    #[cfg(unix)]
    fn entries(dir: &Path) -> Vec<std::ffi::OsString> {
        fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name())
            .collect()
    }

    /// A missing directory under an ancestor others can write is refused
    /// naming that ancestor, and nothing is created under it; the same
    /// tree with the ancestor `0755`, and `1777` (sticky), are the
    /// controls, each made owner-only all the way down.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_refused_ancestor_gets_nothing_created_under_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = private_tempdir().expect("tempdir");
        chmod(root.path(), 0o700);
        let wide = root.path().join("wide");
        fs::create_dir(&wide).expect("mkdir");
        chmod(&wide, 0o777);
        let dir = wide.join("a").join("b").join("c");
        let detail = refused_at(create_private_dir(&dir).map(|()| dir.clone()), &wide);
        assert!(detail.contains("0777"), "{detail}");
        assert!(
            entries(&wide).is_empty(),
            "nothing created: {:?}",
            entries(&wide)
        );

        for mode in [0o755, 0o1777] {
            chmod(&wide, mode);
            create_private_dir(&dir).expect("the control");
            for made in [wide.join("a"), wide.join("a").join("b"), dir.clone()] {
                let mode = fs::symlink_metadata(&made)
                    .expect("made")
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, OWNER_ONLY_DIR, "{}", made.display());
            }
            fs::remove_dir_all(wide.join("a")).expect("clean");
        }
    }

    /// A component already there when its turn comes -- made in the
    /// window after the base was judged -- is adopted when it is our
    /// owner-only directory, and refused naming it, with nothing made
    /// inside, when it is wider.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_component_that_appears_unprivate_is_refused_before_anything_is_made_in_it() {
        let root = private_tempdir().expect("tempdir");
        chmod(root.path(), 0o700);
        let uid = effective_uid().expect("readable");
        let appeared = root.path().join("a");
        fs::create_dir(&appeared).expect("mkdir");
        chmod(&appeared, 0o755);
        refused_at(
            create_each_as(root.path(), Path::new("a/b"), uid).map(|()| appeared.clone()),
            &appeared,
        );
        assert!(entries(&appeared).is_empty(), "nothing made inside it");

        chmod(&appeared, 0o700);
        create_each_as(root.path(), Path::new("a/b"), uid).expect("adopted: ours, owner-only");
        assert!(appeared.join("b").is_dir());
    }

    /// The ownership half of adoption: an owner-only directory that is
    /// not `uid`'s is refused naming it, with nothing made inside --
    /// staged as ours directly under root's sticky `/tmp` and asked for
    /// another uid, since under a tempdir of ours that uid is refused at
    /// an ancestor first. The same call for our own uid is the control.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_component_another_uid_owns_is_refused_before_anything_is_made_in_it() {
        let appeared = owned_private_dir_under_tmp();
        let name = appeared.path().file_name().expect("a name");
        let missing = Path::new(name).join("b");
        let uid = effective_uid().expect("readable");
        let detail = refused_at(
            create_each_as(Path::new("/tmp"), &missing, uid.wrapping_add(1))
                .map(|()| appeared.path().to_path_buf()),
            appeared.path(),
        );
        assert!(detail.starts_with("owned by uid"), "{detail}");
        assert!(
            entries(appeared.path()).is_empty(),
            "nothing made inside it"
        );

        create_each_as(Path::new("/tmp"), &missing, uid).expect("the control: ours");
        assert!(appeared.path().join("b").is_dir());
    }

    /// An existing `dir` that is a file, a FIFO or a dangling link is
    /// `AlreadyExists`, as the recursive create answered; a link to a
    /// directory is accepted, as it was (the control).
    #[cfg(target_os = "linux")]
    #[test]
    fn an_existing_path_that_is_not_a_directory_is_refused() {
        let root = private_tempdir().expect("tempdir");
        chmod(root.path(), 0o700);
        let file = root.path().join("file");
        fs::write(&file, b"").expect("write");
        let fifo = root.path().join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(made.success(), "mkfifo");
        let dangling = root.path().join("dangling");
        std::os::unix::fs::symlink(root.path().join("nowhere"), &dangling).expect("link");
        for path in [&file, &fifo, &dangling] {
            assert!(
                matches!(
                    create_private_dir(path),
                    Err(PersistError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists
                ),
                "{} is not a directory",
                path.display()
            );
        }
        let real = root.path().join("real");
        create_private_dir(&real).expect("made");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        create_private_dir(&link).expect("the control: a link to a directory");
    }

    /// A missing part that climbs with `..` is refused rather than
    /// followed out of the judged base; the same path without it is the
    /// control.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_missing_path_that_climbs_is_refused() {
        let root = private_tempdir().expect("tempdir");
        chmod(root.path(), 0o700);
        let climbs = root.path().join("a").join("..").join("b");
        assert!(matches!(
            create_private_dir(&climbs),
            Err(PersistError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidInput
        ));
        assert!(entries(root.path()).is_empty(), "nothing created");
        create_private_dir(&root.path().join("b")).expect("the control");
    }

    /// A directory whose fsync cannot succeed, without a race.
    ///
    /// `0o300` is write plus execute and NO READ: `rename` into it still
    /// works, and `File::open` on the directory itself fails with
    /// EACCES — precisely the failure the write used to discard. Returns
    /// false when the mode blocked nothing (running as root, or a
    /// filesystem that ignores it), so the test says so rather than
    /// passing vacuously.
    #[cfg(unix)]
    fn make_unreadable(dir: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o300)).expect("chmod");
        fs::File::open(dir).is_err()
    }

    #[cfg(unix)]
    #[test]
    fn a_write_whose_directory_cannot_be_synced_says_so() {
        // The rename has landed and the NAME may not survive a crash.
        // `let _ = dir.sync_all()` reported that as a successful write —
        // and this function writes a profile's configuration and its
        // identity key, so the name failing to survive a reboot is the
        // loss of the identity itself.
        use std::os::unix::fs::PermissionsExt;
        let dir = private_tempdir().expect("tempdir");
        let path = dir.path().join("profile.json");

        // POSITIVE CONTROL FIRST: the same write succeeds while the
        // directory is readable.
        write_atomic(&path, b"{}").expect("an ordinary write succeeds");

        if !make_unreadable(dir.path()) {
            println!("skipped: 0o300 did not block opening the directory here");
            return;
        }
        let refused = write_atomic(&path, b"{\"v\":2}");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod back");
        assert!(
            matches!(refused, Err(PersistError::Unsynced(_))),
            "a write that could not fsync its directory must not report success, and says \
             the new file is in place: {refused:?}"
        );
        // And the rename really did land: the failure is about
        // durability of the NAME, not about the write not happening.
        assert_eq!(
            fs::read(&path).expect("the new contents are in place"),
            b"{\"v\":2}",
            "the error says the name may not be durable, not that nothing was written"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_exclusive_create_whose_directory_cannot_be_synced_says_so() {
        // The THIRD door, which the two above do not reach: this one
        // publishes by hard link rather than rename, and `0o300` passes
        // `require_private_dir` — it checks that group and other have no
        // bits, not that the owner can read.
        use std::os::unix::fs::PermissionsExt;
        let dir = private_tempdir().expect("tempdir");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod");

        // POSITIVE CONTROL: a readable directory takes the key.
        create_private_exclusive(&dir.path().join("first.key"), b"k").expect("an ordinary create");

        if !make_unreadable(dir.path()) {
            println!("skipped: 0o300 did not block opening the directory here");
            return;
        }
        let path = dir.path().join("identity.key");
        let refused = create_private_exclusive(&path, b"k");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod back");
        assert!(
            matches!(refused, Err(PersistError::Unsynced(_))),
            "an exclusive create that could not fsync its directory must not \
             report success, and says the file is in place: {refused:?}"
        );
        assert!(
            path.exists(),
            "the link was published; what is in doubt is whether its name survives"
        );
    }

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
