// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The `interweave-ed25519-bip39-entropy-v1` backup record.
//!
//! The file a human writes to paper or a password manager. Unlike
//! [`crate::RecoveryPhrase`], this type IS serializable — that is its
//! whole purpose — which makes it the one place private-key-equivalent
//! material is deliberately written down.
//!
//! That makes the surrounding rules sharper, not looser:
//!
//! - it is a LOCAL artifact. It must never cross IPC, a Channel event, a
//!   transport message, discovery, the endpoint directory, or a log
//!   (ADR-0033, `IDENTITY.md`).
//! - `Debug` is redacted, so it cannot reach a panic message or a
//!   tracing span the way a derived one would.
//! - restore fails CLOSED on `expected_peer_id` mismatch, because a
//!   checksum-valid phrase for a different key would otherwise restore a
//!   working identity that is not the one anybody wanted.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::{IdentityError, ProfileIdentity, RecoveryPhrase, recovery::PHRASE_WORDS};

/// The only format identifier this build reads or writes.
pub const FORMAT: &str = "interweave-ed25519-bip39-entropy-v1";

/// The only identity algorithm this format carries.
pub const ALGORITHM: &str = "ed25519";

/// A recovery record, as stored.
///
/// See the module documentation: serializable on purpose, and local-only.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRecord {
    /// Always [`FORMAT`].
    pub format: String,
    /// Always [`ALGORITHM`]. Restore refuses anything else rather than
    /// attempting a conversion.
    pub identity_algorithm: String,
    /// The PeerId this phrase must reconstruct.
    ///
    /// Optional in the record only because a phrase may survive alone —
    /// on paper, without the file. When present, restore requires an
    /// exact match.
    ///
    /// Absent or a PeerId. NOT `null`: the schema permits a string here
    /// and does not include null, and absence is what means "this record
    /// carries no identity check". An explicit null read as absence would
    /// silently downgrade a record from checked to unchecked.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_peer_id"
    )]
    pub expected_peer_id: Option<String>,
    /// Exactly 24 words.
    ///
    /// 256 bits of entropy plus an 8-bit checksum. The shorter BIP-39
    /// lengths are refused for this format rather than accepted with less
    /// entropy.
    pub words: Vec<String>,
}

/// An optional PeerId that may be ABSENT but never explicitly `null`.
fn absent_or_peer_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    Option::<String>::deserialize(deserializer)?
        .map(Some)
        .ok_or_else(|| D::Error::custom("must be a PeerId or omitted entirely, not null"))
}

impl fmt::Debug for RecoveryRecord {
    /// Prints no words.
    ///
    /// The record exists to be written to one file a human controls. A
    /// derived `Debug` would additionally write it to every log line,
    /// panic message and crash report that happened to format it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoveryRecord")
            .field("format", &self.format)
            .field("identity_algorithm", &self.identity_algorithm)
            .field("expected_peer_id", &self.expected_peer_id)
            .field(
                "words",
                &format_args!("<{} words redacted>", self.words.len()),
            )
            .finish()
    }
}

impl RecoveryRecord {
    /// Build a record for `identity`.
    ///
    /// `expected_peer_id` is always included when writing. It is optional
    /// in the schema for records that arrive without it, not because
    /// omitting it is a good idea: it is the only thing that turns a
    /// checksum check into an identity check.
    ///
    /// # Errors
    /// Returns [`IdentityError`] if the phrase cannot be derived.
    pub fn of(identity: &ProfileIdentity) -> Result<Self, IdentityError> {
        let phrase = identity.recovery_phrase()?;
        let peer = identity.transport_identity()?;
        Ok(Self {
            format: FORMAT.to_owned(),
            identity_algorithm: ALGORITHM.to_owned(),
            expected_peer_id: Some(peer.as_str().to_owned()),
            words: phrase
                .expose_words()
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        })
    }

    /// Check the record's own shape before using any of it.
    ///
    /// # Errors
    /// Returns [`IdentityError`] for an unknown format, a non-Ed25519
    /// algorithm, or a word count other than 24.
    pub fn validate(&self) -> Result<(), IdentityError> {
        if self.format != FORMAT {
            return Err(IdentityError::Bip39(format!(
                "unknown recovery format {:?}; this build reads {FORMAT}",
                self.format
            )));
        }
        if self.identity_algorithm != ALGORITHM {
            return Err(IdentityError::Bip39(format!(
                "identity_algorithm {:?} is refused rather than converted",
                self.identity_algorithm
            )));
        }
        if self.words.len() != PHRASE_WORDS {
            return Err(IdentityError::WrongWordCount {
                got: self.words.len(),
                want: PHRASE_WORDS,
            });
        }
        // The schema pins each word to `^[a-z]{3,8}$` — the English
        // wordlist is ASCII and no entry is outside those bounds. Checked
        // here as well as by the checksum because a word carrying
        // whitespace or a control character would otherwise reach the
        // joiner and change the phrase's meaning silently.
        for word in &self.words {
            let ok = (3..=8).contains(&word.len()) && word.bytes().all(|b| b.is_ascii_lowercase());
            if !ok {
                return Err(IdentityError::Bip39(format!(
                    "word {word:?} is outside the English BIP-39 wordlist grammar"
                )));
            }
        }
        // And the identity check must itself be well formed: a malformed
        // `expected_peer_id` cannot match anything, so accepting one
        // would leave a record that looks checked and is not.
        if let Some(expected) = &self.expected_peer_id {
            interweave_transport_api::TransportIdentity::parse(expected.clone())
                .map_err(IdentityError::Id)?;
        }
        Ok(())
    }

    /// Reconstruct the identity this record describes.
    ///
    /// Fails CLOSED when `expected_peer_id` is present and does not
    /// match: a checksum-valid phrase for a different key reconstructs a
    /// perfectly working identity, and without this check the restore
    /// would report success while replacing the profile with a stranger.
    ///
    /// # Errors
    /// Returns [`IdentityError`] for a malformed record, an invalid
    /// phrase, or a PeerId mismatch.
    pub fn restore(&self) -> Result<ProfileIdentity, IdentityError> {
        self.validate()?;
        let phrase = RecoveryPhrase::parse(&self.words.join(" "))?;
        let identity = ProfileIdentity::from_phrase(&phrase)?;

        if let Some(expected) = &self.expected_peer_id {
            let got = identity.transport_identity()?;
            if got.as_str() != expected {
                return Err(IdentityError::PeerIdMismatch {
                    got: got.as_str().to_owned(),
                    expected: expected.clone(),
                });
            }
        }
        Ok(identity)
    }
}
