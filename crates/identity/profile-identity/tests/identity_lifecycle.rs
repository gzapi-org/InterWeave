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
    let path = dir.path().join("state").join("identity.key");

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
    let err = ProfileIdentity::load(&dir.path().join("state").join("identity.key"))
        .expect_err("a missing key must not be a fresh identity");
    assert!(matches!(err, IdentityError::NotFound), "unexpected: {err}");
}

#[test]
#[cfg(unix)]
fn a_world_readable_key_is_refused_rather_than_repaired() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("identity.key");
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
    let path = dir.path().join("state").join("identity.key");
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
    let path = dir.path().join("state").join("identity.key");
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
    let path = dir.path().join("state").join("identity.key");
    let original = ProfileIdentity::generate();
    original.save(&path).expect("save");
    let bytes_before = std::fs::read(&path).expect("read");

    let restored = ProfileIdentity::from_phrase(&original.recovery_phrase().expect("phrase"))
        .expect("restore");
    let path2 = dir.path().join("state").join("restored.key");
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
    let path = dir.path().join("state").join("identity.key");
    interweave_profile_config::write_private_atomic(&path, b"not a protobuf").expect("write");
    let err = ProfileIdentity::load(&path).expect_err("corrupt must not silently regenerate");
    assert!(
        matches!(err, IdentityError::Corrupt(_)),
        "unexpected: {err}"
    );
}

/// The checksum catches a transposition, on a phrase that cannot vary.
///
/// This used to be asserted against a freshly GENERATED phrase, and the
/// assertion was false: a 24-word phrase carries an 8-bit checksum, so a
/// transposition survives it about one time in 256. Measured at 78 of
/// 20,000. The test therefore failed roughly four runs in a thousand,
/// which is rare enough to read as noise and frequent enough to dequeue
/// a pull request — which is how it was found.
///
/// The frozen golden vector removes the variance. `abandon` twenty-three
/// times plus `art` is the standard all-zero BIP-39 vector, so swapping
/// the ends is a fixed input with a fixed answer.
#[test]
fn the_checksum_catches_a_transposition() {
    let f = fixture();
    let mnemonic = f["vectors"][0]["mnemonic"].as_str().expect("mnemonic");
    let mut parts: Vec<&str> = mnemonic.split_whitespace().collect();
    let last = parts.len() - 1;
    parts.swap(0, last);
    let swapped = parts.join(" ");

    assert_ne!(
        swapped, mnemonic,
        "the swap must actually change the phrase"
    );
    assert!(
        RecoveryPhrase::parse(&swapped).is_err(),
        "this transposition of the golden phrase must fail its checksum"
    );
}

/// The one-in-256 path where a transposition survives the checksum.
///
/// What is load-bearing here is that the path RUNS: a phrase whose
/// checksum happens to validate after a transposition must reconstruct
/// cleanly rather than error or panic, and no other test in this suite
/// reaches that branch. Breaking reconstruction on it reddens this and
/// nothing else.
///
/// The `assert_ne!` is deliberately kept but is NOT the point, and it is
/// worth saying so rather than letting it read as the guarantee: two
/// different entropies give two different Ed25519 keys by construction,
/// so it asserts keygen injectivity. It stays because it documents what
/// the surviving case means for the owner — a mistype reconstructs SOME
/// identity, just not theirs.
///
/// The surviving case is SEARCHED FOR rather than waited for. Asserting
/// only when a random phrase happens to collide would leave the
/// assertion asleep in 255 runs out of 256, which is a test that mostly
/// does nothing while reading as coverage. At one in 256 the search
/// takes a few hundred generations and finding nothing in ten thousand
/// is a probability of about 3e-17, so the bound cannot flake.
#[test]
fn a_transposition_that_survives_the_checksum_is_a_different_identity() {
    let mut examined = 0;
    for _ in 0..10_000 {
        let identity = ProfileIdentity::generate();
        let phrase = identity.recovery_phrase().expect("phrase");
        let words = phrase.expose_words();
        let mut parts: Vec<&str> = words.split_whitespace().collect();
        // Still all valid BIP-39 words, still 24 of them.
        parts.swap(0, 1);
        let swapped = parts.join(" ");
        if swapped == words {
            continue; // the first two words happened to be identical
        }
        let Ok(other) = RecoveryPhrase::parse(&swapped) else {
            continue; // caught by the checksum, which is the common case
        };

        examined += 1;
        let rebuilt = ProfileIdentity::from_phrase(&other).expect("a parsed phrase reconstructs");
        assert_ne!(
            rebuilt.transport_identity().expect("peer id").as_str(),
            identity.transport_identity().expect("peer id").as_str(),
            "a transposed phrase reconstructed the SAME identity"
        );
        break;
    }
    assert_eq!(
        examined, 1,
        "no transposition survived the checksum in ten thousand attempts, \
         which at one in 256 should be impossible — the checksum or the \
         generator has changed"
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

    // EXACT, not "does not contain a word".
    //
    // A substring test against the redaction text is unsound, because the
    // redaction text is English and so is the BIP-39 wordlist: `act` is a
    // word and `redacted` contains it, `word` is a word and `words`
    // contains it. That made this assertion fail roughly once in a
    // thousand runs on the phrase alone, for a phrase that had leaked
    // nothing — a flake that reads as a security failure, which is the
    // worst kind to hand someone at 3am.
    //
    // The real contract is stronger and simpler anyway: this Debug prints
    // one fixed string with no phrase-derived content in it at all.
    // Pinning that makes leakage unrepresentable rather than unlikely.
    let phrase_debug = format!("{phrase:?}");
    assert_eq!(
        phrase_debug, "RecoveryPhrase(<24 words redacted>)",
        "the phrase Debug must be a fixed redaction and nothing else"
    );

    // Exact here too, and for a sharper version of the same reason: the
    // type's own name contains a BIP-39 word (`ProfileIdentity` contains
    // `file`), so a substring scan over 24 words would fail on about one
    // identity in eighty while nothing had leaked.
    //
    // Naming the whole output states the contract instead of sampling
    // it: the PeerId, which is public by construction, and nothing else.
    let peer = identity.transport_identity().expect("peer id");
    let id_debug = format!("{identity:?}");
    assert_eq!(
        id_debug,
        format!("ProfileIdentity({})", peer.as_str()),
        "the identity Debug must be its PeerId and nothing else"
    );
}

#[test]
fn the_stored_file_is_not_the_mnemonic() {
    // The key file is the libp2p portable encoding, per IDENTITY.md. If
    // it were ever the words, the phrase would be sitting in a file the
    // recovery contract says it must never be written to.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("identity.key");
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
    let path = dir.path().join("state").join("identity.key");

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
    let path = dir.path().join("state").join("identity.key");

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
    let path = dir.path().join("state").join("identity.key");

    let original = ProfileIdentity::generate();
    let phrase = original.recovery_phrase().expect("phrase");
    let original_peer = original.transport_identity().expect("peer id");

    let stranger = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    assert!(
        matches!(
            ProfileIdentity::restore_new(&path, &phrase, &stranger),
            Err(IdentityError::PeerIdMismatch { .. })
        ),
        "a phrase reconstructing someone else must be refused"
    );
    assert!(
        !path.exists(),
        "and a refused restore must not have written anything"
    );

    let restored = ProfileIdentity::restore_new(&path, &phrase, &original_peer)
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
    let path = dir.path().join("state").join("identity.key");

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
    let path = dir.path().join("state").join("identity.key");

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
        !dir.path()
            .join("state")
            .join("identity.key.rotating")
            .exists(),
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
    let path = dir.path().join("state").join("identity.key");

    let established = ProfileIdentity::generate();
    established.save(&path).expect("save");
    let established_peer = established.transport_identity().expect("peer id");

    let marker = dir.path().join("state").join("identity.key.rotating");
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

#[test]
fn restore_and_rotation_exclude_each_other() {
    // Rotation took the marker and restore wrote straight to the path, so
    // the two could interleave on the same file with no exclusion between
    // them. A restore landing inside a rotation is either lost, or
    // replaces the identity that rotation's `Rotation.current` says is
    // stored — which makes the compare-and-swap a guarantee against other
    // rotations rather than a guarantee about the file, and a caller
    // cannot tell those apart from outside.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("state").join("identity.key");

    let established = ProfileIdentity::generate();
    established.save(&path).expect("save");
    let established_peer = established.transport_identity().expect("peer id");

    let other = ProfileIdentity::generate();
    let phrase = other.recovery_phrase().expect("phrase");
    let other_peer = other.transport_identity().expect("peer id");

    // With the marker held, a restore over an existing identity is
    // refused rather than racing.
    let marker = dir.path().join("state").join("identity.key.rotating");
    std::fs::hard_link(&path, &marker).expect("hold the marker");
    assert!(
        matches!(
            ProfileIdentity::restore_replace(&path, &phrase, &other_peer, &established_peer),
            Err(IdentityError::RotationInProgress { .. })
        ),
        "a restore must not overwrite a profile mid-rotation"
    );
    assert_eq!(
        ProfileIdentity::load(&path)
            .expect("loads")
            .transport_identity()
            .expect("peer id")
            .as_str(),
        established_peer.as_str(),
        "and the identity it would have replaced is untouched"
    );

    std::fs::remove_file(&marker).expect("release");
    ProfileIdentity::restore_replace(&path, &phrase, &other_peer, &established_peer)
        .expect("restore once nothing holds it");
    assert_eq!(
        ProfileIdentity::load(&path)
            .expect("loads")
            .transport_identity()
            .expect("peer id")
            .as_str(),
        other_peer.as_str()
    );
    assert!(
        !marker.exists(),
        "a completed restore must not leave the marker behind"
    );
}

#[test]
fn a_restore_cannot_replace_an_established_profile_without_naming_it() {
    // The defect the split exists for: `restore` took the rotation marker
    // for exclusion and then ignored it, so it never read what was
    // stored. A valid phrase for B, with B as the expected identity,
    // therefore installed B over an established A and reported success --
    // which `IDENTITY-RECOVERY.md` item 8 refuses and the sentence beside
    // it names directly: a restore "must not overwrite an established
    // profile automatically". The old test for this started from an EMPTY
    // destination, so it could not reach the case. Review finding.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("state").join("identity.key");

    let established = ProfileIdentity::generate();
    established.save(&path).expect("establish A");
    let established_peer = established.transport_identity().expect("peer id");

    let incoming = ProfileIdentity::generate();
    let phrase = incoming.recovery_phrase().expect("phrase");
    let incoming_peer = incoming.transport_identity().expect("peer id");

    let survives = |what: &str| {
        assert_eq!(
            ProfileIdentity::load(&path)
                .expect("loads")
                .transport_identity()
                .expect("peer id")
                .as_str(),
            established_peer.as_str(),
            "{what}"
        );
    };

    // A phrase that reconstructs exactly what it claims, over an
    // established profile, through the creation path.
    assert!(
        matches!(
            ProfileIdentity::restore_new(&path, &phrase, &incoming_peer),
            Err(IdentityError::AlreadyExists)
        ),
        "a restore must not overwrite an established profile"
    );
    survives("and the established identity is untouched");

    // The replace path, naming the WRONG old identity.
    let stranger = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    assert!(
        matches!(
            ProfileIdentity::restore_replace(&path, &phrase, &incoming_peer, &stranger),
            Err(IdentityError::PeerIdMismatch { .. })
        ),
        "replacing must name the identity actually stored"
    );
    survives("and a refused replacement changes nothing");

    // Naming it correctly is the operator's explicit choice, and works.
    let (restored, rotation) =
        ProfileIdentity::restore_replace(&path, &phrase, &incoming_peer, &established_peer)
            .expect("naming what is stored is the documented path");
    assert_eq!(rotation.previous.as_str(), established_peer.as_str());
    assert_eq!(rotation.current.as_str(), incoming_peer.as_str());
    assert_eq!(
        restored.transport_identity().expect("peer id").as_str(),
        incoming_peer.as_str()
    );
    assert_eq!(
        ProfileIdentity::load(&path)
            .expect("loads")
            .transport_identity()
            .expect("peer id")
            .as_str(),
        incoming_peer.as_str(),
        "and the replacement is what is stored afterwards"
    );
}

#[test]
fn a_restore_into_an_empty_profile_is_a_creation() {
    // Nothing to exclude and nothing to replace: the common case is a
    // person who has lost everything, and requiring a key to already be
    // there would make restore useless exactly when it is needed.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("state").join("identity.key");

    let original = ProfileIdentity::generate();
    let phrase = original.recovery_phrase().expect("phrase");
    let peer = original.transport_identity().expect("peer id");

    ProfileIdentity::restore_new(&path, &phrase, &peer).expect("restore onto an empty profile");
    assert_eq!(
        ProfileIdentity::load(&path)
            .expect("loads")
            .transport_identity()
            .expect("peer id")
            .as_str(),
        peer.as_str()
    );
    assert!(
        !dir.path()
            .join("state")
            .join("identity.key.rotating")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_key_path_is_refused_not_followed() {
    // `exists` and the permission check both follow links, so a link
    // pointing at another account's mode-0600 file passed both and the
    // key was read from wherever it pointed.
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let elsewhere = dir.path().join("elsewhere.key");
    std::fs::write(&elsewhere, b"not a key").expect("target exists");
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600))
        .expect("owner-only target");

    // A PRIVATE PARENT, because `load` now requires one -- the real
    // profile layout puts the key in a 0700 state directory and a
    // tempdir root is whatever the umask gave it.
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("mkdir");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let link = state.join("identity.key");
    std::os::unix::fs::symlink(&elsewhere, &link).expect("link created");

    assert!(
        matches!(ProfileIdentity::load(&link), Err(IdentityError::NotAFile)),
        "a symlinked key path is refused rather than followed"
    );
}

#[cfg(unix)]
#[test]
fn a_key_in_a_directory_others_can_write_is_refused() {
    // `load` asked every question BY PATHNAME -- `symlink_metadata`, then
    // `is_owner_only`'s own `metadata`, then `read` -- so an entry swapped
    // between them let the checks inspect the legitimate mode-0600 key and
    // the read take something else. The parent check is what makes that
    // swap impossible rather than merely detectable: someone who cannot
    // write the directory cannot replace the entry at all. The private
    // writes in `persistence` already require this of the directory they
    // write INTO, and a private key should not be read under weaker terms
    // than it was written. Review finding.
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let path = state.join("identity.key");
    ProfileIdentity::generate().save(&path).expect("save");
    ProfileIdentity::load(&path).expect("loads from the directory save created");

    // Permission drift on the directory, with the key itself untouched.
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o777)).expect("widen");
    assert!(
        matches!(ProfileIdentity::load(&path), Err(IdentityError::Storage(_))),
        "a key whose directory anyone can write is refused"
    );

    // And tightening it again is enough; nothing about the key changed.
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).expect("tighten");
    ProfileIdentity::load(&path).expect("loads again once the directory is private");
}

#[cfg(unix)]
#[test]
fn an_oversized_key_file_is_refused_before_it_is_read() {
    // `read` sized its buffer from the file, so a local oversized file
    // could exhaust memory before the decoder rejected it. Nothing
    // legitimate approaches the ceiling: a protobuf Ed25519 keypair is
    // well under a hundred bytes.
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("mkdir");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = state.join("identity.key");
    std::fs::write(&path, vec![0u8; 64 * 1024]).expect("oversized file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("owner-only");

    match ProfileIdentity::load(&path) {
        Err(IdentityError::Corrupt(detail)) => assert!(
            detail.contains("maximum"),
            "the refusal names the ceiling: {detail}"
        ),
        other => panic!("an oversized key file must be refused: {other:?}"),
    }
}

/// A reader that counts what was actually pulled from it.
///
/// `serde_json::from_str` parses a document the caller has already
/// materialised, so it cannot tell early refusal from late. Reading
/// through this instead makes "stopped before the end" a measurement.
struct Counting<'a> {
    bytes: &'a [u8],
    read: std::cell::Cell<usize>,
}

impl std::io::Read for &Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let start = self.read.get();
        let n = (self.bytes.len() - start).min(buf.len()).min(64);
        buf[..n].copy_from_slice(&self.bytes[start..start + n]);
        self.read.set(start + n);
        Ok(n)
    }
}

/// A record document whose `words` array has `count` entries.
fn record_json_with_words(count: usize) -> String {
    let words: Vec<String> = (0..count).map(|_| "\"abandon\"".to_owned()).collect();
    format!(
        "{{\"format\":\"interweave-ed25519-bip39-entropy-v1\",\
          \"identity_algorithm\":\"ed25519\",\
          \"words\":[{}]}}",
        words.join(",")
    )
}

#[test]
fn a_record_claiming_a_million_words_is_refused_before_they_are_read() {
    // `validate()` checks the word count, but it runs after Serde has
    // already built the whole `Vec<String>`. A local recovery file
    // claiming a million words was therefore allocated in full and then
    // refused. Everything else in this repository bounds before it
    // allocates; the deserializer now does too.
    let text = record_json_with_words(1_000_000);
    let reader = Counting {
        bytes: text.as_bytes(),
        read: std::cell::Cell::new(0),
    };

    let result: Result<interweave_profile_identity::RecoveryRecord, _> =
        serde_json::from_reader(&reader);
    let error = result.expect_err("a million words is refused");
    assert!(
        error.to_string().contains("more than 24"),
        "the refusal names the ceiling: {error}"
    );

    // THE POINT OF THE TEST: it gave up near the start of the array
    // rather than consuming the document. 64 bytes is this reader's chunk
    // size, so the bound is generous by two orders of magnitude and still
    // a tiny fraction of the ten million bytes on offer.
    let consumed = reader.read.get();
    assert!(
        consumed < 4096,
        "refusal must precede the allocation: read {consumed} of {} bytes",
        text.len()
    );
}

#[test]
fn a_record_with_twenty_five_words_is_refused() {
    // One past the ceiling, to pin the boundary rather than only the
    // absurd case: a 25-word array is what an off-by-one writer produces.
    let text = record_json_with_words(PHRASE_WORDS_IN_TEST + 1);
    let error = serde_json::from_str::<interweave_profile_identity::RecoveryRecord>(&text)
        .expect_err("25 words is refused");
    assert!(
        error.to_string().contains("more than 24"),
        "the refusal names the ceiling: {error}"
    );
}

#[test]
fn a_four_word_record_deserializes_and_validate_refuses_it_by_count() {
    // THE SHORT DIRECTION, which nothing covered. The deserializer's bound is
    // AT MOST 24, so a four-word array gets past it by design and the count is
    // settled downstream.
    //
    // WHAT THIS PINS is `RecoveryRecord::validate`'s own length check:
    // deleting that block makes the first assertion fail. Measured.
    //
    // WHAT IT ONLY ASSERTS is the `restore` outcome. For a FOUR-word phrase
    // `bip39` refuses the joined string inside `RecoveryPhrase::parse` before
    // `parse`'s own count check is reached, so `restore` still errs with
    // either in-tree check removed -- so nothing here pins it, and the
    // assertion stands as a statement of the behaviour callers get rather
    // than as a guard. `parse`'s check is pinned by
    // `a_phrase_of_the_wrong_length_is_refused`, which feeds twelve words: a
    // count `bip39` accepts.
    //
    // Three reviews were needed for this paragraph, each correcting the last:
    // the doc claimed "fail-closed even if `validate` is called without the
    // other" with no test at all; then this comment named a mutation for the
    // `restore` assertion that no in-tree change can produce; then it said
    // that assertion would go red if `restore` stopped consulting either
    // path, which is false for either path taken on its own. The claims here
    // are now the two that were measured, and no more. Review findings on
    // PR #86.
    let text = record_json_with_words(SHORT_WORDS_IN_TEST);
    let record = serde_json::from_str::<interweave_profile_identity::RecoveryRecord>(&text)
        .expect("four words deserializes -- the bound is a ceiling, not an equality");
    assert!(
        matches!(
            record.validate(),
            Err(interweave_profile_identity::IdentityError::WrongWordCount {
                got: SHORT_WORDS_IN_TEST,
                want: PHRASE_WORDS_IN_TEST
            })
        ),
        "validate must refuse a short phrase by count: {:?}",
        record.validate()
    );
    assert!(
        record.restore().is_err(),
        "restore stays fail-closed for a short phrase -- held by `bip39` even \
         with both InterWeave checks removed, so this is a floor and not a pin"
    );
}

#[test]
fn a_record_with_exactly_twenty_four_words_still_deserializes() {
    // The control. A bound that refuses the legitimate document is not a
    // fix, and a recovery record is read by someone who has already lost
    // something.
    let text = record_json_with_words(PHRASE_WORDS_IN_TEST);
    let record = serde_json::from_str::<interweave_profile_identity::RecoveryRecord>(&text)
        .expect("24 words deserializes");
    // Not a valid phrase -- 24 copies of one word fails the checksum --
    // but it got past the deserializer, which is what this asserts.
    assert!(record.restore().is_err(), "the checksum still applies");
}

#[test]
fn a_record_word_longer_than_the_wordlist_is_refused_before_it_is_kept() {
    // The count is not the only unbounded dimension: 24 words of a
    // megabyte each also satisfies the length check.
    // Twenty-four of them, so the measurement below can distinguish
    // "refused at the first" from "read them all and then refused".
    let long = "a".repeat(64 * 1024);
    let words: Vec<String> = (0..PHRASE_WORDS_IN_TEST)
        .map(|_| format!("\"{long}\""))
        .collect();
    let text = format!(
        "{{\"format\":\"interweave-ed25519-bip39-entropy-v1\",\
          \"identity_algorithm\":\"ed25519\",\
          \"words\":[{}]}}",
        words.join(",")
    );
    let reader = Counting {
        bytes: text.as_bytes(),
        read: std::cell::Cell::new(0),
    };
    let result: Result<interweave_profile_identity::RecoveryRecord, _> =
        serde_json::from_reader(&reader);
    let error = result.expect_err("an oversized word is refused");
    assert!(
        error.to_string().contains("English BIP-39 wordlist"),
        "the refusal names the wordlist: {error}"
    );
    // Serde hands a visitor the whole string token, so the parser
    // necessarily reads the FIRST oversized word -- that is unavoidable
    // and not what this checks. What it checks is that the refusal comes
    // there rather than after all twenty-four have been retained, which
    // is the difference between 64 KiB and 1.5 MiB of live allocation.
    let consumed = reader.read.get();
    assert!(
        consumed < 2 * 64 * 1024,
        "refusal must land on the first word: read {consumed} of {} bytes",
        text.len()
    );
}

#[test]
fn a_record_label_longer_than_any_legal_value_is_refused() {
    // `format` and `identity_algorithm` are compared against constants of
    // 35 and 7 bytes, so a megabyte label is already wrong -- the only
    // question was whether it was copied before being refused.
    let long = "z".repeat(1024 * 1024);
    let text = format!(
        "{{\"format\":\"{long}\",\"identity_algorithm\":\"ed25519\",\
          \"words\":[]}}"
    );
    let error = serde_json::from_str::<interweave_profile_identity::RecoveryRecord>(&text)
        .expect_err("an oversized label is refused");
    assert!(
        error
            .to_string()
            .contains("cannot be any value this format defines"),
        "the refusal explains itself: {error}"
    );
}

/// 24, restated here rather than imported.
///
/// `PHRASE_WORDS` is crate-private, and an integration test asserting a
/// boundary should not take the boundary from the code it is checking.
const PHRASE_WORDS_IN_TEST: usize = 24;

/// A word count far enough below the ceiling that no off-by-one reading of
/// the bound could accept it.
const SHORT_WORDS_IN_TEST: usize = 4;

#[test]
fn an_oversized_expected_peer_id_is_refused_before_it_is_kept() {
    // The fourth string-bearing field, missed by the pass that bounded the
    // other three. `validate` refuses it through `TransportIdentity::parse`
    // — but that checks the ceiling after Serde has already copied the
    // value, which is the whole defect that pass existed to close.
    let long = "Q".repeat(1024 * 1024);
    let words: Vec<String> = (0..PHRASE_WORDS_IN_TEST)
        .map(|_| "\"abandon\"".to_owned())
        .collect();
    let text = format!(
        "{{\"format\":\"interweave-ed25519-bip39-entropy-v1\",\
          \"identity_algorithm\":\"ed25519\",\
          \"expected_peer_id\":\"{long}\",\
          \"words\":[{}]}}",
        words.join(",")
    );
    let error = serde_json::from_str::<interweave_profile_identity::RecoveryRecord>(&text)
        .expect_err("an oversized peer id is refused");
    assert!(
        error.to_string().contains("cannot be one: the ceiling is"),
        "the refusal names the ceiling: {error}"
    );
}

#[test]
fn an_explicit_null_expected_peer_id_is_still_refused() {
    // The control for the rewrite above: bounding the field must not turn
    // an explicit `null` into "absent", because absence means the record
    // was written with no check while `null` means someone emptied the
    // check — and reading the second as the first silently downgrades a
    // checked record to an unchecked one.
    let words: Vec<String> = (0..PHRASE_WORDS_IN_TEST)
        .map(|_| "\"abandon\"".to_owned())
        .collect();
    let text = format!(
        "{{\"format\":\"interweave-ed25519-bip39-entropy-v1\",\
          \"identity_algorithm\":\"ed25519\",\
          \"expected_peer_id\":null,\
          \"words\":[{}]}}",
        words.join(",")
    );
    let error = serde_json::from_str::<interweave_profile_identity::RecoveryRecord>(&text)
        .expect_err("an explicit null is refused");
    assert!(
        error.to_string().contains("omitted entirely, not null"),
        "and it says why: {error}"
    );
}

#[test]
fn an_omitted_expected_peer_id_is_still_absent_rather_than_an_error() {
    // The other half of the control: omission must keep working, or every
    // record written without a peer-id check becomes unreadable.
    let words: Vec<String> = (0..PHRASE_WORDS_IN_TEST)
        .map(|_| "\"abandon\"".to_owned())
        .collect();
    let text = format!(
        "{{\"format\":\"interweave-ed25519-bip39-entropy-v1\",\
          \"identity_algorithm\":\"ed25519\",\
          \"words\":[{}]}}",
        words.join(",")
    );
    let record = serde_json::from_str::<interweave_profile_identity::RecoveryRecord>(&text)
        .expect("an omitted peer id deserializes");
    assert!(
        record.expected_peer_id.is_none(),
        "and it reads as absent, not as a value"
    );
}
