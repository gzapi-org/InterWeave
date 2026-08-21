// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Identity generation, persistence, restart, and recovery.
//!
//! The claims SPIKE-006 could not make. A spike proves things about a
//! library boundary; these prove things about the adapter built on it,
//! and the restart cases in particular are properties of code that did
//! not exist when the spike ran.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use interweave_profile_identity::{IdentityError, ProfileIdentity, RecoveryPhrase};
use interweave_transport_api::TransportIdentity;

fn fixture() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/identity/<crate> is three levels below the root")
        .join("fixtures/identity/ed25519-bip39-entropy-v1.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("fixture is JSON")
}

#[test]
fn the_frozen_golden_reconstructs_through_this_adapter() {
    // The vector the Python verifier recomputes on every CI run, now
    // recomputed by the production Rust path too. Two independent
    // implementations agreeing is the point; a fixture only one thing
    // checks is that thing's opinion written down.
    let f = fixture();
    let v = &f["vectors"][0];
    let mnemonic = v["mnemonic"].as_str().expect("mnemonic");
    let expected_peer = v["expected_peer_id"].as_str().expect("peer id");

    let phrase = RecoveryPhrase::parse(mnemonic).expect("the golden phrase parses");
    let identity = ProfileIdentity::from_phrase(&phrase).expect("reconstructs");
    let got = identity.transport_identity().expect("peer id");

    assert_eq!(got.as_str(), expected_peer);
}

#[test]
fn the_golden_entropy_is_the_seed_not_a_derivation() {
    // ADR-0033: the entropy IS the Ed25519 secret. If a PBKDF2 step crept
    // in, this would still produce a valid identity — just a different
    // one — so the assertion is on the exact bytes.
    let f = fixture();
    let v = &f["vectors"][0];
    let entropy_hex = v["entropy_hex"].as_str().expect("entropy");
    let phrase = RecoveryPhrase::parse(v["mnemonic"].as_str().expect("mnemonic")).expect("parses");

    let entropy = phrase.expose_entropy().expect("32 bytes");
    let got: String = entropy.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(got, entropy_hex);
}

#[test]
fn an_identity_survives_a_restart_byte_for_byte() {
    // Stage 4's exit gate needs this, and it is a property of the file
    // format plus the loader — not of the library boundary the spike
    // measured.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");

    let original = ProfileIdentity::generate();
    let before = original.transport_identity().expect("peer id");
    let phrase_before = original.recovery_phrase().expect("phrase");
    original.save(&path).expect("save");
    drop(original);

    let reloaded = ProfileIdentity::load(&path).expect("load");
    assert_eq!(reloaded.transport_identity().expect("peer id"), before);
    assert_eq!(
        reloaded.recovery_phrase().expect("phrase"),
        phrase_before,
        "the same seed must come back, not merely a working key"
    );
}

#[test]
fn a_missing_key_is_an_error_and_never_a_new_identity() {
    // Silent regeneration hands the profile a new PeerId, invalidating
    // every trust relationship anyone had with it, and looks exactly like
    // a successful start.
    let dir = tempfile::tempdir().expect("tempdir");
    let err = ProfileIdentity::load(&dir.path().join("identity.key"))
        .expect_err("a missing key must not be a fresh identity");
    assert!(matches!(err, IdentityError::NotFound), "unexpected: {err}");
}

#[test]
#[cfg(unix)]
fn a_world_readable_key_is_refused_rather_than_repaired() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    ProfileIdentity::generate().save(&path).expect("save");
    assert!(ProfileIdentity::load(&path).is_ok());

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("loosen");
    let err = ProfileIdentity::load(&path).expect_err("a readable key must be refused");
    assert!(
        matches!(err, IdentityError::PermissionsTooOpen),
        "unexpected: {err}"
    );
    // And it is REFUSED, not quietly tightened: a key that has been
    // exposed should be treated as disclosed.
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o644, "loading must not silently change the mode");
}

#[test]
fn the_saved_key_is_owner_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    ProfileIdentity::generate().save(&path).expect("save");
    assert!(interweave_profile_config::is_owner_only(&path).expect("mode"));
}

#[test]
fn verify_is_read_only_and_fails_closed_on_the_wrong_phrase() {
    // A checksum-valid phrase for a DIFFERENT key is still checksum-
    // valid, so the checksum alone would let a wrong phrase read as
    // verified.
    let mine = ProfileIdentity::generate();
    let theirs = ProfileIdentity::generate();
    let my_id = mine.transport_identity().expect("peer id");
    let their_phrase = theirs.recovery_phrase().expect("phrase");

    let err = ProfileIdentity::verify_phrase(&their_phrase, &my_id)
        .expect_err("a phrase for another key must not verify");
    assert!(
        matches!(err, IdentityError::PeerIdMismatch { .. }),
        "unexpected: {err}"
    );

    // And the right one does.
    ProfileIdentity::verify_phrase(&mine.recovery_phrase().expect("phrase"), &my_id)
        .expect("the profile's own phrase verifies");
}

#[test]
fn verify_touches_no_file() {
    // The read-only half of recovery. A verification that failed must
    // leave the running identity exactly as it was.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    let identity = ProfileIdentity::generate();
    identity.save(&path).expect("save");
    let before = std::fs::read(&path).expect("read");

    let other = ProfileIdentity::generate()
        .recovery_phrase()
        .expect("phrase");
    let _ = ProfileIdentity::verify_phrase(&other, &identity.transport_identity().expect("id"));

    assert_eq!(std::fs::read(&path).expect("read"), before);
}

#[test]
fn a_restored_identity_round_trips_to_the_same_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    let original = ProfileIdentity::generate();
    original.save(&path).expect("save");
    let bytes_before = std::fs::read(&path).expect("read");

    let restored = ProfileIdentity::from_phrase(&original.recovery_phrase().expect("phrase"))
        .expect("restore");
    let path2 = dir.path().join("restored.key");
    restored.save(&path2).expect("save");

    assert_eq!(
        std::fs::read(&path2).expect("read"),
        bytes_before,
        "a restore must reproduce the identity byte-for-byte, not an equivalent one"
    );
}

#[test]
fn a_corrupt_key_file_is_an_error_not_a_new_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    interweave_profile_config::write_private_atomic(&path, b"not a protobuf").expect("write");
    let err = ProfileIdentity::load(&path).expect_err("corrupt must not silently regenerate");
    assert!(
        matches!(err, IdentityError::Corrupt(_)),
        "unexpected: {err}"
    );
}

#[test]
fn a_mistyped_phrase_is_caught_by_the_checksum() {
    let identity = ProfileIdentity::generate();
    let phrase = identity.recovery_phrase().expect("phrase");
    let words = phrase.expose_words();
    let mut parts: Vec<&str> = words.split_whitespace().collect();
    // Swap two words: still all valid BIP-39 words, still 24 of them.
    parts.swap(0, 1);
    let swapped = parts.join(" ");
    if swapped == words {
        return; // the first two words happened to be identical
    }
    assert!(
        RecoveryPhrase::parse(&swapped).is_err(),
        "a reordered phrase must fail its checksum"
    );
}

#[test]
fn a_phrase_of_the_wrong_length_is_refused() {
    let short = "abandon abandon abandon abandon abandon abandon abandon abandon abandon \
                 abandon abandon about";
    let err = RecoveryPhrase::parse(short).expect_err("12 words is not this format");
    assert!(
        matches!(
            err,
            IdentityError::WrongWordCount { got: 12, want: 24 } | IdentityError::Bip39(_)
        ),
        "unexpected: {err}"
    );
}

#[test]
fn no_secret_material_reaches_debug_output() {
    // ADR-0033 and IDENTITY.md: the phrase must never reach logs, crash
    // reports, or traces. A derived Debug puts it in all three.
    let identity = ProfileIdentity::generate();
    let phrase = identity.recovery_phrase().expect("phrase");
    let words = phrase.expose_words();
    let first_word = words.split_whitespace().next().expect("a word");

    let phrase_debug = format!("{phrase:?}");
    assert!(
        !phrase_debug.contains(first_word) || first_word.len() < 3,
        "the phrase reached Debug output: {phrase_debug}"
    );
    assert!(phrase_debug.contains("redacted"), "{phrase_debug}");

    // The identity prints its PeerId, which is public, and nothing else.
    let id_debug = format!("{identity:?}");
    for word in words.split_whitespace() {
        assert!(
            !id_debug.contains(&format!(" {word} ")),
            "identity Debug leaked a phrase word: {id_debug}"
        );
    }
    let peer = identity.transport_identity().expect("peer id");
    assert!(id_debug.contains(peer.as_str()), "{id_debug}");
}

#[test]
fn the_stored_file_is_not_the_mnemonic() {
    // The key file is the libp2p portable encoding, per IDENTITY.md. If
    // it were ever the words, the phrase would be sitting in a file the
    // recovery contract says it must never be written to.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    let identity = ProfileIdentity::generate();
    identity.save(&path).expect("save");

    let bytes = std::fs::read(&path).expect("read");
    let text = String::from_utf8_lossy(&bytes);
    let words = identity.recovery_phrase().expect("phrase").expose_words();
    for word in words.split_whitespace().take(4) {
        assert!(!text.contains(word), "the key file contains phrase words");
    }
}

#[test]
fn a_generated_identity_is_a_canonical_peer_id() {
    // The neutral contract's grammar and libp2p's output must agree; if
    // they did not, every other crate would be validating something
    // libp2p cannot produce.
    for _ in 0..32 {
        let id = ProfileIdentity::generate()
            .transport_identity()
            .expect("libp2p produced a PeerId the neutral grammar accepts");
        assert!(TransportIdentity::parse(id.as_str()).is_ok());
    }
}

// -------------------------------------------------------------------
// The backup record — the artifact `identity/recovery-record` describes
// -------------------------------------------------------------------

#[test]
fn a_record_round_trips_through_json_and_restores_the_same_identity() {
    let identity = ProfileIdentity::generate();
    let expected = identity.transport_identity().expect("peer id");

    let record = interweave_profile_identity::RecoveryRecord::of(&identity).expect("record");
    let json = serde_json::to_string(&record).expect("serializes");
    let parsed: interweave_profile_identity::RecoveryRecord =
        serde_json::from_str(&json).expect("deserializes");

    let restored = parsed.restore().expect("restores");
    assert_eq!(restored.transport_identity().expect("peer id"), expected);
}

#[test]
fn a_record_validates_against_the_frozen_schema() {
    // The flip from `approved` to `active` claims this record shape now
    // describes real files. This is what makes that claim checkable.
    let identity = ProfileIdentity::generate();
    let record = interweave_profile_identity::RecoveryRecord::of(&identity).expect("record");
    let value = serde_json::to_value(&record).expect("to value");

    assert_eq!(value["format"], "interweave-ed25519-bip39-entropy-v1");
    assert_eq!(value["identity_algorithm"], "ed25519");
    assert_eq!(
        value["words"].as_array().expect("array").len(),
        24,
        "exactly 24 words; the shorter BIP-39 lengths are refused for this format"
    );
    assert!(value["expected_peer_id"].is_string());
}

#[test]
fn a_record_naming_another_identity_is_refused() {
    // The check that turns a checksum test into an identity test. Without
    // it a restore would report success while replacing the profile with
    // a stranger.
    let mine = ProfileIdentity::generate();
    let theirs = ProfileIdentity::generate();

    let mut record = interweave_profile_identity::RecoveryRecord::of(&theirs).expect("record");
    record.expected_peer_id = Some(
        mine.transport_identity()
            .expect("peer id")
            .as_str()
            .to_owned(),
    );

    let err = record
        .restore()
        .expect_err("a mismatched record must not restore");
    assert!(
        matches!(err, IdentityError::PeerIdMismatch { .. }),
        "unexpected: {err}"
    );
}

#[test]
fn a_record_with_another_algorithm_is_refused_rather_than_converted() {
    let identity = ProfileIdentity::generate();
    let mut record = interweave_profile_identity::RecoveryRecord::of(&identity).expect("record");
    record.identity_algorithm = "secp256k1".to_owned();
    assert!(record.restore().is_err());
}

#[test]
fn a_record_with_an_unknown_format_is_refused() {
    let identity = ProfileIdentity::generate();
    let mut record = interweave_profile_identity::RecoveryRecord::of(&identity).expect("record");
    record.format = "some-other-format-v9".to_owned();
    assert!(record.restore().is_err());
}

#[test]
fn a_record_carries_no_words_into_debug_output() {
    let identity = ProfileIdentity::generate();
    let record = interweave_profile_identity::RecoveryRecord::of(&identity).expect("record");
    let printed = format!("{record:?}");
    for word in &record.words {
        assert!(
            !printed.contains(&format!("\"{word}\"")),
            "the record leaked a word into Debug: {printed}"
        );
    }
    assert!(printed.contains("redacted"), "{printed}");
}

#[test]
fn a_record_with_an_unknown_field_is_refused() {
    // `additionalProperties: false` in the schema. An unknown field in a
    // backup file is a file this build does not understand, and guessing
    // is the wrong response to that.
    let json = r#"{"format":"interweave-ed25519-bip39-entropy-v1",
        "identity_algorithm":"ed25519","words":[],"passphrase":"hunter2"}"#;
    assert!(serde_json::from_str::<interweave_profile_identity::RecoveryRecord>(json).is_err());
}

#[test]
fn saving_over_an_established_identity_is_refused() {
    // `write_private_atomic` renames over its target, so a save aimed at
    // an occupied path is a rotation wearing the name of a write: the
    // profile's persistent PeerId changes and every trust relationship
    // established against the old one stops resolving.
    //
    // This is the same failure `NotFound` exists to prevent — an
    // established profile silently acquiring a new identity — arriving
    // through the other door.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("identity.key");

    let established = ProfileIdentity::generate();
    established.save(&path).expect("the first save creates it");
    let established_peer = established.transport_identity().expect("peer id");

    let intruder = ProfileIdentity::generate();
    let refused = intruder.save(&path);
    assert!(
        matches!(refused, Err(IdentityError::AlreadyExists)),
        "a second save must be refused, got {refused:?}"
    );

    // Refused, not partially applied: the established key is still the
    // one on disk.
    let loaded = ProfileIdentity::load(&path).expect("still loads");
    assert_eq!(
        loaded.transport_identity().expect("peer id").as_str(),
        established_peer.as_str(),
        "the refused save must leave the established identity untouched"
    );
}

#[test]
fn replacing_an_identity_is_available_but_has_to_be_asked_for() {
    // Rotation is legitimate; it just cannot happen by accident, and it
    // cannot happen to a profile the caller has not actually read.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("identity.key");

    let established = ProfileIdentity::generate();
    established.save(&path).expect("first save");
    let established_peer = established.transport_identity().expect("peer id");

    let replacement = ProfileIdentity::generate();

    // Naming the wrong current identity means operating on a profile the
    // caller has not read. The answer is to stop, not to overwrite.
    let stranger = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    assert!(
        matches!(
            replacement.replace_saved(&path, &stranger),
            Err(IdentityError::PeerIdMismatch { .. })
        ),
        "a rotation must name the identity it is replacing"
    );
    assert_eq!(
        ProfileIdentity::load(&path)
            .expect("loads")
            .transport_identity()
            .expect("peer id")
            .as_str(),
        established_peer.as_str(),
        "and the refused rotation changed nothing"
    );

    let rotation = replacement
        .replace_saved(&path, &established_peer)
        .expect("an explicit replacement is allowed");

    // Both halves, because a rotation is only meaningful as a pair: the
    // old PeerId is what every existing trust relationship names.
    assert_eq!(rotation.previous.as_str(), established_peer.as_str());
    assert_eq!(
        rotation.current.as_str(),
        replacement.transport_identity().expect("peer id").as_str()
    );

    let loaded = ProfileIdentity::load(&path).expect("loads");
    assert_eq!(
        loaded.transport_identity().expect("peer id").as_str(),
        rotation.current.as_str(),
        "the replacement is what is now stored"
    );
}

#[test]
fn a_restore_must_name_the_profile_it_is_restoring() {
    // A checksum-valid phrase for a different key is still
    // checksum-valid. Without the comparison a restore installs a
    // stranger's identity and reports success — and a restore is exactly
    // the operation performed by someone who has lost their state and
    // cannot tell.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("identity.key");

    let original = ProfileIdentity::generate();
    let phrase = original.recovery_phrase().expect("phrase");
    let original_peer = original.transport_identity().expect("peer id");

    let stranger = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    assert!(
        matches!(
            ProfileIdentity::restore(&path, &phrase, &stranger),
            Err(IdentityError::PeerIdMismatch { .. })
        ),
        "a phrase reconstructing someone else must be refused"
    );
    assert!(
        !path.exists(),
        "and a refused restore must not have written anything"
    );

    let restored = ProfileIdentity::restore(&path, &phrase, &original_peer)
        .expect("the right phrase for the right profile");
    assert_eq!(
        restored.transport_identity().expect("peer id").as_str(),
        original_peer.as_str()
    );
    assert_eq!(
        ProfileIdentity::load(&path)
            .expect("loads")
            .transport_identity()
            .expect("peer id")
            .as_str(),
        original_peer.as_str(),
        "and it is on disk, not merely returned"
    );
}

#[test]
fn concurrent_creation_produces_exactly_one_winner() {
    // A check-then-write guard has a window: two processes initializing
    // the same profile both pass the check before either writes, and the
    // loser silently replaces the identity the winner established. That
    // is the failure the refusal exists to prevent, reintroduced by the
    // shape of the guard.
    //
    // Threads rather than processes because the guarantee has to come
    // from the filesystem operation either way — a check-then-write loses
    // this race regardless of what does the racing.
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("identity.key");

    const RACERS: usize = 8;
    let barrier = Arc::new(Barrier::new(RACERS));
    let mut handles = Vec::new();
    for _ in 0..RACERS {
        let barrier = Arc::clone(&barrier);
        let path = path.clone();
        handles.push(std::thread::spawn(move || {
            let identity = ProfileIdentity::generate();
            let peer = identity
                .transport_identity()
                .expect("peer id")
                .as_str()
                .to_owned();
            barrier.wait();
            identity.save(&path).map(|()| peer)
        }));
    }

    let mut winners = Vec::new();
    let mut refusals = 0;
    for h in handles {
        match h.join().expect("thread did not panic") {
            Ok(peer) => winners.push(peer),
            Err(IdentityError::AlreadyExists) => refusals += 1,
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    assert_eq!(
        winners.len(),
        1,
        "exactly one caller may create the identity"
    );
    assert_eq!(refusals, RACERS - 1, "every other caller is told it lost");

    // And the file on disk is the winner's, whole — not a blend of eight
    // writers that all believed they had an empty path.
    let loaded = ProfileIdentity::load(&path).expect("loads");
    assert_eq!(
        loaded.transport_identity().expect("peer id").as_str(),
        winners[0],
        "the stored identity is the one the winner wrote"
    );
}

#[test]
fn concurrent_rotation_produces_exactly_one_winner() {
    // The same window the creation race had, one operation over.
    // Reading the stored identity, checking it, and then writing lets two
    // processes both name the identity that really is stored, both pass
    // the check, and both report a successful rotation — while the later
    // write silently replaces the earlier one, leaving the first caller
    // reporting an identity that is no longer there.
    //
    // That is exactly the guarantee `replacing` exists to provide, so a
    // check that cannot hold it is worse than no check: it reads as one.
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("identity.key");

    let established = ProfileIdentity::generate();
    established.save(&path).expect("first save");
    let established_peer = established.transport_identity().expect("peer id");

    const RACERS: usize = 8;
    let barrier = Arc::new(Barrier::new(RACERS));
    let mut handles = Vec::new();
    for _ in 0..RACERS {
        let barrier = Arc::clone(&barrier);
        let path = path.clone();
        let replacing = established_peer.clone();
        handles.push(std::thread::spawn(move || {
            let replacement = ProfileIdentity::generate();
            barrier.wait();
            replacement.replace_saved(&path, &replacing)
        }));
    }

    let mut winners = Vec::new();
    let mut refused = 0;
    for h in handles {
        match h.join().expect("thread did not panic") {
            Ok(rotation) => winners.push(rotation),
            // Either it could not take the marker, or it took it after
            // the winner and found an identity it had not named. Both are
            // a refusal; neither is a second rotation.
            Err(
                IdentityError::RotationInProgress { .. } | IdentityError::PeerIdMismatch { .. },
            ) => {
                refused += 1;
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    assert_eq!(winners.len(), 1, "exactly one rotation may succeed");
    assert_eq!(
        refused,
        RACERS - 1,
        "and every other caller is told it lost"
    );

    // The winner's report is TRUE: what it says is stored really is.
    let rotation = &winners[0];
    assert_eq!(rotation.previous.as_str(), established_peer.as_str());
    assert_eq!(
        ProfileIdentity::load(&path)
            .expect("loads")
            .transport_identity()
            .expect("peer id")
            .as_str(),
        rotation.current.as_str(),
        "the successful rotation must describe the identity actually on disk"
    );

    // And the marker is released, so the profile is not wedged.
    assert!(
        !dir.path().join("identity.key.rotating").exists(),
        "a completed rotation must not leave its marker behind"
    );
    let next = ProfileIdentity::generate();
    next.replace_saved(&path, &rotation.current)
        .expect("a later rotation still works");
}

#[test]
fn an_interrupted_rotation_is_reported_rather_than_ignored() {
    // A marker left by a rotation that died is indistinguishable from one
    // held right now, so both are reported. Removing it is a person's
    // decision, which is the right amount of friction for an operation
    // that invalidates every trust relationship.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("identity.key");

    let established = ProfileIdentity::generate();
    established.save(&path).expect("save");
    let established_peer = established.transport_identity().expect("peer id");

    let marker = dir.path().join("identity.key.rotating");
    std::fs::hard_link(&path, &marker).expect("simulate an interrupted rotation");

    let replacement = ProfileIdentity::generate();
    match replacement.replace_saved(&path, &established_peer) {
        Err(IdentityError::RotationInProgress { marker: reported }) => {
            assert_eq!(reported, marker, "the error names what has to be removed");
        }
        other => panic!("expected RotationInProgress, got {other:?}"),
    }

    // Nothing changed, and clearing the marker restores the operation.
    assert_eq!(
        ProfileIdentity::load(&path)
            .expect("loads")
            .transport_identity()
            .expect("peer id")
            .as_str(),
        established_peer.as_str()
    );
    std::fs::remove_file(&marker).expect("clear it");
    replacement
        .replace_saved(&path, &established_peer)
        .expect("rotation works once the marker is gone");
}
