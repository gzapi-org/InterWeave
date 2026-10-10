// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The identity at rest (plan §20 step 6), on the host over real files,
//! with a software AES-256-GCM `SeedCipher` standing where the device's
//! Keystore key stands: the exact fixture seed round-trips with its frozen
//! `PeerId`; every refusal -- a flipped bit, a wrong length, another key,
//! another identity, an invalidated or missing key -- gives no identity
//! back, leaves the record byte for byte as it was, and seals nothing, so
//! no new key is minted over the profile. Each refusal runs beside the
//! unlock that succeeds on the same record.
//!
//! What this cannot show is the Keystore: the adapter implementing the
//! seam, and its instrumented test, are the app's (§20 "Keystore",
//! carried by name in the PR).

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use interweave_profile_config::runtime::KeyUnlockPolicy;
use interweave_profile_config::{ProfilePaths, TrustBoundary};
use interweave_profile_identity::{ProfileIdentity, RecoveryPhrase};
use interweave_transport_api::TransportIdentity;
use interweave_transport_embedded::custody::{
    self, CipherFailure, CustodyRefused, ENVELOPE_LEN, IV_LEN, RecordRefused, RecoveryCause,
    SEALED_LEN, Sealed, Seed, SeedCipher, UnlockRefused,
};

const PROFILE: &str = "work";
const POLICIES: [KeyUnlockPolicy; 2] = [
    KeyUnlockPolicy::BackgroundCompatible,
    KeyUnlockPolicy::UserPresence,
];

/// The frozen identity vectors (TEST-ONLY public material): each
/// vector's mnemonic and the `PeerId` it must derive.
fn vectors() -> Vec<(String, String)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/identity/ed25519-bip39-entropy-v1.json");
    let fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("the fixture")).expect("json");
    fixture["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .map(|v| {
            (
                v["mnemonic"].as_str().expect("mnemonic").to_owned(),
                v["expected_peer_id"].as_str().expect("peer").to_owned(),
            )
        })
        .collect()
}

/// The frozen vector and a generated identity beside it, as (mnemonic,
/// `PeerId`): the second identity the swaps and refusals need, since the
/// fixture freezes one.
fn two() -> Vec<(String, String)> {
    let mut both = vectors();
    both.truncate(1);
    let other = ProfileIdentity::generate();
    both.push((
        other.recovery_phrase().expect("phrase").expose_words(),
        peer_of(&other),
    ));
    both
}

fn identity(mnemonic: &str) -> ProfileIdentity {
    ProfileIdentity::from_phrase(&RecoveryPhrase::parse(mnemonic).expect("phrase")).expect("id")
}

/// The device's Keystore, in software: one AES-256-GCM key per policy,
/// a NEW one on every seal as the seam requires, the IV the cipher's own.
/// `fail` makes `open` answer as the platform would on that failure.
#[derive(Default)]
struct SoftCipher {
    keys: Mutex<HashMap<u8, [u8; 32]>>,
    seals: AtomicUsize,
    fail: Mutex<Option<CipherFailure>>,
}

fn slot(policy: KeyUnlockPolicy) -> u8 {
    match policy {
        KeyUnlockPolicy::BackgroundCompatible => 0,
        KeyUnlockPolicy::UserPresence => 1,
    }
}

impl SoftCipher {
    fn fail_with(&self, failure: Option<CipherFailure>) {
        *self.fail.lock().unwrap() = failure;
    }
    fn forget(&self, policy: KeyUnlockPolicy) {
        self.keys.lock().unwrap().remove(&slot(policy));
    }
    fn seals(&self) -> usize {
        self.seals.load(Ordering::SeqCst)
    }
}

impl SeedCipher for SoftCipher {
    fn seal(
        &self,
        policy: KeyUnlockPolicy,
        aad: &[u8],
        seed: &Seed,
    ) -> Result<Sealed, CipherFailure> {
        self.seals.fetch_add(1, Ordering::SeqCst);
        let key: [u8; 32] = rand::random();
        self.keys.lock().unwrap().insert(slot(policy), key);
        let iv: [u8; IV_LEN] = rand::random();
        let sealed = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key))
            .encrypt(
                Nonce::from_slice(&iv),
                Payload {
                    msg: seed.expose(),
                    aad,
                },
            )
            .map_err(|_| CipherFailure::Unavailable("encrypt".to_owned()))?;
        Ok(Sealed {
            iv: iv.to_vec(),
            sealed,
        })
    }

    fn open(
        &self,
        policy: KeyUnlockPolicy,
        iv: &[u8; IV_LEN],
        sealed: &[u8; SEALED_LEN],
        aad: &[u8],
    ) -> Result<Seed, CipherFailure> {
        if let Some(failure) = self.fail.lock().unwrap().clone() {
            return Err(failure);
        }
        let key = *self
            .keys
            .lock()
            .unwrap()
            .get(&slot(policy))
            .ok_or(CipherFailure::KeyMissing)?;
        let plain = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key))
            .decrypt(Nonce::from_slice(iv), Payload { msg: sealed, aad })
            .map_err(|_| CipherFailure::Authentication)?;
        Ok(Seed::new(plain.try_into().expect("32 bytes")))
    }
}

/// A cipher that opens anything to a fixed seed: an envelope that
/// "authenticates" and derives another identity.
struct Substituting(Seed);

impl SeedCipher for Substituting {
    fn seal(&self, _: KeyUnlockPolicy, _: &[u8], _: &Seed) -> Result<Sealed, CipherFailure> {
        Err(CipherFailure::Unavailable("not used".to_owned()))
    }
    fn open(
        &self,
        _: KeyUnlockPolicy,
        _: &[u8; IV_LEN],
        _: &[u8; SEALED_LEN],
        _: &[u8],
    ) -> Result<Seed, CipherFailure> {
        Ok(Seed::new(*self.0.expose()))
    }
}

/// A cipher returning an IV or sealed bytes of another length.
struct Misshapen {
    iv: usize,
    sealed: usize,
}

impl SeedCipher for Misshapen {
    fn seal(&self, _: KeyUnlockPolicy, _: &[u8], _: &Seed) -> Result<Sealed, CipherFailure> {
        Ok(Sealed {
            iv: vec![0; self.iv],
            sealed: vec![0; self.sealed],
        })
    }
    fn open(
        &self,
        _: KeyUnlockPolicy,
        _: &[u8; IV_LEN],
        _: &[u8; SEALED_LEN],
        _: &[u8],
    ) -> Result<Seed, CipherFailure> {
        Err(CipherFailure::Unavailable("not used".to_owned()))
    }
}

/// `T/open/app`, as `host.rs` builds it: the app's data directory
/// `0700` under an other-writable parent no walk may cross.
struct App {
    _root: tempfile::TempDir,
    paths: ProfilePaths,
}

fn new_app() -> App {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .expect("tempdir");
    let open = root.path().join("open");
    let dir = open.join("app");
    for (path, mode) in [(&open, 0o777), (&dir, 0o700)] {
        std::fs::create_dir(path).expect("mkdir");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
    let paths =
        ProfilePaths::resolve_embedded(PROFILE, TrustBoundary::new(&dir).expect("a boundary"))
            .expect("paths");
    App { _root: root, paths }
}

fn record(app: &App) -> Vec<u8> {
    std::fs::read(custody::custody_file(&app.paths)).expect("the record")
}

fn put(app: &App, bytes: &[u8]) {
    let path = custody::custody_file(&app.paths);
    std::fs::write(&path, bytes).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
}

fn peer_of(id: &ProfileIdentity) -> String {
    id.transport_identity().expect("peer").as_str().to_owned()
}

/// Gate (a)'s first clause on the host: each frozen vector's exact seed,
/// in both policies, stored and given back with its frozen `PeerId`; the
/// record is the layout, owner-only, and a second wrap of the same seed
/// is other bytes (a fresh key and IV), the `PeerId` the same.
#[test]
fn the_exact_seed_round_trips_with_its_frozen_peer_id() {
    let vectors = vectors();
    assert!(!vectors.is_empty(), "the fixture holds vectors");
    for (mnemonic, frozen) in &vectors {
        for policy in POLICIES {
            let app = new_app();
            let cipher = SoftCipher::default();
            let stored = custody::provision(&app.paths, &cipher, &identity(mnemonic), policy)
                .expect("stored");
            assert_eq!(stored.as_str(), frozen);
            let bytes = record(&app);
            assert_eq!(bytes.len(), ENVELOPE_LEN + frozen.len());
            assert_eq!(&bytes[..4], b"IWK1");
            assert_eq!(bytes[4], 1);
            assert_eq!(bytes[5], slot(policy));
            assert_eq!(&bytes[ENVELOPE_LEN..], frozen.as_bytes());
            let mode = std::fs::metadata(custody::custody_file(&app.paths))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);

            let unlocked = custody::unlock(&app.paths, &cipher).expect("unlocked");
            assert_eq!(&peer_of(&unlocked), frozen);
            assert_eq!(
                unlocked.recovery_phrase().expect("phrase").expose_words(),
                *mnemonic,
                "the same 32 bytes, not merely the same PeerId"
            );
            assert_eq!(
                custody::recorded_identity(&app.paths)
                    .expect("read")
                    .map(|p| p.as_str().to_owned()),
                Some(frozen.clone())
            );

            let again = new_app();
            custody::provision(&again.paths, &cipher, &identity(mnemonic), policy).expect("again");
            assert_ne!(record(&again)[..ENVELOPE_LEN], bytes[..ENVELOPE_LEN]);
        }
    }
}

/// No record: unprovisioned, and asking creates nothing.
#[test]
fn an_unprovisioned_profile_is_answered_as_one_and_nothing_is_made() {
    let app = new_app();
    let cipher = SoftCipher::default();
    assert_eq!(
        custody::unlock(&app.paths, &cipher).err(),
        Some(UnlockRefused::Unprovisioned)
    );
    assert!(!app.paths.identity_dir().exists(), "unlock wrote nothing");
    assert_eq!(cipher.seals(), 0);
    assert_eq!(custody::recorded_identity(&app.paths).expect("read"), None);
}

/// Every single-bit flip of the stored envelope gives no identity back
/// and leaves the record as it was; the unflipped record unlocks before
/// and after (the control).
#[test]
fn every_flipped_envelope_bit_is_refused_and_the_record_is_kept() {
    let (mnemonic, _) = vectors().remove(0);
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&mnemonic),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("stored");
    let good = record(&app);
    let mut tally: HashMap<String, usize> = HashMap::new();
    for bit in 0..ENVELOPE_LEN * 8 {
        let mut bad = good.clone();
        bad[bit / 8] ^= 1 << (bit % 8);
        put(&app, &bad);
        match custody::unlock(&app.paths, &cipher) {
            Err(UnlockRefused::RecoveryRequired(cause)) => {
                *tally.entry(format!("{cause:?}")).or_default() += 1;
            }
            Err(other) => panic!("bit {bit}: {other:?}"),
            Ok(_) => panic!("bit {bit}: a flipped envelope gave an identity back"),
        }
        assert_eq!(record(&app), bad, "bit {bit}: the record was rewritten");
    }
    put(&app, &good);
    custody::unlock(&app.paths, &cipher).expect("the control unlocks");
    assert_eq!(tally.values().sum::<usize>(), ENVELOPE_LEN * 8);
    // SPIKE-009 H3's shape, 528 bits: magic, version and the five
    // invalid policy values by the header check, before any cipher. The
    // one flip to the OTHER valid policy asks for that policy's key: this
    // profile holds none, so it is `KeyMissing` here (the spike's single
    // key counted it as authentication) -- a refusal either way, and
    // `the_swapped_policy_fails_with_both_keys_held` shows it fails
    // authentication when that key exists. The rest by authentication.
    assert_eq!(tally.get("Record(Magic)"), Some(&32));
    assert_eq!(tally.get("Record(Version)"), Some(&8));
    assert_eq!(tally.get("Record(Policy)"), Some(&7));
    assert_eq!(tally.get("KeyMissing"), Some(&1));
    assert_eq!(tally.get("Authentication"), Some(&480));
    assert_eq!(cipher.seals(), 1, "no refusal sealed anything");
}

/// The policy byte is associated data: swapped for the other valid
/// value while both policies' keys exist, the envelope fails
/// authentication rather than opening under the other key.
#[test]
fn the_swapped_policy_fails_with_both_keys_held() {
    let (mnemonic, _) = vectors().remove(0);
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&mnemonic),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("stored");
    let good = record(&app);
    // The other policy's key, sealed for another profile's record.
    let other = new_app();
    custody::provision(
        &other.paths,
        &cipher,
        &ProfileIdentity::generate(),
        KeyUnlockPolicy::UserPresence,
    )
    .expect("the other key");
    let mut swapped = good.clone();
    swapped[5] = 1;
    put(&app, &swapped);
    assert_eq!(
        custody::unlock(&app.paths, &cipher).err(),
        Some(UnlockRefused::RecoveryRequired(
            RecoveryCause::Authentication
        ))
    );
    put(&app, &good);
    custody::unlock(&app.paths, &cipher).expect("the control unlocks");
}

/// The `PeerId` beside the envelope is authenticated, not trusted: another
/// valid `PeerId` there fails authentication, a damaged one is refused
/// before the cipher.
#[test]
fn the_recorded_peer_id_is_authenticated_not_trusted() {
    let vectors = two();
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&vectors[0].0),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("stored");
    let good = record(&app);

    let mut moved = good[..ENVELOPE_LEN].to_vec();
    moved.extend_from_slice(vectors[1].1.as_bytes());
    put(&app, &moved);
    assert_eq!(
        custody::unlock(&app.paths, &cipher).err(),
        Some(UnlockRefused::RecoveryRequired(
            RecoveryCause::Authentication
        ))
    );

    let mut damaged = good.clone();
    *damaged.last_mut().unwrap() = b'!';
    put(&app, &damaged);
    assert_eq!(
        custody::unlock(&app.paths, &cipher).err(),
        Some(UnlockRefused::RecoveryRequired(RecoveryCause::Record(
            RecordRefused::PeerId
        )))
    );

    put(&app, &good);
    custody::unlock(&app.paths, &cipher).expect("the control unlocks");
}

/// Truncated, empty, envelope-only and oversized records are refused for
/// their length before the cipher.
#[test]
fn a_record_of_the_wrong_length_is_refused_before_the_cipher() {
    let (mnemonic, _) = vectors().remove(0);
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&mnemonic),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("stored");
    let good = record(&app);
    let mut oversized = good.clone();
    oversized.extend(std::iter::repeat_n(b'1', 200));
    for bad in [
        Vec::new(),
        good[..ENVELOPE_LEN - 1].to_vec(),
        good[..ENVELOPE_LEN].to_vec(),
        oversized,
    ] {
        put(&app, &bad);
        cipher.fail_with(Some(CipherFailure::Unavailable(
            "the cipher was asked".to_owned(),
        )));
        assert_eq!(
            custody::unlock(&app.paths, &cipher).err(),
            Some(UnlockRefused::RecoveryRequired(RecoveryCause::Record(
                RecordRefused::Length
            ))),
            "{} bytes",
            bad.len()
        );
        cipher.fail_with(None);
    }
    put(&app, &good);
    custody::unlock(&app.paths, &cipher).expect("the control unlocks");
}

/// Another wrapping key -- what an invalidated-and-remade Keystore key
/// looks like (SPIKE-009 H5) -- fails authentication.
#[test]
fn another_wrapping_key_fails_authentication() {
    let (mnemonic, _) = vectors().remove(0);
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&mnemonic),
        KeyUnlockPolicy::UserPresence,
    )
    .expect("stored");
    custody::unlock(&app.paths, &cipher).expect("the control unlocks");
    let other = SoftCipher::default();
    other.keys.lock().unwrap().insert(1, rand::random());
    assert_eq!(
        custody::unlock(&app.paths, &other).err(),
        Some(UnlockRefused::RecoveryRequired(
            RecoveryCause::Authentication
        ))
    );
}

/// Gate (a)'s client-side clause at the custody path: an invalidated or
/// missing key gives no identity, says recovery, leaves the record as it
/// was and seals nothing -- no new key over the profile -- and the
/// identity comes back only through its own phrase.
#[test]
fn an_invalidated_or_missing_key_requires_recovery_and_mints_nothing() {
    let (mnemonic, frozen) = vectors().remove(0);
    for (failure, cause) in [
        (CipherFailure::KeyInvalidated, RecoveryCause::KeyInvalidated),
        (CipherFailure::KeyMissing, RecoveryCause::KeyMissing),
    ] {
        let app = new_app();
        let cipher = SoftCipher::default();
        custody::provision(
            &app.paths,
            &cipher,
            &identity(&mnemonic),
            KeyUnlockPolicy::UserPresence,
        )
        .expect("stored");
        custody::unlock(&app.paths, &cipher).expect("the control unlocks");
        let before = record(&app);

        if failure == CipherFailure::KeyMissing {
            cipher.forget(KeyUnlockPolicy::UserPresence);
        } else {
            cipher.fail_with(Some(failure.clone()));
        }
        for _ in 0..3 {
            assert_eq!(
                custody::unlock(&app.paths, &cipher).err(),
                Some(UnlockRefused::RecoveryRequired(cause.clone()))
            );
        }
        assert_eq!(record(&app), before, "the record is kept for recovery");
        assert_eq!(cipher.seals(), 1, "nothing was sealed: no key was minted");
        assert_eq!(
            custody::recorded_identity(&app.paths)
                .expect("read")
                .unwrap()
                .as_str(),
            frozen
        );
        assert!(matches!(
            custody::provision(
                &app.paths,
                &cipher,
                &ProfileIdentity::generate(),
                KeyUnlockPolicy::UserPresence
            ),
            Err(CustodyRefused::AlreadyProvisioned)
        ));
        assert_eq!(cipher.seals(), 1, "a refused provision sealed nothing");

        cipher.fail_with(None);
        let expected = TransportIdentity::parse(frozen.clone()).expect("peer");
        let phrase = RecoveryPhrase::parse(&mnemonic).expect("phrase");
        let restored = custody::restore(
            &app.paths,
            &cipher,
            &phrase,
            &expected,
            KeyUnlockPolicy::UserPresence,
        )
        .expect("restored from its phrase");
        assert_eq!(peer_of(&restored), frozen);
        assert_eq!(
            peer_of(&custody::unlock(&app.paths, &cipher).expect("unlocks")),
            frozen
        );
    }
}

/// The two answers that decide nothing about the record.
#[test]
fn an_absent_user_or_an_unavailable_platform_decides_nothing() {
    let (mnemonic, _) = vectors().remove(0);
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&mnemonic),
        KeyUnlockPolicy::UserPresence,
    )
    .expect("stored");
    let before = record(&app);
    cipher.fail_with(Some(CipherFailure::UserNotAuthenticated));
    assert_eq!(
        custody::unlock(&app.paths, &cipher).err(),
        Some(UnlockRefused::UserNotAuthenticated)
    );
    cipher.fail_with(Some(CipherFailure::Unavailable("busy".to_owned())));
    assert_eq!(
        custody::unlock(&app.paths, &cipher).err(),
        Some(UnlockRefused::Unavailable("busy".to_owned()))
    );
    assert_eq!(record(&app), before);
    cipher.fail_with(None);
    custody::unlock(&app.paths, &cipher).expect("the control unlocks");
}

/// A seed that opens and derives another `PeerId` is refused.
#[test]
fn a_seed_deriving_another_identity_is_refused() {
    let vectors = two();
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&vectors[0].0),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("stored");
    let other = RecoveryPhrase::parse(&vectors[1].0)
        .expect("phrase")
        .expose_entropy()
        .expect("seed");
    assert_eq!(
        custody::unlock(&app.paths, &Substituting(Seed::new(other))).err(),
        Some(UnlockRefused::RecoveryRequired(
            RecoveryCause::WrongIdentity
        ))
    );
    let own = RecoveryPhrase::parse(&vectors[0].0)
        .expect("phrase")
        .expose_entropy()
        .expect("seed");
    custody::unlock(&app.paths, &Substituting(Seed::new(own))).expect("the control: its own seed");
}

/// A record anyone else may read, or a link in its place, is refused.
#[test]
fn a_record_that_is_not_private_is_refused() {
    let (mnemonic, _) = vectors().remove(0);
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&mnemonic),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("stored");
    let path = custody::custody_file(&app.paths);
    let not_private = Some(UnlockRefused::RecoveryRequired(RecoveryCause::Record(
        RecordRefused::NotPrivate,
    )));

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert_eq!(custody::unlock(&app.paths, &cipher).err(), not_private);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    custody::unlock(&app.paths, &cipher).expect("the control unlocks");

    let real = path.with_extension("real");
    std::fs::rename(&path, &real).expect("move");
    std::os::unix::fs::symlink(&real, &path).expect("link");
    assert_eq!(custody::unlock(&app.paths, &cipher).err(), not_private);
}

/// Provisioning a profile that has a record writes nothing and seals
/// nothing -- a seal would replace the key the record needs.
#[test]
fn a_second_provision_is_refused_and_the_first_still_unlocks() {
    let vectors = two();
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&vectors[0].0),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("stored");
    let before = record(&app);
    assert!(matches!(
        custody::provision(
            &app.paths,
            &cipher,
            &identity(&vectors[1].0),
            KeyUnlockPolicy::BackgroundCompatible
        ),
        Err(CustodyRefused::AlreadyProvisioned)
    ));
    assert_eq!(record(&app), before);
    assert_eq!(cipher.seals(), 1);
    assert_eq!(
        peer_of(&custody::unlock(&app.paths, &cipher).expect("unlocks")),
        vectors[0].1
    );
}

/// Restore never changes the profile's `PeerId`: a phrase for another
/// identity, or an `expected` the record does not name, writes nothing.
#[test]
fn restore_refuses_any_other_identity() {
    let vectors = two();
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&vectors[0].0),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("stored");
    let before = record(&app);
    let own = TransportIdentity::parse(vectors[0].1.clone()).expect("peer");
    let other = TransportIdentity::parse(vectors[1].1.clone()).expect("peer");
    let phrase = |i: usize| RecoveryPhrase::parse(&vectors[i].0).expect("phrase");

    assert!(matches!(
        custody::restore(
            &app.paths,
            &cipher,
            &phrase(1),
            &own,
            KeyUnlockPolicy::BackgroundCompatible
        ),
        Err(CustodyRefused::OtherIdentity)
    ));
    assert!(matches!(
        custody::restore(
            &app.paths,
            &cipher,
            &phrase(1),
            &other,
            KeyUnlockPolicy::BackgroundCompatible
        ),
        Err(CustodyRefused::RecordNamesOther)
    ));
    assert_eq!(record(&app), before);
    assert_eq!(cipher.seals(), 1);

    // The control: its own phrase, a damaged record replaced.
    let mut damaged = before.clone();
    damaged[0] ^= 1;
    put(&app, &damaged);
    custody::restore(
        &app.paths,
        &cipher,
        &phrase(0),
        &own,
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("restored");
    assert_eq!(
        peer_of(&custody::unlock(&app.paths, &cipher).expect("unlocks")),
        vectors[0].1
    );
}

/// A cipher answering another IV or sealed length is refused and
/// nothing written; the layout's lengths are written (the control).
#[test]
fn a_misshapen_seal_is_refused_and_nothing_is_written() {
    let (mnemonic, _) = vectors().remove(0);
    for (iv, sealed) in [
        (16, SEALED_LEN),
        (IV_LEN, SEALED_LEN - 1),
        (IV_LEN, SEALED_LEN + 1),
    ] {
        let app = new_app();
        assert!(
            matches!(
                custody::provision(
                    &app.paths,
                    &Misshapen { iv, sealed },
                    &identity(&mnemonic),
                    KeyUnlockPolicy::BackgroundCompatible,
                ),
                Err(CustodyRefused::CipherShape)
            ),
            "iv {iv}, sealed {sealed}"
        );
        assert!(!custody::custody_file(&app.paths).exists());
    }
    let app = new_app();
    custody::provision(
        &app.paths,
        &Misshapen {
            iv: IV_LEN,
            sealed: SEALED_LEN,
        },
        &identity(&mnemonic),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("the layout's lengths are framed");
}

/// A record whose directory others may write cannot be trusted to be
/// the profile's: unlock gives no identity and decides nothing, and
/// restore writes nothing over it, since whose it is is unknown.
#[test]
fn a_record_under_a_loosened_directory_is_neither_used_nor_replaced() {
    let (mnemonic, frozen) = vectors().remove(0);
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&mnemonic),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("stored");
    let before = record(&app);
    let dir = app.paths.identity_dir();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o757)).expect("chmod");
    assert!(matches!(
        custody::unlock(&app.paths, &cipher),
        Err(UnlockRefused::Unavailable(_))
    ));
    let expected = TransportIdentity::parse(frozen).expect("peer");
    let phrase = RecoveryPhrase::parse(&mnemonic).expect("phrase");
    assert!(matches!(
        custody::restore(
            &app.paths,
            &cipher,
            &phrase,
            &expected,
            KeyUnlockPolicy::BackgroundCompatible
        ),
        Err(CustodyRefused::Unreadable(_))
    ));
    assert_eq!(cipher.seals(), 1, "nothing sealed");
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert_eq!(record(&app), before);
    custody::unlock(&app.paths, &cipher).expect("the control unlocks");
}
