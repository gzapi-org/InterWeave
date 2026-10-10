// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Reading a private state file: the one reader the trust overlay and
//! the availability overlay share, so the rules a file is judged by --
//! its directory private under the boundary, the file regular, not a
//! link, this uid's, readable by nobody else, bounded in size -- are
//! written once.

use std::io::Read as _;
use std::path::Path;

use crate::{PersistError, TrustBoundary, persist};

/// Why a private file cannot be read; each caller maps it into its own
/// error, which names the file.
#[derive(Debug)]
pub(crate) enum PrivateReadError {
    /// The file is present and could not be read.
    Read(std::io::Error),
    /// The file, or its directory, is not private.
    NotPrivate {
        /// What is wrong with it.
        detail: String,
    },
    /// The file is larger than the caller's bound.
    TooLarge,
}

/// The text of the private file at `path`, its directory resolved under
/// `boundary`: `None` when the file or its directory is absent.
pub(crate) fn read_private_within(
    path: &Path,
    boundary: &TrustBoundary,
    max_bytes: u64,
) -> Result<Option<String>, PrivateReadError> {
    // Read under the state directory AS RESOLVED (ADR-0028 A
    // 2026-10-08), as the overlays' writes are: `ProfileLock` judged the
    // same directory before load, and this resolves it once more so
    // the read opens where that judgement led. An absent directory
    // holds no file.
    let dir = match persist::resolve_private_dir_within(persist::parent_dir(path), boundary) {
        Ok(dir) => dir,
        Err(PersistError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(PersistError::Io(e)) => return Err(PrivateReadError::Read(e)),
        Err(e) => {
            return Err(PrivateReadError::NotPrivate {
                detail: e.to_string(),
            });
        }
    };
    let resolved = dir.join(persist::file_name(path).map_err(|_| {
        PrivateReadError::Read(std::io::Error::from(std::io::ErrorKind::InvalidInput))
    })?);
    let file = match open_private(&resolved) {
        Ok(file) => file,
        Err(PrivateReadError::Read(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let mut text = String::new();
    file.take(max_bytes + 1)
        .read_to_string(&mut text)
        .map_err(PrivateReadError::Read)?;
    if text.len() as u64 > max_bytes {
        return Err(PrivateReadError::TooLarge);
    }
    Ok(Some(text))
}

/// `text` parsed as `T` from a JSON OBJECT and nothing else. serde's
/// derived struct visitor also accepts a sequence -- the fields in
/// order -- so `from_str` alone would read `[[],[]]` as a trust overlay
/// and `["stay-reachable"]` as the availability choice: a second shape
/// neither file is written in. Pinned by the array cases of
/// `a_present_overlay_that_cannot_be_trusted_stops_the_load` and
/// `a_file_naming_anything_but_the_choice_is_refused`.
pub(crate) fn object_only<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("not a JSON object".to_owned());
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}

/// The uid a private file must be owned by, or -- this process's uid
/// unreadable -- a refusal on READ: the owner cannot be checked, so the
/// file cannot be trusted; never a write failure (#215 review P3).
/// `an_unreadable_uid_refuses_the_file_on_read`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn owner_uid(read: Result<u32, PersistError>) -> Result<u32, PrivateReadError> {
    read.map_err(|_| PrivateReadError::NotPrivate {
        detail: "its owner cannot be checked: this process's uid is unreadable".to_owned(),
    })
}

/// Open `path` for reading only if it is a regular file, not a link,
/// owned by this process's uid and readable or writable by nobody else
/// -- the identity key's rule. Judged on the OPENED file, so the file
/// checked is the file read.
///
/// Its DIRECTORY is judged and resolved by the caller
/// ([`read_private_within`]), which
/// hands this the path under the directory as resolved.
fn open_private(path: &Path) -> Result<std::fs::File, PrivateReadError> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        // O_NONBLOCK so a FIFO in its place opens at once and is refused
        // below as not a regular file, rather than holding start until a
        // writer appears; it changes nothing for a regular file
        // (`an_overlay_that_is_a_fifo_is_refused_without_waiting`).
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| {
                // ELOOP is O_NOFOLLOW meeting a link.
                if e.raw_os_error() == Some(libc::ELOOP) {
                    PrivateReadError::NotPrivate {
                        detail: "it is a symbolic link".to_owned(),
                    }
                } else {
                    PrivateReadError::Read(e)
                }
            })?;
        let meta = file.metadata().map_err(PrivateReadError::Read)?;
        let uid = owner_uid(persist::effective_uid())?;
        if !meta.file_type().is_file() {
            return Err(PrivateReadError::NotPrivate {
                detail: "it is not a regular file".to_owned(),
            });
        }
        if meta.uid() != uid {
            return Err(PrivateReadError::NotPrivate {
                detail: format!("owned by uid {}, not {uid}", meta.uid()),
            });
        }
        let mode = meta.mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(PrivateReadError::NotPrivate {
                detail: format!("mode is {mode:04o}, wider than 0600"),
            });
        }
        Ok(file)
    }
    // Elsewhere ownership and mode cannot be checked here, so a present
    // overlay is refused -- but an absent one is still the empty overlay,
    // not a reason not to start.
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(PrivateReadError::Read(e)),
            _ => Err(PrivateReadError::NotPrivate {
                detail: "owner-only permissions cannot be checked on this platform".to_owned(),
            }),
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod tests {
    use super::{PersistError, PrivateReadError, owner_uid};

    #[test]
    fn an_unreadable_uid_refuses_the_file_on_read() {
        assert!(matches!(
            owner_uid(Err(PersistError::UnsupportedPlatform)),
            Err(PrivateReadError::NotPrivate { .. })
        ));
        // The control: a uid read is the owner checked against.
        assert_eq!(owner_uid(Ok(1000)).ok(), Some(1000));
    }
}
