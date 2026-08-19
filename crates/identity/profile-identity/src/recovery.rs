// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The 24-word recovery phrase.
//!
//! # This type is private-key-equivalent
//!
//! The phrase encodes the exact 32-byte Ed25519 secret seed. Anyone
//! holding it holds the identity. `RETENTION.md`, `IDENTITY.md` and
//! ADR-0033 all say the same thing: it must never cross IPC, Channel
//! events, transport messages, discovery, the endpoint directory, logs,
//! or normal configuration.
//!
//! That is enforced here rather than asserted:
//!
//! - no `Display`, so it cannot be interpolated into a message;
//! - no `Serialize`, so it cannot be put in a config file, an IPC frame,
//!   or a JSON log;
//! - a hand-written `Debug` that prints no words, so it cannot reach a
//!   panic message, a tracing span, or a crash report;
//! - the only way to read the words is [`RecoveryPhrase::expose_words`],
//!   whose name is the warning.
//!
//! # The wallet path does not exist here
//!
//! BIP-39 also defines a PBKDF2 derivation from the words to a 64-byte
//! wallet seed. This project does not use it: the entropy IS the key
//! (ADR-0033). SPIKE-006 measured what happens if the two are confused —
//! the same 24 words produce a completely different PeerId — so the
//! failure would be silent. The `bip39::Mnemonic` is therefore private to
//! this module and never handed out; a caller cannot reach `to_seed`
//! because it cannot reach the type that has it.

use core::fmt;

use crate::IdentityError;

/// Bytes of entropy in a 24-word phrase, and of an Ed25519 seed.
pub const ENTROPY_BYTES: usize = 32;

/// Words in a phrase.
pub const PHRASE_WORDS: usize = 24;

/// A 24-word BIP-39 phrase encoding an Ed25519 secret seed.
///
/// See the module documentation for why this type has no `Display`, no
/// `Serialize`, and a redacted `Debug`.
#[derive(Clone)]
pub struct RecoveryPhrase {
    /// Private, and never handed out. Holding the `bip39::Mnemonic`
    /// itself would put `to_seed` — the wallet derivation this project
    /// must never use — one method call away from every caller.
    inner: bip39::Mnemonic,
}

impl RecoveryPhrase {
    /// Encode a 32-byte seed as 24 words.
    ///
    /// # Errors
    /// Returns [`IdentityError::Bip39`] if the entropy is not encodable,
    /// which for a fixed 32-byte input should not occur.
    pub fn from_entropy(entropy: &[u8; ENTROPY_BYTES]) -> Result<Self, IdentityError> {
        let inner = bip39::Mnemonic::from_entropy(entropy)
            .map_err(|e| IdentityError::Bip39(e.to_string()))?;
        Ok(Self { inner })
    }

    /// Parse a phrase a human typed.
    ///
    /// Validates the BIP-39 checksum, which catches a mistyped or
    /// reordered word. It does NOT prove the phrase is the right
    /// identity — a checksum-valid phrase for a different key is still
    /// checksum-valid. [`crate::ProfileIdentity::verify_phrase`] is the
    /// check that answers that question.
    ///
    /// # Errors
    /// Returns [`IdentityError::Bip39`] for a bad checksum, an unknown
    /// word, or the wrong word count.
    pub fn parse(text: &str) -> Result<Self, IdentityError> {
        let inner: bip39::Mnemonic = text
            .trim()
            .parse()
            .map_err(|e: bip39::Error| IdentityError::Bip39(e.to_string()))?;
        if inner.word_count() != PHRASE_WORDS {
            return Err(IdentityError::WrongWordCount {
                got: inner.word_count(),
                want: PHRASE_WORDS,
            });
        }
        Ok(Self { inner })
    }

    /// The 32 bytes of entropy, which ARE the Ed25519 secret seed.
    ///
    /// Named for what it does. There is no BIP-39 passphrase and no
    /// PBKDF2 step: the entropy is the key (ADR-0033).
    ///
    /// # Errors
    /// Returns [`IdentityError::WrongEntropyLength`] if the phrase does
    /// not carry exactly 32 bytes.
    pub fn expose_entropy(&self) -> Result<[u8; ENTROPY_BYTES], IdentityError> {
        let (bytes, len) = self.inner.to_entropy_array();
        if len != ENTROPY_BYTES {
            return Err(IdentityError::WrongEntropyLength { got: len });
        }
        let mut out = [0u8; ENTROPY_BYTES];
        out.copy_from_slice(&bytes[..ENTROPY_BYTES]);
        Ok(out)
    }

    /// The words, for showing a human who asked to write them down.
    ///
    /// The name is the warning. Every call site is a place the phrase
    /// leaves this type, and each one should be somewhere a person is
    /// deliberately being shown their own recovery phrase — never a log,
    /// never an IPC reply, never an error message.
    #[must_use]
    pub fn expose_words(&self) -> String {
        self.inner.to_string()
    }
}

impl fmt::Debug for RecoveryPhrase {
    /// Prints nothing of the phrase.
    ///
    /// A derived `Debug` would put 24 words into whatever formatted it —
    /// a panic message, a tracing span, a crash report — all of which are
    /// places ADR-0033 says the phrase must never reach, and none of
    /// which this crate controls.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryPhrase(<24 words redacted>)")
    }
}

/// Equality compares the ENTROPY, not the rendered text.
///
/// Two phrases that encode the same seed are the same phrase whatever
/// their whitespace. Constant-time comparison is deliberately not claimed
/// here: this is used to check a restore against a fixture, never to
/// authenticate anything.
impl PartialEq for RecoveryPhrase {
    fn eq(&self, other: &Self) -> bool {
        self.inner.to_entropy_array().1 == other.inner.to_entropy_array().1
            && self.inner.to_entropy_array().0 == other.inner.to_entropy_array().0
    }
}

impl Eq for RecoveryPhrase {}
