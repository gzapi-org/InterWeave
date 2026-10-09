// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The trust overlay: what `admin.trust.set` changed, kept in the state
//! directory (ADR-0028 A 2026-10-07).
//!
//! `<state>/trust-overlay.json` holds exactly two lists against
//! `config.yaml`'s `trust.allowed_peers`: `added`, the peers allowed
//! beyond it, and `revoked`, the configured peers revoked. The effective
//! allowlist is (configured ∪ added) ∖ revoked.
//!
//! The overlay is AUTHORISATION, not configuration and not a cache: a
//! missing file is an empty overlay, but a present one that cannot be
//! trusted -- unreadable, unparseable, someone else's, writable by
//! anyone else, a peer in both lists, an effective set past the bound --
//! stops the daemon. Skipping it would silently re-allow every peer the
//! operator revoked.
//!
//! `config.yaml` is never written; the overlay is a delta against the
//! configuration the daemon started with, NORMALISED at load so an
//! operator who edits `config.yaml` between restarts gets the union they
//! expect and no stale entry undoes that edit.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use interweave_transport_api::TransportIdentity;
use interweave_trust_api::PeerTrustPolicy;
use serde::{Deserialize, Serialize};

use crate::{PersistError, ProfilePaths, TrustBoundary, persist};

/// The overlay's file name in the profile's state directory.
pub const TRUST_OVERLAY_FILE: &str = "trust-overlay.json";

/// The most a trust overlay may hold on disk. Two lists of at most
/// [`PeerTrustPolicy::MAX_ALLOWED_PEERS`] peer ids each fit well inside;
/// anything larger is refused before it is parsed.
pub const MAX_TRUST_OVERLAY_BYTES: u64 = 1024 * 1024;

/// Where a peer on the effective allowlist comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustSource {
    /// `config.yaml`'s `trust.allowed_peers`, not revoked.
    Configured,
    /// Added by `admin.trust.set`, kept in the overlay.
    Administered,
}

/// The two lists, normalised or not; [`TrustOverlay::load`] hands out
/// only a normalised one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustOverlay {
    added: BTreeSet<TransportIdentity>,
    revoked: BTreeSet<TransportIdentity>,
}

/// Why a trust overlay stops the daemon, or why a set could not be kept.
#[derive(Debug)]
pub enum OverlayError {
    /// The file is present and could not be read.
    Read(std::io::Error),
    /// The file is not a regular file owned by this uid and readable by
    /// nobody else, or is a symbolic link.
    NotPrivate {
        /// What is wrong with it.
        detail: String,
    },
    /// The file is larger than [`MAX_TRUST_OVERLAY_BYTES`].
    TooLarge,
    /// The file is not the overlay's shape.
    Parse(String),
    /// A list holds more than [`PeerTrustPolicy::MAX_ALLOWED_PEERS`].
    ListTooLong,
    /// A peer is in both `added` and `revoked`.
    InBothLists {
        /// The peer named twice.
        peer: TransportIdentity,
    },
    /// (configured ∪ added) ∖ revoked is past the allowlist's bound.
    EffectiveTooLarge {
        /// How many peers it holds.
        got: usize,
    },
    /// The overlay could not be written: at load, the normalisation
    /// rewrite; at a set, the set itself.
    Write(PersistError),
}

impl core::fmt::Display for OverlayError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "the trust overlay cannot be read: {e}"),
            Self::NotPrivate { detail } => write!(f, "the trust overlay is not private: {detail}"),
            Self::TooLarge => write!(
                f,
                "the trust overlay is larger than {MAX_TRUST_OVERLAY_BYTES} bytes"
            ),
            Self::Parse(e) => write!(f, "the trust overlay does not parse: {e}"),
            Self::ListTooLong => write!(
                f,
                "a trust overlay list holds more than {} peers",
                PeerTrustPolicy::MAX_ALLOWED_PEERS
            ),
            Self::InBothLists { peer } => write!(
                f,
                "the trust overlay names {} as both added and revoked",
                peer.as_str()
            ),
            Self::EffectiveTooLarge { got } => write!(
                f,
                "the configured and administered allowlist holds {got} peers, more than {}",
                PeerTrustPolicy::MAX_ALLOWED_PEERS
            ),
            Self::Write(e) => write!(f, "the trust overlay cannot be written: {e}"),
        }
    }
}

impl core::error::Error for OverlayError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Read(e) => Some(e),
            Self::Write(e) => Some(e),
            _ => None,
        }
    }
}

impl OverlayError {
    /// Whether the failed write had already put the new overlay in place:
    /// the rename landed and syncing the directory failed. A caller that
    /// promises "a failed write changes nothing" puts the previous
    /// overlay back after this one.
    #[must_use]
    pub const fn installed(&self) -> bool {
        matches!(self, Self::Write(PersistError::Unsynced(_)))
    }
}

impl TrustOverlay {
    /// The overlay's path for `paths`' profile.
    #[must_use]
    pub fn path_for(paths: &ProfilePaths) -> PathBuf {
        paths.state_dir().join(TRUST_OVERLAY_FILE)
    }

    /// The peers allowed beyond the configuration.
    #[must_use]
    pub fn added(&self) -> impl ExactSizeIterator<Item = &TransportIdentity> {
        self.added.iter()
    }

    /// The configured peers revoked.
    #[must_use]
    pub fn revoked(&self) -> impl ExactSizeIterator<Item = &TransportIdentity> {
        self.revoked.iter()
    }

    /// Read the overlay at `path`, normalise it against `configured`,
    /// rewrite it when normalising changed it, and return it with the
    /// effective allowlist. An absent file is the empty overlay and is
    /// not created.
    ///
    /// # Errors
    /// Every [`OverlayError`] but none of them recoverable: each is a
    /// reason not to start (ADR-0028 A 2026-10-07).
    pub fn load(
        path: &Path,
        configured: &BTreeSet<TransportIdentity>,
    ) -> Result<(Self, BTreeSet<TransportIdentity>), OverlayError> {
        Self::load_within(path, configured, &TrustBoundary::root())
    }

    /// [`load`](Self::load), the state directory's walk stopping at
    /// `boundary` -- the embedded runtime's, whose overlay ADR-0028 (A
    /// 2026-10-07) keeps under the boundary its platform supplies.
    ///
    /// # Errors
    /// As [`load`](Self::load).
    pub fn load_within(
        path: &Path,
        configured: &BTreeSet<TransportIdentity>,
        boundary: &TrustBoundary,
    ) -> Result<(Self, BTreeSet<TransportIdentity>), OverlayError> {
        let Some(mut overlay) = Self::read(path, boundary)? else {
            return Ok((Self::default(), configured.clone()));
        };
        let changed = overlay.normalise(configured);
        let effective = overlay.effective(configured)?;
        if changed {
            overlay.write_within(path, boundary)?;
        }
        Ok((overlay, effective))
    }

    /// The overlay as it is on disk, unnormalised: `None` when absent.
    fn read(path: &Path, boundary: &TrustBoundary) -> Result<Option<Self>, OverlayError> {
        // Read under the state directory AS RESOLVED (ADR-0028 A
        // 2026-10-08), as the overlay's write is: `ProfileLock` judged the
        // same directory before load, and this resolves it once more so
        // the read opens where that judgement led. An absent directory
        // holds no overlay.
        let dir = match persist::resolve_private_dir_within(persist::parent_dir(path), boundary) {
            Ok(dir) => dir,
            Err(PersistError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(PersistError::Io(e)) => return Err(OverlayError::Read(e)),
            Err(e) => {
                return Err(OverlayError::NotPrivate {
                    detail: e.to_string(),
                });
            }
        };
        let resolved = dir.join(persist::file_name(path).map_err(|_| {
            OverlayError::Read(std::io::Error::from(std::io::ErrorKind::InvalidInput))
        })?);
        let file = match open_private(&resolved) {
            Ok(file) => file,
            Err(OverlayError::Read(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let mut text = String::new();
        file.take(MAX_TRUST_OVERLAY_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(OverlayError::Read)?;
        if text.len() as u64 > MAX_TRUST_OVERLAY_BYTES {
            return Err(OverlayError::TooLarge);
        }
        let overlay: Self =
            serde_json::from_str(&text).map_err(|e| OverlayError::Parse(e.to_string()))?;
        if overlay.added.len() > PeerTrustPolicy::MAX_ALLOWED_PEERS
            || overlay.revoked.len() > PeerTrustPolicy::MAX_ALLOWED_PEERS
        {
            return Err(OverlayError::ListTooLong);
        }
        if let Some(peer) = overlay.added.intersection(&overlay.revoked).next() {
            return Err(OverlayError::InBothLists { peer: peer.clone() });
        }
        Ok(Some(overlay))
    }

    /// Drop what the configuration already says: an added peer the
    /// configuration lists, a revoked peer it does not. Whether that
    /// changed anything.
    fn normalise(&mut self, configured: &BTreeSet<TransportIdentity>) -> bool {
        let before = (self.added.len(), self.revoked.len());
        self.added.retain(|peer| !configured.contains(peer));
        self.revoked.retain(|peer| configured.contains(peer));
        before != (self.added.len(), self.revoked.len())
    }

    /// (configured ∪ added) ∖ revoked, within the allowlist's bound.
    ///
    /// # Errors
    /// [`OverlayError::EffectiveTooLarge`] past
    /// [`PeerTrustPolicy::MAX_ALLOWED_PEERS`]: refused, never truncated.
    pub fn effective(
        &self,
        configured: &BTreeSet<TransportIdentity>,
    ) -> Result<BTreeSet<TransportIdentity>, OverlayError> {
        let effective: BTreeSet<TransportIdentity> = configured
            .union(&self.added)
            .filter(|peer| !self.revoked.contains(*peer))
            .cloned()
            .collect();
        if effective.len() > PeerTrustPolicy::MAX_ALLOWED_PEERS {
            return Err(OverlayError::EffectiveTooLarge {
                got: effective.len(),
            });
        }
        Ok(effective)
    }

    /// The overlay after `admin.trust.set(peer, allowed)`, one of the four
    /// moves on the normalised lists, or `None` when the set changes
    /// nothing:
    ///
    /// - allow a configured peer: remove it from `revoked`;
    /// - allow any other peer: add it to `added`;
    /// - revoke a configured peer: add it to `revoked`;
    /// - revoke any other peer: remove it from `added`.
    ///
    /// This only moves the lists: the caller checks the moved overlay's
    /// bound ([`TrustOverlay::effective`]) beside the policy's before
    /// writing it, since an overlay left ahead can hold a peer the
    /// policy does not.
    #[must_use]
    pub fn set(
        &self,
        configured: &BTreeSet<TransportIdentity>,
        peer: &TransportIdentity,
        allowed: bool,
    ) -> Option<Self> {
        let mut next = self.clone();
        let changed = match (allowed, configured.contains(peer)) {
            (true, true) => next.revoked.remove(peer),
            (true, false) => next.added.insert(peer.clone()),
            (false, true) => next.revoked.insert(peer.clone()),
            (false, false) => next.added.remove(peer),
        };
        changed.then_some(next)
    }

    /// Where `peer` on the effective allowlist comes from, or `None` when
    /// it is not on it.
    #[must_use]
    pub fn source(
        &self,
        configured: &BTreeSet<TransportIdentity>,
        peer: &TransportIdentity,
    ) -> Option<TrustSource> {
        if configured.contains(peer) {
            (!self.revoked.contains(peer)).then_some(TrustSource::Configured)
        } else {
            self.added
                .contains(peer)
                .then_some(TrustSource::Administered)
        }
    }

    /// Write the overlay whole: a temporary file in the same directory,
    /// owner-only from creation, renamed over `path`.
    ///
    /// # Errors
    /// [`OverlayError::Write`]. A failure before the rename leaves the
    /// previous file as it was; one after it --
    /// [`OverlayError::installed`] -- leaves THIS overlay in place, its
    /// name perhaps not durable.
    pub fn write(&self, path: &Path) -> Result<(), OverlayError> {
        self.write_within(path, &TrustBoundary::root())
    }

    /// [`write`](Self::write), the state directory's walk stopping at
    /// `boundary`.
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn write_within(&self, path: &Path, boundary: &TrustBoundary) -> Result<(), OverlayError> {
        let text = serde_json::to_vec_pretty(self)
            .map_err(|e| OverlayError::Write(PersistError::Io(std::io::Error::other(e))))?;
        persist::write_private_atomic_within(path, &text, boundary).map_err(OverlayError::Write)
    }
}

/// The uid an overlay must be owned by, or -- this process's uid
/// unreadable -- a refusal on READ: the owner cannot be checked, so the
/// file cannot be trusted; never a write failure (#215 review P3).
/// `an_unreadable_uid_refuses_the_overlay_on_read`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn owner_uid(read: Result<u32, PersistError>) -> Result<u32, OverlayError> {
    read.map_err(|_| OverlayError::NotPrivate {
        detail: "its owner cannot be checked: this process's uid is unreadable".to_owned(),
    })
}

/// Open `path` for reading only if it is a regular file, not a link,
/// owned by this process's uid and readable or writable by nobody else
/// -- the identity key's rule. Judged on the OPENED file, so the file
/// checked is the file read.
///
/// Its DIRECTORY is judged and resolved by the caller (`read`), which
/// hands this the path under the directory as resolved.
fn open_private(path: &Path) -> Result<std::fs::File, OverlayError> {
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
                    OverlayError::NotPrivate {
                        detail: "it is a symbolic link".to_owned(),
                    }
                } else {
                    OverlayError::Read(e)
                }
            })?;
        let meta = file.metadata().map_err(OverlayError::Read)?;
        let uid = owner_uid(persist::effective_uid())?;
        if !meta.file_type().is_file() {
            return Err(OverlayError::NotPrivate {
                detail: "it is not a regular file".to_owned(),
            });
        }
        if meta.uid() != uid {
            return Err(OverlayError::NotPrivate {
                detail: format!("owned by uid {}, not {uid}", meta.uid()),
            });
        }
        let mode = meta.mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(OverlayError::NotPrivate {
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(OverlayError::Read(e)),
            _ => Err(OverlayError::NotPrivate {
                detail: "owner-only permissions cannot be checked on this platform".to_owned(),
            }),
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod tests {
    use super::{OverlayError, PersistError, owner_uid};

    #[test]
    fn an_unreadable_uid_refuses_the_overlay_on_read() {
        assert!(matches!(
            owner_uid(Err(PersistError::UnsupportedPlatform)),
            Err(OverlayError::NotPrivate { .. })
        ));
        // The control: a uid read is the owner checked against.
        assert_eq!(owner_uid(Ok(1000)).ok(), Some(1000));
    }
}
