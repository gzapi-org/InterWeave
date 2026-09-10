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
    #[serde(deserialize_with = "bounded_label")]
    pub format: String,
    /// Always [`ALGORITHM`]. Restore refuses anything else rather than
    /// attempting a conversion.
    #[serde(deserialize_with = "bounded_label")]
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
    ///
    /// BOUNDED WHILE READING, not after. [`Self::validate`] still checks
    /// the count and the grammar -- it is the contract's check and a
    /// hand-built record never went through a deserializer -- but by the
    /// time it ran, serde had already built the whole `Vec` and every
    /// `String` in it. A local file claiming ten million words was
    /// therefore allocated in full and then refused. This repository
    /// bounds before it allocates everywhere else, and a recovery record
    /// is read by someone who has already lost something. Review finding.
    #[serde(deserialize_with = "bounded_words")]
    pub words: Vec<String>,
}

/// The longest `format` or `identity_algorithm` worth retaining.
///
/// Both are compared against a constant, so anything longer is already
/// wrong; the only question is whether it is refused before or after
/// being copied. The two constants are 35 and 7 bytes.
const MAX_LABEL_BYTES: usize = 64;

/// The longest word in the English BIP-39 wordlist.
///
/// Both this and the count ceiling are the schema's own bounds, not
/// independent guesses: `identity/recovery-record.schema.json` pins
/// `words` to `maxItems: 24` with each entry `^[a-z]{3,8}$`. Enforcing
/// them while reading rather than after is the whole change -- the
/// values are the contract's.
const MAX_WORD_BYTES: usize = 8;

/// A short label, refused before it is retained.
fn bounded_label<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Visitor;

    impl serde::de::Visitor<'_> for Visitor {
        type Value = String;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "a label of at most {MAX_LABEL_BYTES} bytes")
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
            // CHECKED BEFORE `to_owned`. The parser has seen the token
            // either way -- that is its job -- but nothing here keeps it.
            if value.len() > MAX_LABEL_BYTES {
                return Err(E::custom(format!(
                    "a label of {} bytes cannot be any value this format defines",
                    value.len()
                )));
            }
            Ok(value.to_owned())
        }
    }

    deserializer.deserialize_str(Visitor)
}

/// AT MOST the words a phrase may have, refused as they arrive.
///
/// Not "exactly": a SHORT phrase deserializes here and is refused later, by
/// [`RecoveryRecord::validate`] and -- independently -- by
/// `RecoveryPhrase::parse`, which `restore` reaches through
/// `self.words.join(" ")`. An earlier version of this said `validate` was
/// "the only place", which is an over-claim in a smaller font.
///
/// THERE ARE THREE ENFORCERS DOWNSTREAM, not the two a correction of that
/// over-claim then named: `validate`'s length check, `parse`'s
/// `word_count()` check, and -- reached before either of them on this input
/// -- `bip39`'s own `is_invalid_word_count`, whose floor is twelve words.
///
/// `a_record_with_four_words_deserializes_and_is_refused_by_both_downstream_checks`
/// pins the FIRST of those and asserts the outcome of the other two. The
/// distinction is the point: deleting `validate`'s check turns that test
/// red, while deleting both InterWeave checks does not, because the
/// dependency refuses a four-word string on its own. So `restore` being
/// fail-closed for a short phrase is a floor this crate inherits rather
/// than an invariant it holds. Two reviews were needed to get this
/// paragraph to say that.
///
/// This title also said "Exactly" while the `expecting` string further down
/// this same function had already been corrected to "at most", with a
/// comment saying in as many words that exactness is a claim this function
/// does not make -- the pair went stale inside one function.
/// What this deserializer holds is the UPPER bound, which is what keeps a
/// hostile document from allocating: the lower bound is a validation
/// concern and has no attacker value. Review finding on PR #86.
fn bounded_words<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Word(String);

    impl<'de> Deserialize<'de> for Word {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct Visitor;

            impl serde::de::Visitor<'_> for Visitor {
                type Value = Word;

                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(f, "a BIP-39 word of at most {MAX_WORD_BYTES} bytes")
                }

                fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Word, E> {
                    if value.len() > MAX_WORD_BYTES {
                        return Err(E::custom(format!(
                            "a word of {} bytes is outside the English BIP-39 wordlist",
                            value.len()
                        )));
                    }
                    Ok(Word(value.to_owned()))
                }
            }

            d.deserialize_str(Visitor)
        }
    }

    struct Words;

    impl<'de> serde::de::Visitor<'de> for Words {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            // NOT "exactly", which this said until a review read it
            // against the code: the visitor enforces the CEILING, and the
            // lower bound is `validate`'s, so a three-word array
            // deserializes here and is refused there. "exactly" is a
            // claim this function does not make.
            write!(f, "at most {PHRASE_WORDS} BIP-39 words")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Vec<String>, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            use serde::de::Error as _;
            // CAPACITY FROM THE CEILING, never from the input's own
            // `size_hint`: a sequence is free to claim any length.
            let mut out: Vec<String> = Vec::with_capacity(PHRASE_WORDS);
            while let Some(Word(word)) = seq.next_element::<Word>()? {
                if out.len() == PHRASE_WORDS {
                    return Err(A::Error::custom(format!(
                        "more than {PHRASE_WORDS} words; this format carries exactly that many"
                    )));
                }
                out.push(word);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Words)
}

/// An optional PeerId that may be ABSENT but never explicitly `null`.
///
/// BOUNDED, like the other three string-bearing fields. The pass that
/// bounded `format`, `identity_algorithm` and `words` missed this one, so
/// a record carrying a ten-megabyte `expected_peer_id` was RETAINED in
/// full and then refused by `validate`'s `TransportIdentity::parse`, which
/// checks `MAX_BYTES` after the allocation rather than before it.
/// Review finding on PR #86.
///
/// RETAINED, not read. The parser must scan the whole string token before
/// it can hand it over -- that is its job, and a single scalar has no
/// early exit the way a sequence does -- so what the ceiling avoids here
/// is keeping the `String`, not touching the bytes. The counting-reader
/// measurement that covers `words` does NOT transfer to this field, and
/// saying so because the two were described as one defect.
fn absent_or_peer_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Visitor;

    impl serde::de::Visitor<'_> for Visitor {
        type Value = String;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "a PeerId of at most {} bytes",
                interweave_transport_api::TransportIdentity::MAX_BYTES
            )
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
            let max = interweave_transport_api::TransportIdentity::MAX_BYTES;
            if value.len() > max {
                return Err(E::custom(format!(
                    "a peer id of {} bytes cannot be one: the ceiling is {max}",
                    value.len()
                )));
            }
            Ok(value.to_owned())
        }
    }

    struct Outer;

    impl<'de> serde::de::Visitor<'de> for Outer {
        type Value = Option<String>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "a PeerId, or the field omitted entirely")
        }

        /// UNREACHABLE THROUGH JSON, and measured rather than assumed:
        /// mutating this arm to `Ok(None)` leaves every test green, while
        /// the same mutation to `visit_none` fails
        /// `an_explicit_null_expected_peer_id_is_still_refused`. So
        /// `serde_json` presents `null` as `None` and never as a unit, and
        /// this arm is a fail-closed guard for a format that does
        /// otherwise -- not the enforcement. Saying so because an
        /// untested arm that looks like enforcement is worse than none.
        fn visit_unit<E: serde::de::Error>(self) -> Result<Option<String>, E> {
            Err(E::custom("must be a PeerId or omitted entirely, not null"))
        }

        /// THE ARM THAT ENFORCES IT. An explicit `null` is refused rather
        /// than read as absent: absence means "this record was written
        /// without a check", while a `null` means someone wrote the field
        /// and emptied it, and reading the second as the first silently
        /// downgrades a record from checked to unchecked.
        fn visit_none<E: serde::de::Error>(self) -> Result<Option<String>, E> {
            Err(E::custom("must be a PeerId or omitted entirely, not null"))
        }

        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Option<String>, D::Error> {
            d.deserialize_str(Visitor).map(Some)
        }
    }

    deserializer.deserialize_option(Outer)
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
