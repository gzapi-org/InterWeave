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
    PersistError, create_private_exclusive, is_owner_only, write_private_atomic,
};
use interweave_transport_api::{IdError, TransportIdentity};
use libp2p_identity::{Keypair, PeerId, ed25519};

pub use record::{ALGORITHM, FORMAT, RecoveryRecord};
pub use recovery::{ENTROPY_BYTES, PHRASE_WORDS, RecoveryPhrase};

/// What can go wrong with a profile identity.
#[derive(Debug)]
pub enum IdentityError {
    /// The key file does not exist.
    ///
    /// Deliberately distinct from every other error: an established
    /// profile must never silently regenerate, so the caller has to
    /// decide between "create a new profile" and "restore this one".
    NotFound,
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
    /// resolving. Nothing here makes that safe — it makes it *stated*,
    /// so a rotation cannot happen because a save was pointed at an
    /// occupied path.
    ///
    /// # Errors
    /// Returns [`IdentityError::Storage`] if the write fails.
    pub fn replace_saved(&self, path: &Path) -> Result<(), IdentityError> {
        self.write_to(path)
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
    /// Returns [`IdentityError::PermissionsTooOpen`] if the file is
    /// readable by anyone else. Refused rather than repaired: a key that
    /// has been exposed should be treated as disclosed, and tightening
    /// the mode quietly would hide that it ever was.
    pub fn load(path: &Path) -> Result<Self, IdentityError> {
        if !path.exists() {
            return Err(IdentityError::NotFound);
        }
        if !is_owner_only(path)? {
            return Err(IdentityError::PermissionsTooOpen);
        }
        let mut bytes =
            std::fs::read(path).map_err(|e| IdentityError::Storage(PersistError::Io(e)))?;
        let keypair = Keypair::from_protobuf_encoding(&bytes)
            .map_err(|e| IdentityError::Corrupt(e.to_string()))?;
        // The buffer held the seed. Overwrite it rather than leaving a
        // copy for whatever reuses the allocation.
        bytes.fill(0);

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
