// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The profile's transport identity.
//!
//! One profile owns one persistent Ed25519 key and the PeerId derived
//! from it. This crate generates it, stores it owner-only, loads it back,
//! and implements the ADR-0033 recovery phrase.
//!
//! # The libp2p boundary stops here
//!
//! This is the lowest crate permitted to know about libp2p, and its own
//! surface speaks [`TransportIdentity`] — the neutral type — so a
//! `libp2p_identity::PeerId` never travels upward. `crates/api/*` must
//! not depend on libp2p at all (CLAUDE.md §4), and the way to keep that
//! true is for the translation to happen once, here.
//!
//! # What SPIKE-006 constrains
//!
//! The seed is exported through `AsRef<[u8]>`, because
//! `ed25519::SecretKey::to_bytes` is `pub(crate)` in the pinned version.
//! The 64-byte `Keypair::to_bytes()` form is never used even though its
//! first half is genuinely the seed: a 64-byte intermediate is one
//! refactor away from being mnemonic-encoded whole, which the recovery
//! contract forbids. See `spikes/spike-006/README.md`.
//!
//! # Never silently regenerate
//!
//! An established profile whose key file is missing or unreadable is an
//! error, not an invitation to make a new identity. Regenerating would
//! hand the profile a new PeerId, silently invalidating every trust
//! relationship anyone had with it — and it would look like a successful
//! start.

#![forbid(unsafe_code)]

pub mod record;
pub mod recovery;

use std::path::Path;

use interweave_profile_config::{
    PersistError, create_private_exclusive, require_private_dir, write_private_atomic,
};
use interweave_transport_api::{IdError, TransportIdentity};
use libp2p_identity::{Keypair, PeerId, ed25519};

pub use record::{ALGORITHM, FORMAT, RecoveryRecord};
pub use recovery::{ENTROPY_BYTES, PHRASE_WORDS, RecoveryPhrase};

/// Run `f` holding the exclusive marker for `path`.
///
/// EVERY path that overwrites a stored identity goes through here.
/// Rotation and restore both replace the same file, so exclusion that
/// covered only one of them would be a guarantee against rotations
/// rather than a guarantee about the file — and the caller cannot tell
/// those apart from the outside.
///
/// `save` is deliberately not here: it creates and never replaces, and
/// its own `link` fails while any file exists, so it cannot interleave
/// with a replacement in the first place.
///
/// The marker is released whatever happened. One left behind by a failed
/// operation would block every later one for a reason that has nothing
/// to do with the key.
fn holding_marker<T>(
    path: &Path,
    f: impl FnOnce(&Path) -> Result<T, IdentityError>,
) -> Result<T, IdentityError> {
    let marker = marker_path(path);
    acquire_marker(path, &marker)?;
    let outcome = f(&marker);
    let _ = std::fs::remove_file(&marker);
    outcome
}

/// Take the rotation marker, or say why not.
///
/// # Why `ENOENT` is not "there is no identity"
///
/// `link` resolves its source to an INODE and then completes the link.
/// The winning rotation finishes with a `rename` onto the same path,
/// which drops the previous inode's last link — so a loser whose `link`
/// had already resolved that inode finds it with no links left and gets
/// `ENOENT`, while the name itself never stopped existing.
///
/// Reporting that as [`IdentityError::NotFound`] told a caller its
/// profile had no key at the exact moment another caller was rotating
/// it, which is both false and alarming. The condition is transient by
/// construction — it happens only because a rotation just completed — so
/// the answer is to look again. What the retry then finds is a different
/// identity, and the loser gets the `PeerIdMismatch` it should have had.
///
/// A genuinely absent key is distinguished by asking whether the NAME is
/// there, which is a different question from whether the inode survived.
fn acquire_marker(path: &Path, marker: &Path) -> Result<(), IdentityError> {
    // Bounded: each attempt costs one completed rotation by someone
    // else, so exhausting them means losing repeatedly rather than
    // waiting on anything.
    const ATTEMPTS: usize = 8;

    for _ in 0..ATTEMPTS {
        match std::fs::hard_link(path, marker) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(IdentityError::RotationInProgress {
                    marker: marker.to_path_buf(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !path.exists() {
                    return Err(IdentityError::NotFound);
                }
                // The name is there; the inode we resolved is not. Some
                // other rotation landed between the two.
            }
            Err(e) => return Err(IdentityError::Storage(PersistError::Io(e))),
        }
    }
    Err(IdentityError::RotationInProgress {
        marker: marker.to_path_buf(),
    })
}

/// The exclusive marker a rotation holds, beside the key it replaces.
///
/// Beside it because `link` works within one filesystem, and because a
/// marker anywhere else would not be found by the next rotation of this
/// profile.
fn marker_path(path: &Path) -> std::path::PathBuf {
    let mut marker = path.as_os_str().to_owned();
    marker.push(".rotating");
    std::path::PathBuf::from(marker)
}

/// What a rotation changed.
///
/// Both PeerIds, because a rotation is only meaningful as a pair: the
/// old one is what every existing trust relationship names, and a caller
/// that cannot say what it was cannot tell anyone what stopped working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rotation {
    /// The identity that was stored before.
    pub previous: TransportIdentity,
    /// The identity stored now.
    pub current: TransportIdentity,
}

/// What can go wrong with a profile identity.
#[derive(Debug)]
pub enum IdentityError {
    /// The key file does not exist.
    ///
    /// Deliberately distinct from every other error: an established
    /// profile must never silently regenerate, so the caller has to
    /// decide between "create a new profile" and "restore this one".
    NotFound,
    /// The key path is not a regular file.
    ///
    /// A symlink, directory, or device where the identity key belongs.
    /// Refused rather than followed: the permission check answers about
    /// what a link POINTS AT, so a link to some other account's
    /// mode-0600 file would otherwise pass it.
    NotAFile,
    /// The key file exists but is readable by someone other than its owner.
    ///
    /// Refused rather than repaired. A key that has been world-readable
    /// should be treated as disclosed, and quietly tightening the mode
    /// would hide that it ever was.
    PermissionsTooOpen,
    /// The stored bytes are not a key this build can read.
    Corrupt(String),
    /// The recovery phrase does not encode the expected identity.
    ///
    /// Fails CLOSED. A checksum-valid phrase for the wrong key would
    /// otherwise restore a different PeerId and look like success.
    PeerIdMismatch {
        /// The identity the phrase reconstructs.
        got: String,
        /// The identity the caller said to expect.
        expected: String,
    },
    /// A phrase with the wrong number of words.
    WrongWordCount {
        /// Words supplied.
        got: usize,
        /// Words required.
        want: usize,
    },
    /// A phrase carrying the wrong amount of entropy.
    WrongEntropyLength {
        /// Bytes carried.
        got: usize,
    },
    /// BIP-39 refused the phrase — bad checksum, unknown word.
    Bip39(String),
    /// The identity could not be stored or read.
    Storage(PersistError),
    /// A stored PeerId is not one the neutral contract accepts.
    Id(IdError),
    /// A rotation is already under way, or one was interrupted.
    ///
    /// Rotation acquires an exclusive marker beside the key. Finding it
    /// present means either another process holds it right now, or a
    /// previous rotation died holding it — and those are not
    /// distinguishable from outside, so both are reported rather than
    /// one being assumed. A stale marker is a state a person removes
    /// after looking at the key, which is the right amount of friction
    /// for an operation that invalidates every trust relationship.
    RotationInProgress {
        /// The marker that must be gone before a rotation can proceed.
        marker: std::path::PathBuf,
    },
    /// `save` was called for a path that already holds an identity.
    ///
    /// The mirror of [`Self::NotFound`], and refused for the same
    /// reason. An established profile owns one persistent PeerId, so
    /// overwriting its key silently is not a save — it is a rotation
    /// that invalidates every trust relationship anyone holds, wearing
    /// the name of an ordinary write. Replacement is deliberate and goes
    /// through [`ProfileIdentity::replace_saved`].
    AlreadyExists,
}

impl From<PersistError> for IdentityError {
    fn from(value: PersistError) -> Self {
        Self::Storage(value)
    }
}

impl core::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAFile => write!(f, "the identity key path is not a regular file"),
            Self::NotFound => write!(
                f,
                "no identity key file; an established profile is never regenerated silently"
            ),
            Self::PermissionsTooOpen => write!(
                f,
                "the identity key is readable by more than its owner; treat it as disclosed"
            ),
            Self::Corrupt(d) => write!(f, "the identity key file cannot be read: {d}"),
            Self::PeerIdMismatch { got, expected } => write!(
                f,
                "the recovery phrase reconstructs {got}, not the expected {expected}"
            ),
            Self::WrongWordCount { got, want } => {
                write!(f, "a recovery phrase is {want} words, got {got}")
            }
            Self::WrongEntropyLength { got } => {
                write!(
                    f,
                    "a recovery phrase carries {ENTROPY_BYTES} bytes, got {got}"
                )
            }
            Self::Bip39(d) => write!(f, "the recovery phrase is not valid: {d}"),
            Self::Storage(e) => write!(f, "identity storage: {e}"),
            Self::Id(e) => write!(f, "stored identity: {e}"),
            Self::RotationInProgress { marker } => write!(
                f,
                "a rotation is in progress or was interrupted; {} must be removed first",
                marker.display()
            ),
            Self::AlreadyExists => write!(
                f,
                "an identity key already exists at that path; replacing it rotates the profile's PeerId"
            ),
        }
    }
}

impl core::error::Error for IdentityError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Storage(e) => Some(e),
            Self::Id(e) => Some(e),
            _ => None,
        }
    }
}

/// One profile's persistent Ed25519 transport identity.
///
/// No `Clone`: a key that is easy to copy is a key that ends up in more
/// places than intended, and nothing here needs a second one.
pub struct ProfileIdentity {
    keypair: ed25519::Keypair,
}

/// The ceiling on a key file, in bytes.
///
/// A protobuf-encoded Ed25519 keypair is well under a hundred bytes. The
/// bound exists so an oversized local file cannot be allocated before the
/// decoder rejects it, not because any real key approaches it.
const MAX_KEY_FILE_BYTES: u64 = 4096;

/// A byte buffer cleared when it is dropped, however it is dropped.
///
/// The seed must not outlive the read on the error path either, and a
/// `fill(0)` after the decode only runs when the decode SUCCEEDS.
struct Zeroizing(Vec<u8>);

impl Drop for Zeroizing {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl ProfileIdentity {
    /// Create a new identity from the local CSPRNG.
    ///
    /// For a NEW profile only. An established profile with a missing key
    /// must surface [`IdentityError::NotFound`] and let a human choose
    /// between creating and restoring.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            keypair: ed25519::Keypair::generate(),
        }
    }

    /// Reconstruct an identity from a recovery phrase.
    ///
    /// # Errors
    /// Returns [`IdentityError`] if the phrase does not carry 32 bytes.
    pub fn from_phrase(phrase: &RecoveryPhrase) -> Result<Self, IdentityError> {
        let mut entropy = phrase.expose_entropy()?;
        // `try_from_bytes` ZEROES this buffer on success (SPIKE-006
        // finding 3), which is why it gets its own copy rather than
        // borrowing anything the caller still needs.
        let secret = ed25519::SecretKey::try_from_bytes(&mut entropy)
            .map_err(|e| IdentityError::Corrupt(e.to_string()))?;
        Ok(Self {
            keypair: ed25519::Keypair::from(secret),
        })
    }

    /// The PeerId, as the neutral contract type.
    ///
    /// # Errors
    /// Returns [`IdentityError::Id`] if libp2p produced a PeerId the
    /// neutral grammar rejects, which would mean the two disagree about
    /// what a PeerId is.
    pub fn transport_identity(&self) -> Result<TransportIdentity, IdentityError> {
        let peer = PeerId::from_public_key(&Keypair::from(self.keypair.clone()).public());
        TransportIdentity::parse(peer.to_base58()).map_err(IdentityError::Id)
    }

    /// The recovery phrase for this identity.
    ///
    /// # Errors
    /// Returns [`IdentityError::Bip39`] if the seed cannot be encoded.
    pub fn recovery_phrase(&self) -> Result<RecoveryPhrase, IdentityError> {
        RecoveryPhrase::from_entropy(&self.seed())
    }

    /// The keypair, for the one caller that must drive a Swarm with it.
    ///
    /// Deliberately named for what it is used for. The transport
    /// substrate needs the real key to complete a Noise handshake, so
    /// this cannot be avoided — but it is the only way the key leaves
    /// this crate, and every call site is therefore visible in a grep.
    ///
    /// Returns the general `Keypair` rather than the Ed25519 one because
    /// that is what `SwarmBuilder` takes; the conversion is lossless.
    #[must_use]
    pub fn swarm_keypair(&self) -> Keypair {
        Keypair::from(self.keypair.clone())
    }

    /// The exact 32-byte Ed25519 secret seed.
    ///
    /// Through `AsRef<[u8]>`, because `SecretKey::to_bytes` is
    /// `pub(crate)` in the pinned libp2p version (SPIKE-006 finding 1).
    /// Private to this crate: the seed is the identity, and a public
    /// accessor would be a second way for it to escape.
    fn seed(&self) -> [u8; ENTROPY_BYTES] {
        let secret = self.keypair.secret();
        let bytes: &[u8] = secret.as_ref();
        let mut out = [0u8; ENTROPY_BYTES];
        out.copy_from_slice(&bytes[..ENTROPY_BYTES]);
        out
    }

    /// Check a phrase against an expected identity WITHOUT writing anything.
    ///
    /// The read-only half of recovery: a human verifying that the words
    /// they wrote down really do restore this profile. It touches no
    /// file, so a verification that fails leaves the running identity
    /// exactly as it was.
    ///
    /// Fails closed on mismatch. A checksum-valid phrase for a different
    /// key is still checksum-valid, so the checksum alone would let a
    /// wrong phrase read as verified.
    ///
    /// # Errors
    /// Returns [`IdentityError::PeerIdMismatch`] if the phrase
    /// reconstructs a different identity.
    pub fn verify_phrase(
        phrase: &RecoveryPhrase,
        expected: &TransportIdentity,
    ) -> Result<(), IdentityError> {
        let restored = Self::from_phrase(phrase)?;
        let got = restored.transport_identity()?;
        if got.as_str() == expected.as_str() {
            Ok(())
        } else {
            Err(IdentityError::PeerIdMismatch {
                got: got.as_str().to_owned(),
                expected: expected.as_str().to_owned(),
            })
        }
    }

    /// Write the identity to `path`, owner-only and atomically.
    ///
    /// The file holds the libp2p portable private-key encoding, which is
    /// what `IDENTITY.md` specifies. That encoding contains the seed, so
    /// the file is private-key-equivalent and is written with the same
    /// care.
    ///
    /// Refuses a path that already holds an identity. `write_private_atomic`
    /// renames over its target, so without this
    /// `ProfileIdentity::generate().save(existing)` destroys an
    /// established key and hands the profile a new PeerId — the exact
    /// silent regeneration [`IdentityError::NotFound`] exists to
    /// prevent, arriving through the other door. Rotation is a decision,
    /// so it has its own call: [`Self::replace_saved`].
    ///
    /// THE FILESYSTEM decides the path is free, in the operation that
    /// installs the file. Checking first and writing second leaves a
    /// window: two processes initializing the same profile both pass the
    /// check before either writes, and the loser silently replaces the
    /// identity the winner had already established — which is the very
    /// failure this refusal exists to prevent, reintroduced by the shape
    /// of the guard.
    ///
    /// # Errors
    /// Returns [`IdentityError::AlreadyExists`] if `path` exists, or
    /// [`IdentityError::Storage`] if the write fails.
    pub fn save(&self, path: &Path) -> Result<(), IdentityError> {
        let encoded = self.encoded()?;
        create_private_exclusive(path, &encoded).map_err(|e| match e {
            PersistError::AlreadyExists => IdentityError::AlreadyExists,
            other => IdentityError::Storage(other),
        })
    }

    /// Replace the identity stored at `path`, rotating the profile.
    ///
    /// Separated from [`Self::save`] because the consequence is
    /// different in kind: the profile's persistent PeerId changes, and
    /// every trust relationship established against the old one stops
    /// resolving. Nothing here makes that safe — it makes it *stated*.
    ///
    /// `replacing` is the identity the caller believes is stored, and it
    /// is checked against the file. A rotation is only ever intentional
    /// about a specific key: naming the wrong one means the caller is
    /// operating on a profile it has not actually read, and the answer
    /// to that is to stop, not to overwrite. The returned [`Rotation`]
    /// carries both PeerIds so what changed can be recorded, announced,
    /// or shown to a human before anything else acts on it.
    ///
    /// # Errors
    /// Returns [`IdentityError::NotFound`] if nothing is stored there,
    /// [`IdentityError::PeerIdMismatch`] if the stored identity is not
    /// `replacing`, or [`IdentityError::Storage`] if the write fails or the
    /// stored key's directory is not private. Reaches the same `load` as
    /// `restore_replace`, so it can also return
    /// [`IdentityError::RotationInProgress`], [`IdentityError::NotAFile`],
    /// [`IdentityError::PermissionsTooOpen`] and [`IdentityError::Corrupt`]
    /// -- none of which this block listed. Review finding on PR #86.
    pub fn replace_saved(
        &self,
        path: &Path,
        replacing: &TransportIdentity,
    ) -> Result<Rotation, IdentityError> {
        // COMPARE AND SWAP, not compare then swap.
        //
        // Reading the stored identity, checking it, and then writing
        // leaves a window: two processes rotating the same profile, both
        // naming the identity that really is stored, both pass the check
        // before either writes. Both then return a successful `Rotation`
        // while the later write silently replaces the earlier one — so
        // the first caller reports an identity that is no longer there,
        // which is precisely the guarantee `replacing` was added to
        // provide.
        //
        // `link` is the exclusive acquire: it fails with `EEXIST` and
        // never replaces, so exactly one rotation can hold the marker.
        // It also pins the inode that was at `path` when it succeeded,
        // which is what makes the identity read below the one actually
        // being replaced rather than whatever is there a moment later.
        holding_marker(path, |marker| self.rotate_holding(path, marker, replacing))
    }

    /// The rotation itself, with the marker held.
    fn rotate_holding(
        &self,
        path: &Path,
        marker: &Path,
        replacing: &TransportIdentity,
    ) -> Result<Rotation, IdentityError> {
        // Read through the MARKER, not through `path`: the marker is the
        // inode this rotation acquired, and it is the one being replaced.
        let stored = Self::load(marker)?.transport_identity()?;
        if stored.as_str() != replacing.as_str() {
            return Err(IdentityError::PeerIdMismatch {
                got: stored.as_str().to_owned(),
                expected: replacing.as_str().to_owned(),
            });
        }
        let current = self.transport_identity()?;
        self.write_to(path)?;
        Ok(Rotation {
            previous: stored,
            current,
        })
    }

    /// Restore a profile from its recovery phrase into an EMPTY `path`.
    ///
    /// The whole point of a restore is that the caller knows which
    /// profile they are restoring, so `expected` is required and checked
    /// before anything touches the filesystem. A checksum-valid phrase
    /// for a different key is still checksum-valid; without the
    /// comparison this would cheerfully install a stranger's identity and
    /// report success.
    ///
    /// REFUSES AN ESTABLISHED PROFILE rather than overwriting it, which
    /// is `IDENTITY-RECOVERY.md` item 8 and the sentence beside it: a
    /// restore "must not overwrite an established profile
    /// automatically". Replacing one is [`Self::restore_replace`], which
    /// requires naming what is being replaced. An earlier single
    /// `restore` did both — it took the rotation marker for exclusion and
    /// then ignored it, so it never read what was stored, and a valid
    /// phrase for B installed B over an established A and reported
    /// success. The marker made that safe against concurrent rotations
    /// and not against the thing item 8 forbids. Review finding.
    ///
    /// # Errors
    /// Returns [`IdentityError::PeerIdMismatch`] if the phrase
    /// reconstructs a different identity, [`IdentityError::AlreadyExists`]
    /// if anything is stored at `path`, the phrase errors of
    /// [`Self::from_phrase`], or [`IdentityError::Storage`] if the write
    /// fails.
    pub fn restore_new(
        path: &Path,
        phrase: &RecoveryPhrase,
        expected: &TransportIdentity,
    ) -> Result<Self, IdentityError> {
        Self::verify_phrase(phrase, expected)?;
        let restored = Self::from_phrase(phrase)?;
        // The exclusive create is what makes two concurrent restores
        // safe, and it is also what refuses an established profile --
        // one mechanism for both, which is why this path needs no
        // marker of its own.
        restored.save(path)?;
        Ok(restored)
    }

    /// Restore a profile from its recovery phrase OVER an established
    /// one, naming the identity being replaced.
    ///
    /// `IDENTITY-RECOVERY.md` item 8: for an established profile,
    /// replacement is refused "unless the expected old PeerId matches and
    /// the operator explicitly chooses the restore/replace path". Calling
    /// this IS that choice, and `replacing` is that match.
    ///
    /// The replacement itself is [`Self::replace_saved`]'s
    /// compare-and-swap, reused rather than restated: the marker pins the
    /// inode, the stored identity is read through it, and the write
    /// happens only if it is the one named. Two restores, or a restore
    /// and a rotation, therefore cannot both report having replaced the
    /// same identity.
    ///
    /// A CORRUPT KEY FILE HAS NO PATH THROUGH HERE, and that is a
    /// consequence of splitting `restore` worth stating. This reads the
    /// identity being replaced in order to check it is the one named, so a
    /// file it cannot decode is a `Corrupt` error rather than something to
    /// overwrite -- and `restore_new` refuses because the file exists. The
    /// old single `restore` overwrote it silently. An operator removes the
    /// file; nothing in this API will do it for them, because a key this
    /// build cannot read may still be a key some other build can.
    ///
    /// # Errors
    /// Returns [`IdentityError::PeerIdMismatch`] if the phrase
    /// reconstructs something other than `expected` or the stored
    /// identity is not `replacing`, [`IdentityError::NotFound`] if
    /// nothing is stored at `path`,
    /// [`IdentityError::RotationInProgress`] if another rotation or
    /// restore holds the marker, [`IdentityError::Corrupt`] if the stored
    /// identity cannot be decoded -- reachable through the `load` this
    /// performs to check the file really is `replacing`, and the case the
    /// paragraph above is entirely about -- and for the same reason
    /// [`IdentityError::NotAFile`] and [`IdentityError::PermissionsTooOpen`],
    /// which that `load` applies to the stored identity exactly as it does
    /// to a direct one -- or
    /// [`IdentityError::Storage`] if the write fails OR if the directory
    /// holding the stored key is a symlink or accessible to group or other
    /// -- the read-side cause `load` documents, reached through the same
    /// `load` as the three variants above, and not something a caller would
    /// expect from a sentence about writing.
    pub fn restore_replace(
        path: &Path,
        phrase: &RecoveryPhrase,
        expected: &TransportIdentity,
        replacing: &TransportIdentity,
    ) -> Result<(Self, Rotation), IdentityError> {
        Self::verify_phrase(phrase, expected)?;
        let restored = Self::from_phrase(phrase)?;
        let rotation = restored.replace_saved(path, replacing)?;
        Ok((restored, rotation))
    }

    fn write_to(&self, path: &Path) -> Result<(), IdentityError> {
        write_private_atomic(path, &self.encoded()?)?;
        Ok(())
    }

    fn encoded(&self) -> Result<Vec<u8>, IdentityError> {
        Keypair::from(self.keypair.clone())
            .to_protobuf_encoding()
            .map_err(|e| IdentityError::Corrupt(e.to_string()))
    }

    /// Load the identity from `path`.
    ///
    /// # Errors
    /// Returns [`IdentityError::NotFound`] if there is no file — never a
    /// freshly generated identity, because an established profile that
    /// silently regenerates hands itself a new PeerId and invalidates
    /// every trust relationship anyone had with it, while looking like a
    /// successful start.
    ///
    /// Returns [`IdentityError::Storage`] if the DIRECTORY holding the key
    /// is a symlink or is accessible to group or other. This is a separate
    /// object from the file mode below and surfaces as a different variant,
    /// which the operator-visible contract did not say: a state directory
    /// that drifted to `0755` -- what a hand-made `mkdir` gives under the
    /// usual umask -- fails start-up with a storage error rather than a
    /// permissions one. Review finding on PR #86.
    ///
    /// Returns [`IdentityError::NotAFile`] for a path that is not a regular
    /// file, and [`IdentityError::Corrupt`] for a file this build cannot
    /// decode or one past the 4 KiB ceiling -- both reachable, neither
    /// previously listed.
    ///
    /// Returns [`IdentityError::PermissionsTooOpen`] if the file is
    /// readable by anyone else. Refused rather than repaired: a key that
    /// has been exposed should be treated as disclosed, and tightening
    /// the mode quietly would hide that it ever was.
    pub fn load(path: &Path) -> Result<Self, IdentityError> {
        // ONE HANDLE, CHECKED AND READ. Every check here used to be a
        // separate lookup BY PATHNAME -- `symlink_metadata`, then
        // `is_owner_only`'s own `metadata`, then `read` -- so a directory
        // entry swapped between them let the checks inspect the
        // legitimate mode-0600 key and the read take something else,
        // a symlink included. The file is opened once and every
        // subsequent question is asked of that open handle, so there is
        // no window between the answers and the bytes.
        //
        // The parent is verified FIRST, because that is what makes the
        // swap impossible rather than merely detectable: an attacker who
        // cannot write the directory cannot replace the entry at all.
        // `persistence`'s private writes already require this of the
        // directory they write into, and a private key is the one file
        // that should not be read under weaker terms than it was
        // written. Review finding.
        // `Some("")` FOR A BARE RELATIVE PATH, which the first version of
        // this check filtered out -- so `load("identity.key")` ran no
        // directory check at all, and the reader WAS weaker than the
        // writer for exactly the input this paragraph says it must not be.
        // `profile-config`'s own `parent_dir` maps the empty parent to `.`
        // and both private writers go through it; this is that, restated
        // here because the helper is private to that crate. Review finding
        // on PR #86.
        // Unconditional: every path has a directory to check, because
        // `parent_or_dot` supplies `.` for the one shape that has no
        // directory component. The braces scope the `match` and are not a
        // condition that went missing.
        {
            match require_private_dir(parent_or_dot(path)) {
                Ok(()) => {}
                Err(PersistError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(IdentityError::NotFound);
                }
                Err(e) => return Err(IdentityError::Storage(e)),
            }
        }

        // What is AT the path, before opening: a symlink is refused
        // rather than followed, because an identity that has been
        // redirected should be reported, not silently loaded from
        // elsewhere.
        let here = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(IdentityError::NotFound);
            }
            Err(e) => return Err(IdentityError::Storage(PersistError::Io(e))),
        };
        if here.file_type().is_symlink() || !here.is_file() {
            return Err(IdentityError::NotAFile);
        }

        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(IdentityError::NotFound);
            }
            Err(e) => return Err(IdentityError::Storage(PersistError::Io(e))),
        };
        let opened = file
            .metadata()
            .map_err(|e| IdentityError::Storage(PersistError::Io(e)))?;

        // THE SAME INODE THE CHECK ABOVE SAW. If the entry changed
        // between them the open followed something else, and that is a
        // refusal rather than a retry: the only thing that swaps a key
        // file under a loader is something that should not be able to.
        //
        // BELT AND BRACES, AND NO TEST REACHES IT. The private parent
        // above is the guarantee -- it removes the ability to swap the
        // entry at all -- and this only narrows the window that remains
        // if the directory is private but something inside the process
        // raced. Triggering it needs a swap between two syscalls, which
        // a test cannot schedule deterministically, so this is a claim
        // deliberately left unenforced rather than one dressed up with a
        // test that agrees with it for free.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if (opened.dev(), opened.ino()) != (here.dev(), here.ino()) {
                return Err(IdentityError::NotAFile);
            }
        }

        if !opened.is_file() {
            return Err(IdentityError::NotAFile);
        }
        // Asked of the HANDLE. `is_owner_only` takes a path, so calling
        // it here would reintroduce the lookup this function just
        // eliminated.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if opened.permissions().mode() & 0o077 != 0 {
                return Err(IdentityError::PermissionsTooOpen);
            }
        }
        // UNREACHABLE SINCE THE DIRECTORY CHECK MOVED ABOVE IT.
        // `require_private_dir` answers `UnsupportedPlatform` off unix and
        // now runs first, so `load` fails before this branch. No
        // behavioural change -- `is_owner_only` returned the same error
        // from here, so non-unix `load` already failed -- but the branch
        // documents a fallback that cannot be taken. Kept rather than
        // deleted because it is the shape a future unix-less port needs,
        // and deleting it would hide that this function has a
        // platform-specific half at all. Review finding on PR #86.
        #[cfg(not(unix))]
        {
            if !interweave_profile_config::is_owner_only(path)? {
                return Err(IdentityError::PermissionsTooOpen);
            }
        }

        // A CEILING BEFORE THE ALLOCATION, from the handle's own size.
        // `read` sized its buffer from the file, so a local oversized
        // file could exhaust memory before the decoder ever rejected it.
        // A protobuf Ed25519 keypair is a handful of bytes; nothing
        // legitimate approaches this.
        if opened.len() > MAX_KEY_FILE_BYTES {
            return Err(IdentityError::Corrupt(format!(
                "key file is {} bytes; the maximum is {MAX_KEY_FILE_BYTES}",
                opened.len()
            )));
        }

        // ZEROED ON EVERY PATH, including the failing one. Filling the
        // buffer after a successful decode left the seed in allocator
        // memory whenever decoding FAILED -- the case where a caller is
        // least likely to be looking. Bounded by the ceiling above, and
        // read from the handle that was measured.
        use std::io::Read as _;
        let mut buf = Zeroizing(Vec::with_capacity(
            usize::try_from(opened.len()).unwrap_or(0),
        ));
        file.take(MAX_KEY_FILE_BYTES)
            .read_to_end(&mut buf.0)
            .map_err(|e| IdentityError::Storage(PersistError::Io(e)))?;
        let bytes = buf;

        let keypair = Keypair::from_protobuf_encoding(&bytes.0)
            .map_err(|e| IdentityError::Corrupt(e.to_string()))?;

        let ed = keypair
            .try_into_ed25519()
            .map_err(|e| IdentityError::Corrupt(e.to_string()))?;
        Ok(Self { keypair: ed })
    }
}

impl core::fmt::Debug for ProfileIdentity {
    /// Prints the PeerId and nothing else.
    ///
    /// A derived `Debug` would put the secret into any panic message or
    /// tracing span that formatted it. The PeerId is public by
    /// construction and is the only part worth seeing.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let peer = PeerId::from_public_key(&Keypair::from(self.keypair.clone()).public());
        write!(f, "ProfileIdentity({peer})")
    }
}

/// The directory a path is read from, with `.` for a bare filename.
///
/// `Path::new("identity.key").parent()` is `Some("")`, not `None`, and an
/// earlier version of [`ProfileIdentity::load`]'s directory check filtered
/// that out -- so the one path shape with no directory component was read
/// with no directory check at all. That made the reader weaker than the
/// writer for exactly the input `load`'s own comment says it must not be:
/// both private writers go through `profile-config`'s `parent_dir`, which
/// maps the empty parent to `.`. This is that rule, restated because the
/// helper is private to that crate. Review finding on PR #86.
///
/// Tested beside the source rather than through `load`, because producing
/// a `Some("")` parent in an integration test means changing the process's
/// working directory -- which is global, and the suite runs two threads.
/// `Path::new("/")` answers `.` rather than `/`, because `parent()` is
/// `None` there. That is a deliberate copy of `profile-config`'s
/// `parent_dir`, which has the identical edge: the two must agree or the
/// reader and the writer check different directories, and agreeing matters
/// more than the edge. No caller passes a filesystem root.
fn parent_or_dot(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

#[cfg(test)]
mod tests {
    use super::parent_or_dot;
    use std::path::Path;

    #[test]
    fn a_bare_filename_reads_from_the_current_directory_not_from_nowhere() {
        // The case that was skipped. `Some("")` is not `None`, and
        // filtering it out is what removed the check entirely.
        assert_eq!(parent_or_dot(Path::new("identity.key")), Path::new("."));
        assert_eq!(parent_or_dot(Path::new("")), Path::new("."));
    }

    #[test]
    fn a_path_with_a_directory_keeps_it() {
        // The control: the ordinary shape must be unaffected, or the fix
        // would be checking the wrong directory for every real caller.
        assert_eq!(
            parent_or_dot(Path::new("/var/lib/interweave/identity.key")),
            Path::new("/var/lib/interweave")
        );
        assert_eq!(
            parent_or_dot(Path::new("state/identity.key")),
            Path::new("state")
        );
    }
}
