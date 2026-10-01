// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `IDENTITY-RECOVERY.md` §Required conformance tests, one to one (plan
//! §16 (10)): each item, the test that fails if it stops being true. Most
//! live in `identity_lifecycle.rs` and in `transportctl`'s process tests;
//! the ones that did not exist are here.
//!
//! 1. Golden zero-secret phrase decodes to 32 zero bytes and the expected
//!    `PeerId`: `identity_lifecycle::the_frozen_golden_reconstructs_through_this_adapter`.
//! 2. Random secret -> 24 words -> the same secret -> the same `PeerId`:
//!    `identity_lifecycle::a_restored_identity_round_trips_to_the_same_file`
//!    (byte-for-byte), and
//!    `a_record_round_trips_through_json_and_restores_the_same_identity`.
//! 3. A one-word mutation failing the checksum is rejected:
//!    [`one_substituted_word_fails_the_checksum`], beside
//!    `identity_lifecycle::the_checksum_catches_a_transposition`.
//! 4. A checksum-valid phrase with the wrong expected `PeerId` is rejected:
//!    `identity_lifecycle::verify_is_read_only_and_fails_closed_on_the_wrong_phrase`,
//!    `a_record_naming_another_identity_is_refused`; as a process,
//!    `transportctl`'s `a_phrase_for_another_identity_writes_nothing`.
//! 5. 12/15/18/21-word phrases are rejected:
//!    [`every_shorter_bip39_length_is_refused_by_its_count`].
//! 6. A non-English wordlist is rejected:
//!    [`english_is_the_only_wordlist_compiled_in`] and
//!    [`a_phrase_of_non_english_words_is_refused`].
//! 7. The BIP-39 PBKDF2 seed is never the Ed25519 secret:
//!    `identity_lifecycle::the_golden_entropy_is_the_seed_not_a_derivation`
//!    and `the_recovery_entropy_is_the_seed_never_the_64_byte_keypair_form`.
//! 8. Export/import unavailable through IPC: `ipc-protocol`'s
//!    `no_method_reaches_the_identity`. Through Claude tools: no Claude
//!    tool exists before Stage 16, and that stage owes the test.
//! 9. The phrase absent from logs, crash reports and config fixtures:
//!    `identity_lifecycle::no_secret_material_reaches_debug_output`,
//!    `a_record_carries_no_words_into_debug_output`,
//!    [`a_malformed_record_is_refused_without_its_words`], `transportctl`'s
//!    refusals asserted word-free (its `cli` tests and its process
//!    tests), and [`no_config_example_or_test_data_carries_a_phrase`].
//! 10. An established key's overwrite fails closed without the explicit
//!     matching restore: `identity_lifecycle::saving_over_an_established_identity_is_refused`,
//!     `a_restore_cannot_replace_an_established_profile_without_naming_it`;
//!     as a process, `transportctl`'s
//!     `an_established_key_is_never_overwritten_by_a_new_restore` and
//!     `a_replace_must_name_the_stored_identity`.
//! 11. Rotation produces a distinct `PeerId` and a distinct phrase:
//!     [`a_rotation_yields_a_distinct_peer_and_phrase`].
//! 12. The verify-only drill writes no key, mutates no profile, uses no
//!     IPC and no network: `identity_lifecycle::verify_touches_no_file`;
//!     as a process, `transportctl identity verify` runs with an empty
//!     environment -- no XDG tree, so no profile and no socket to reach.
//! 13. A phrase restored without its configuration is a bare identity:
//!     `identity_lifecycle::a_record_with_an_unknown_field_is_refused` (a
//!     record carries the identity and nothing else), and `transportctl`'s
//!     `a_restore_changes_nothing_but_the_key`.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use interweave_profile_identity::{IdentityError, ProfileIdentity, RecoveryPhrase};

/// The golden fixture's phrase (IDENTITY-RECOVERY.md): test-only.
const GOLDEN: &str = "abandon abandon abandon abandon abandon abandon abandon abandon \
     abandon abandon abandon abandon abandon abandon abandon abandon \
     abandon abandon abandon abandon abandon abandon abandon art";

/// Item 3. `art` is the one last word the zero entropy's checksum allows;
/// any other word of the list in its place is a different checksum.
#[test]
fn one_substituted_word_fails_the_checksum() {
    assert!(RecoveryPhrase::parse(GOLDEN).is_ok(), "the control parses");
    for substitute in ["abandon", "zoo", "about"] {
        let mutated = GOLDEN.replace(" art", &format!(" {substitute}"));
        assert!(
            matches!(
                RecoveryPhrase::parse(&mutated),
                Err(IdentityError::Bip39(_))
            ),
            "{substitute} in the last place must fail the checksum"
        );
    }
}

/// Item 5. Each is the standard checksum-VALID zero-entropy phrase of its
/// length, so the refusal is the count, not the checksum.
#[test]
fn every_shorter_bip39_length_is_refused_by_its_count() {
    for (words, last) in [(12, "about"), (15, "address"), (18, "agent"), (21, "admit")] {
        let phrase = format!("{} {last}", vec!["abandon"; words - 1].join(" "));
        match RecoveryPhrase::parse(&phrase) {
            Err(IdentityError::WrongWordCount { got, want: 24 }) => assert_eq!(got, words),
            other => panic!("{words} words: refused by count, got {other:?}"),
        }
    }
}

/// Item 6, the guarantee: the wordlists the phrase is read against are
/// the ones compiled in, and a feature enabling another anywhere in the
/// graph would add it -- this fails on that day.
#[test]
fn english_is_the_only_wordlist_compiled_in() {
    assert_eq!(bip39::Language::ALL, &[bip39::Language::English]);
}

/// Item 6, the behaviour: 24 words of another language's list are not
/// words at all here.
#[test]
fn a_phrase_of_non_english_words_is_refused() {
    for word in ["ábaco", "abaisser", "abaco", "あいこくしん"] {
        let phrase = vec![word; 24].join(" ");
        assert!(RecoveryPhrase::parse(&phrase).is_err(), "{word}: refused");
    }
}

/// Item 11. A rotation is a new key: its `PeerId` and its phrase both
/// differ from the old, and the old phrase still restores the old one.
#[test]
fn a_rotation_yields_a_distinct_peer_and_phrase() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("identity.key");
    let old = ProfileIdentity::generate();
    old.save(&path).expect("saved");
    let old_peer = old.transport_identity().expect("a peer id");
    let old_phrase = old.recovery_phrase().expect("a phrase");

    let new = ProfileIdentity::generate();
    let rotation = new.replace_saved(&path, &old_peer).expect("rotated");
    assert_eq!(rotation.previous, old_peer);
    assert_ne!(rotation.current, old_peer, "a distinct PeerId");
    assert_ne!(
        new.recovery_phrase().expect("a phrase").expose_words(),
        old_phrase.expose_words(),
        "a distinct phrase"
    );
    assert_eq!(
        ProfileIdentity::from_phrase(&old_phrase)
            .expect("restores")
            .transport_identity()
            .expect("a peer id"),
        old_peer,
        "the old phrase does not follow the rotation"
    );
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/identity/profile-identity sits three below the root")
        .to_path_buf()
}

/// Every file under `dir`, recursively.
fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("a directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// The longest run of consecutive English wordlist words in `text`.
fn longest_word_run(text: &str, list: &[&str]) -> usize {
    let mut best = 0;
    let mut run = 0;
    for token in text.split(|c: char| !c.is_ascii_lowercase()) {
        if token.is_empty() {
            continue;
        }
        if list.binary_search(&token).is_ok() {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// Item 9, the fixtures half: no shipped example configuration and no
/// test-data file carries twelve or more consecutive wordlist words --
/// the shortest BIP-39 phrase. `fixtures/identity` is the one place a
/// phrase is frozen, by design, and is not scanned.
#[test]
fn no_config_example_or_test_data_carries_a_phrase() {
    let root = repository_root();
    let mut scanned = Vec::new();
    for dir in ["architecture/config", "test-data"] {
        files(&root.join(dir), &mut scanned);
    }
    assert!(
        scanned.len() > 10,
        "the scan reached the examples: {scanned:?}"
    );
    let list = bip39::Language::English.word_list();
    assert!(
        longest_word_run(GOLDEN, list) == 24,
        "the control: the golden phrase is a 24-word run"
    );
    for path in scanned {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let run = longest_word_run(&text, list);
        assert!(
            run < 12,
            "{} carries a run of {run} wordlist words",
            path.display()
        );
    }
}

/// Item 9, the error half: a record refused for its shape names the
/// shape, never a word -- the phrase as one string, and a word outside
/// the grammar, are refused by position. An error is what gets printed
/// and logged.
#[test]
fn a_malformed_record_is_refused_without_its_words() {
    let record = |words: serde_json::Value| {
        serde_json::json!({
            "format": interweave_profile_identity::FORMAT,
            "identity_algorithm": interweave_profile_identity::ALGORITHM,
            "words": words,
        })
        .to_string()
    };
    let as_one_string = record(serde_json::Value::from(GOLDEN));
    let err = serde_json::from_str::<interweave_profile_identity::RecoveryRecord>(&as_one_string)
        .expect_err("a string is not the array");
    assert!(!err.to_string().contains("abandon"), "{err}");

    let mut words: Vec<String> = GOLDEN.split_whitespace().map(str::to_owned).collect();
    words[3] = "Abandon".to_owned();
    let parsed: interweave_profile_identity::RecoveryRecord =
        serde_json::from_str(&record(serde_json::json!(words))).expect("parses");
    let err = parsed.validate().expect_err("outside the grammar");
    let text = err.to_string();
    assert!(
        !text.to_lowercase().contains("abandon") && text.contains("word 3"),
        "{text}"
    );
}
