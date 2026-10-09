// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! THE production YAML loader (plan §16 (13), precondition P5): the one
//! path by which a profile document becomes a `ProfileConfig`.
//!
//! [`ProfileConfig::parse_yaml`] turns text into the model -- every
//! section the schema declares, unknown keys refused at every level --
//! and judges nothing else. [`ProfileConfig::load`] is what a daemon or
//! the admin tool calls: it reads the profile's `config.yaml` under a
//! size ceiling, parses it, refuses a document whose `profile.name` is
//! not the profile it is being loaded as, refuses one that fails
//! `validate()`, and refuses one whose `identity.key_file` resolves inside
//! the human client's directory.

use std::io::Read as _;

use crate::paths::CONFIG_FILE;
use crate::{ConfigError, ProfileConfig, ProfilePaths};

/// The largest profile document read, in bytes.
///
/// A profile is a few kilobytes; the largest one the schema allows --
/// 4096 allowed peers, 64 endpoints with their subsets, sixteen
/// discovery providers -- is a few hundred. A ceiling keeps a mistaken
/// path (a log, a device) from being read whole before the parser can
/// refuse it.
pub const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

/// Why a profile document could not be loaded.
#[derive(Debug)]
pub enum LoadError {
    /// The document could not be read.
    Read(std::io::Error),
    /// The document is larger than [`MAX_PROFILE_BYTES`].
    TooLarge {
        /// The ceiling it exceeded.
        limit: u64,
    },
    /// The document is not a profile: malformed YAML, a missing required
    /// section, an unknown key, a value of the wrong type or a literal
    /// spelled otherwise.
    Parse(String),
    /// `profile.name` is absent or names another profile.
    NameMismatch {
        /// The profile being loaded.
        expected: String,
        /// What the document says, if anything.
        found: Option<String>,
    },
    /// The document parses and breaks the schema's rules.
    Invalid(Vec<ConfigError>),
    /// `identity.key_file` resolves inside the human client's directory
    /// ([`ProfilePaths::human_dir`]): the transport key is never kept among
    /// the client's files, which a backup or export of the client's store
    /// reads (ADR-0040: the UI never receives key bytes).
    KeyFileInHumanDir {
        /// The key file as configured, joined to the profile's
        /// configuration directory.
        path: std::path::PathBuf,
        /// Where that path leads on disk, its existing prefix resolved
        /// with every link followed: what lies inside the human directory
        /// when `path`'s text does not.
        on_disk: std::path::PathBuf,
    },
    /// The key file's or the human client's directory's place on disk
    /// could not be resolved to judge [`LoadError::KeyFileInHumanDir`]:
    /// an existing component could not be inspected for a reason other
    /// than its absence. Refused rather than judged on the path's text,
    /// which is what a link defeats.
    KeyFileUnresolved {
        /// The path that could not be resolved.
        path: std::path::PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// The configuration directory, an ancestor of it or a link on its
    /// path can be changed by an account other than root and this one
    /// (ADR-0028 A 2026-10-08): `config.yaml` names the key path and the
    /// allowlist, so an account that could replace it would choose the
    /// key's directory and admit its own peer. Refused before the file is
    /// read, as a parse failure is fatal.
    ConfigDirUnguarded(crate::PersistError),
    /// `config.yaml` itself is a symbolic link, not a regular file, or
    /// can be written by an account other than root and this one -- owned
    /// by another, other-writable, or group-writable by a group that is
    /// not the owner's private group -- the owner being the account this
    /// process runs as, never the file's -- or under an access ACL
    /// (ADR-0028 A 2026-10-08).
    /// Readable by others is allowed: it is not secret.
    ConfigFileUnguarded {
        /// The file.
        path: std::path::PathBuf,
        /// Which rule it broke, with its owner uid and mode.
        detail: String,
    },
}

/// Open `config.yaml` without following a final link and accept the
/// opened handle only when no account but root and this one can write it
/// (ADR-0028 A 2026-10-08): its directory's judgement is worth nothing if
/// the file in it is someone else's to rewrite. Judged on the HANDLE, so
/// the file judged is the file read.
fn open_guarded(path: &std::path::Path) -> Result<std::fs::File, LoadError> {
    open_guarded_as(
        path,
        crate::effective_uid().map_err(LoadError::ConfigDirUnguarded),
        &crate::persist::HostNames,
    )
}

/// [`open_guarded`] with the owner compared against `uid` -- this
/// process's effective uid, or why it cannot be read -- apart so a test
/// can name another uid: a file another account owns needs that account
/// (`a_document_another_uid_owns_is_refused`) -- and with `names` standing
/// for the name service the group-write clause reads
/// (`a_group_writable_document_needs_the_owners_private_group`).
fn open_guarded_as(
    path: &std::path::Path,
    uid: Result<u32, LoadError>,
    names: &impl crate::persist::NameService,
) -> Result<std::fs::File, LoadError> {
    let refuse = |detail: String| LoadError::ConfigFileUnguarded {
        path: path.to_path_buf(),
        detail,
    };
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        // O_NONBLOCK so a FIFO in its place opens at once and is refused
        // below as not a regular file, rather than holding start until a
        // writer appears; it changes nothing for a regular file
        // (`a_document_that_is_a_fifo_is_refused_without_waiting`).
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                return Err(refuse("it is a symbolic link".to_owned()));
            }
            Err(e) => return Err(LoadError::Read(e)),
        };
        let uid = uid?;
        let meta = file.metadata().map_err(LoadError::Read)?;
        let (owner, gid, mode) = (meta.uid(), meta.gid(), meta.mode() & 0o7777);
        if !meta.is_file() {
            return Err(refuse(format!(
                "owned by uid {owner}, mode {mode:04o}: not a regular file"
            )));
        }
        judge_document((owner, gid, mode), uid, names, || {
            crate::persist::access_acl_of(&file)
        })
        .map_err(refuse)?;
        Ok(file)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (refuse, uid, names);
        Err(LoadError::ConfigDirUnguarded(
            crate::PersistError::UnsupportedPlatform,
        ))
    }
}

/// The writer rules for `config.yaml`, on what its opened handle said:
/// `seen` is `(owner, gid, mode)`, `uid` this process's, `acl` whether
/// the file carries an access ACL (asked only of a group-writable one).
/// Apart from the open so a test can stage an owner it cannot create a
/// file as (`a_group_writable_document_is_judged_for_this_process`).
///
/// # Errors
/// The refusal's detail.
fn judge_document(
    (owner, gid, mode): (u32, u32, u32),
    uid: u32,
    names: &impl crate::persist::NameService,
    acl: impl FnOnce() -> std::io::Result<bool>,
) -> Result<(), String> {
    if owner != 0 && owner != uid {
        return Err(format!(
            "owned by uid {owner}, mode {mode:04o}: owned by neither root nor uid {uid}"
        ));
    }
    if mode & 0o002 != 0 {
        return Err(format!(
            "owned by uid {owner}, mode {mode:04o}: other-writable"
        ));
    }
    // The directory walk's predicate, not a copy of it, asked for this
    // process's account and never the file's owner.
    if mode & 0o020 != 0
        && let Err(detail) = crate::persist::owners_private_group(names, uid, gid, acl())
    {
        return Err(format!("owned by uid {owner}, mode {mode:04o}: {detail}"));
    }
    Ok(())
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "the profile document cannot be read: {e}"),
            Self::TooLarge { limit } => {
                write!(f, "the profile document is larger than {limit} bytes")
            }
            Self::Parse(e) => write!(f, "the profile document is not a valid profile: {e}"),
            Self::NameMismatch { expected, found } => match found {
                Some(found) => write!(
                    f,
                    "profile.name is {found:?} but the profile loaded is {expected:?}"
                ),
                None => write!(
                    f,
                    "profile.name is missing; the profile loaded is {expected:?}"
                ),
            },
            Self::KeyFileInHumanDir { path, on_disk } if path == on_disk => write!(
                f,
                "identity.key_file {} lies inside the human client's directory",
                path.display()
            ),
            Self::KeyFileInHumanDir { path, on_disk } => write!(
                f,
                "identity.key_file {} lies inside the human client's directory on disk, at {}",
                path.display(),
                on_disk.display()
            ),
            Self::KeyFileUnresolved { path, source } => write!(
                f,
                "{} cannot be resolved to judge identity.key_file's place: {source}",
                path.display()
            ),
            Self::ConfigFileUnguarded { path, detail } => write!(
                f,
                "{} can be changed by another account: {detail}",
                path.display()
            ),
            Self::ConfigDirUnguarded(crate::PersistError::UnsupportedPlatform) => write!(
                f,
                "the profile's configuration directory cannot be judged on this platform"
            ),
            Self::ConfigDirUnguarded(e) => write!(
                f,
                "the profile's configuration directory can be changed by another account: {e}"
            ),
            Self::Invalid(errors) => {
                write!(f, "the profile breaks {} rule(s):", errors.len())?;
                for e in errors {
                    write!(f, " {e};")?;
                }
                Ok(())
            }
        }
    }
}

impl core::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Read(e) | Self::KeyFileUnresolved { source: e, .. } => Some(e),
            Self::ConfigDirUnguarded(e) => Some(e),
            _ => None,
        }
    }
}

impl ProfileConfig {
    /// Parse a profile document. Nothing is validated beyond the shape.
    ///
    /// # Errors
    /// [`LoadError::Parse`] for anything that is not a profile.
    pub fn parse_yaml(text: &str) -> Result<Self, LoadError> {
        serde_norway::from_str(text).map_err(|e| LoadError::Parse(e.to_string()))
    }

    /// Load `paths`' profile document: read it (at most
    /// [`MAX_PROFILE_BYTES`]), parse it, require `profile.name` to be the
    /// profile `paths` resolves, and validate it.
    ///
    /// # Errors
    /// [`LoadError`], naming which step refused: read, size, parse, name,
    /// validation, or a key file inside the human client's directory.
    pub fn load(paths: &ProfilePaths) -> Result<Self, LoadError> {
        // The directory judged for who can change it, and the file read
        // under it as resolved (ADR-0028 A 2026-10-08): an absent one is
        // still a read failure.
        let dir = match crate::resolve_guarded_dir(paths.config_dir()) {
            Ok(dir) => dir,
            Err(crate::PersistError::Io(e)) => return Err(LoadError::Read(e)),
            Err(e) => return Err(LoadError::ConfigDirUnguarded(e)),
        };
        let file = open_guarded(&dir.join(CONFIG_FILE))?;
        let mut text = String::new();
        file.take(MAX_PROFILE_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(LoadError::Read)?;
        if text.len() as u64 > MAX_PROFILE_BYTES {
            return Err(LoadError::TooLarge {
                limit: MAX_PROFILE_BYTES,
            });
        }
        let profile = Self::parse_yaml(&text)?;
        if profile.profile.as_ref().map(|p| p.name.as_str()) != Some(paths.profile()) {
            return Err(LoadError::NameMismatch {
                expected: paths.profile().to_owned(),
                found: profile.profile.map(|p| p.name),
            });
        }
        let errors = profile.validate();
        if !errors.is_empty() {
            return Err(LoadError::Invalid(errors));
        }
        // Judged ON DISK, not on the text: each side's existing prefix is
        // resolved (links followed, `..` taken by the kernel), so a key
        // configured at `elsewhere/link/keys/k` with `link` pointing into
        // the human directory is refused -- the lexical comparison this
        // replaced passed it (the external review of 2026-10-04, P2-1).
        // What does not exist yet is joined on the text. A link made
        // after this check, by the same account, is not judged.
        let key = profile.identity.key_file_in(paths);
        let unresolved =
            |path: std::path::PathBuf| move |source| LoadError::KeyFileUnresolved { path, source };
        let human = paths.human_dir();
        let resolved_human = resolve_existing_prefix(&human).map_err(unresolved(human.clone()))?;
        let resolved_key = resolve_existing_prefix(&key).map_err(unresolved(key.clone()))?;
        if resolved_key.starts_with(&resolved_human) {
            return Err(LoadError::KeyFileInHumanDir {
                path: key,
                on_disk: resolved_key,
            });
        }
        Ok(profile)
    }
}

/// `path` with its longest existing prefix resolved by the filesystem
/// (`canonicalize`: every link followed, `.` and `..` taken where they
/// stand) and the rest, which does not exist yet, joined on its text --
/// a `..` there removing the component before it, as creating the
/// directories would.
///
/// # Errors
/// An existing component that cannot be inspected for a reason other
/// than its absence -- a permission refusal, or a file where a directory
/// is named -- and a link whose target does not exist.
fn resolve_existing_prefix(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::path::Component;
    let mut missing = Vec::new();
    let mut prefix = path;
    let resolved = loop {
        match std::fs::canonicalize(prefix) {
            Ok(resolved) => break resolved,
            // A DANGLING LINK is not a missing component: its text is not
            // where it leads, and what it names can appear later.
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(prefix).is_ok() =>
            {
                return Err(e);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match (prefix.parent(), prefix.components().next_back()) {
                    (Some(parent), Some(last)) if !parent.as_os_str().is_empty() => {
                        missing.push(last);
                        prefix = parent;
                    }
                    // A relative path with nothing of it on disk: there is
                    // nothing to resolve.
                    _ => return Ok(path.to_path_buf()),
                }
            }
            Err(e) => return Err(e),
        }
    };
    let mut out = resolved;
    for component in missing.into_iter().rev() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    Ok(out)
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

    use super::{LoadError, open_guarded_as};

    /// `config.yaml`'s owner clause (ADR-0028 A 2026-10-08): a file of
    /// ours at 0644, readable by all and written by no one else, is
    /// accepted as ours -- the control -- and refused asked as another
    /// uid, the owner named. In a sticky configuration directory this
    /// clause is the one that refuses a file another account placed.
    #[cfg(target_os = "linux")]
    #[test]
    #[allow(clippy::expect_used, clippy::panic)]
    fn a_document_another_uid_owns_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = private_tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, b"schema_version: 2\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let uid = crate::effective_uid().expect("the uid");
        let names = &crate::persist::HostNames;
        open_guarded_as(&path, Ok(uid), names).expect("the control: ours");
        // Off root only: a root-owned document is accepted by design, so
        // as root this would fail for a reason other than the one named.
        assert_ne!(
            uid, 0,
            "run as root, the document made here is root's, which the rule accepts"
        );
        match open_guarded_as(&path, Ok(uid.wrapping_add(1)), names) {
            Err(LoadError::ConfigFileUnguarded { path: at, detail }) => {
                assert_eq!(at, path);
                assert!(detail.contains("owned by neither root nor uid"), "{detail}");
            }
            other => panic!("refused as another's: {:?}", other.err()),
        }
    }

    /// The predicate on `config.yaml` asks for THIS PROCESS's private
    /// group, never the file owner's: a root-owned `0664` document in
    /// group `root` is refused for uid 1000, and the same document in
    /// alice's private group passes for her. A document without group
    /// write never asks its ACL.
    #[test]
    #[allow(clippy::expect_used, clippy::panic)]
    fn a_group_writable_document_is_judged_for_this_process() {
        #[derive(Clone)]
        struct Names;
        impl crate::persist::NameService for Names {
            // A flag of this call's own: the test makes one read at a time
            // and must not share the process's with a test beside it.
            fn outstanding(&self) -> &'static std::sync::atomic::AtomicBool {
                Box::leak(Box::new(std::sync::atomic::AtomicBool::new(false)))
            }
            fn user_name(&self, uid: u32) -> std::io::Result<Option<String>> {
                Ok(match uid {
                    0 => Some("root".to_owned()),
                    1000 => Some("alice".to_owned()),
                    _ => None,
                })
            }
            fn group(&self, gid: u32) -> std::io::Result<Option<(String, Vec<String>)>> {
                Ok(match gid {
                    0 => Some(("root".to_owned(), Vec::new())),
                    1001 => Some(("alice".to_owned(), Vec::new())),
                    _ => None,
                })
            }
        }
        let detail = super::judge_document((0, 0, 0o664), 1000, &Names, || Ok(false))
            .expect_err("root's group is not ours");
        assert!(detail.contains("group root (gid 0)"), "{detail}");
        super::judge_document((0, 1001, 0o664), 1000, &Names, || Ok(false))
            .expect("our private group, root owning the file");
        super::judge_document((1000, 0, 0o644), 1000, &Names, || {
            panic!("the ACL is not asked of a document without group write")
        })
        .expect("0644");
    }

    /// The private-group predicate on `config.yaml`'s own group-write
    /// bit, with the name service staged: a `0664` document passes when
    /// its group is the owner's private group, and is refused naming the
    /// group when that group is shared, or as unreadable when the entry
    /// is missing, or when it carries an access ACL. Other-write is
    /// refused whatever the group.
    #[cfg(target_os = "linux")]
    #[test]
    #[allow(clippy::expect_used, clippy::panic)]
    fn a_group_writable_document_needs_the_owners_private_group() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        #[derive(Clone)]
        struct Names(Option<&'static str>);
        impl crate::persist::NameService for Names {
            // A flag of this call's own: the test makes one read at a time
            // and must not share the process's with a test beside it.
            fn outstanding(&self) -> &'static std::sync::atomic::AtomicBool {
                Box::leak(Box::new(std::sync::atomic::AtomicBool::new(false)))
            }
            fn user_name(&self, _: u32) -> std::io::Result<Option<String>> {
                Ok(Some("alice".to_owned()))
            }
            fn group(&self, _: u32) -> std::io::Result<Option<(String, Vec<String>)>> {
                Ok(self.0.map(|name| (name.to_owned(), Vec::new())))
            }
        }
        let dir = private_tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, b"schema_version: 2\n").expect("write");
        let gid = std::fs::metadata(&path).expect("meta").gid();
        let chmod = |m: u32| {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(m)).expect("chmod");
        };
        let uid = crate::effective_uid().expect("the uid");
        chmod(0o664);
        open_guarded_as(&path, Ok(uid), &Names(Some("alice"))).expect("a group of one");
        let refused = |names: Names| match open_guarded_as(&path, Ok(uid), &names) {
            Err(LoadError::ConfigFileUnguarded { detail, .. }) => detail,
            other => panic!("refused: {:?}", other.err()),
        };
        let shared = refused(Names(Some("users")));
        assert!(
            shared.contains(&format!("group users (gid {gid})")),
            "{shared}"
        );
        let missing = refused(Names(None));
        assert!(missing.contains("could not be read"), "{missing}");
        // An access ACL granting another account write makes the group
        // bits its mask: refused, the private group notwithstanding.
        let ran = std::process::Command::new("setfacl")
            .args(["-m", "u:nobody:rw"])
            .arg(&path)
            .status()
            .expect("setfacl runs");
        assert!(ran.success(), "setfacl");
        chmod(0o664);
        let acl = refused(Names(Some("alice")));
        assert!(acl.contains("access ACL"), "{acl}");
        chmod(0o666);
        let other = refused(Names(Some("alice")));
        assert!(other.contains("other-writable"), "{other}");
    }
}
