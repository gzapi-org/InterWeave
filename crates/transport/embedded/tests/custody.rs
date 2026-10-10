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
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use interweave_profile_config::runtime::KeyUnlockPolicy;
use interweave_profile_config::{ProfileLock, ProfilePaths, TrustBoundary};
use interweave_profile_identity::{ProfileIdentity, RecoveryPhrase};
use interweave_transport_api::TransportIdentity;
use interweave_transport_embedded::custody::{
    self, CipherFailure, CustodyRefused, ENVELOPE_LEN, IV_LEN, KeyRef, RecordRefused,
    RecoveryCause, SEALED_LEN, Sealed, Seed, SeedCipher, UnlockRefused,
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

/// The device's Keystore, in software: one AES-256-GCM key per alias
/// (`KeyRef::alias`, a profile and a policy), and on every seal the
/// profile's keys deleted and a NEW one made, as the seam requires; the
/// IV the cipher's own. `fail` makes `open` answer as the platform would
/// on that failure.
#[derive(Default)]
struct SoftCipher {
    keys: Mutex<HashMap<String, [u8; 32]>>,
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
    fn forget(&self, paths: &ProfilePaths, policy: KeyUnlockPolicy) {
        self.keys
            .lock()
            .unwrap()
            .remove(&KeyRef::new(paths, policy).alias());
    }
    /// Put a key at `paths`' alias for `policy` without sealing anything.
    fn plant(&self, paths: &ProfilePaths, policy: KeyUnlockPolicy) {
        self.keys
            .lock()
            .unwrap()
            .insert(KeyRef::new(paths, policy).alias(), rand::random());
    }
    fn seals(&self) -> usize {
        self.seals.load(Ordering::SeqCst)
    }
}

impl SeedCipher for SoftCipher {
    fn seal(&self, key_ref: &KeyRef, aad: &[u8], seed: &Seed) -> Result<Sealed, CipherFailure> {
        self.seals.fetch_add(1, Ordering::SeqCst);
        let key: [u8; 32] = rand::random();
        let mut keys = self.keys.lock().unwrap();
        for of_profile in key_ref.of_profile() {
            keys.remove(&of_profile.alias());
        }
        keys.insert(key_ref.alias(), key);
        drop(keys);
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
        key_ref: &KeyRef,
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
            .get(&key_ref.alias())
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
    fn seal(&self, _: &KeyRef, _: &[u8], _: &Seed) -> Result<Sealed, CipherFailure> {
        Err(CipherFailure::Unavailable("not used".to_owned()))
    }
    fn open(
        &self,
        _: &KeyRef,
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
    fn seal(&self, _: &KeyRef, _: &[u8], _: &Seed) -> Result<Sealed, CipherFailure> {
        Ok(Sealed {
            iv: vec![0; self.iv],
            sealed: vec![0; self.sealed],
        })
    }
    fn open(
        &self,
        _: &KeyRef,
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
    dir: PathBuf,
    paths: ProfilePaths,
}

impl App {
    /// Another profile of the same app, under the same boundary.
    fn profile(&self, name: &str) -> ProfilePaths {
        ProfilePaths::resolve_embedded(name, TrustBoundary::new(&self.dir).expect("a boundary"))
            .expect("paths")
    }
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
    App {
        _root: root,
        dir,
        paths,
    }
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
    // The profile's key for the other policy, which no seal leaves
    // beside the first: planted.
    cipher.plant(&app.paths, KeyUnlockPolicy::UserPresence);
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
    other.plant(&app.paths, KeyUnlockPolicy::UserPresence);
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
            cipher.forget(&app.paths, KeyUnlockPolicy::UserPresence);
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

/// While another holds the profile -- a running host, or another flow --
/// provision and restore are refused before they seal, since a seal
/// rotates the key a live record needs; released, both proceed (the
/// control).
#[test]
fn provision_and_restore_refuse_a_held_profile_before_sealing() {
    let (mnemonic, frozen) = vectors().remove(0);
    let app = new_app();
    let cipher = SoftCipher::default();
    let held = ProfileLock::acquire(&app.paths, Duration::ZERO).expect("the test holds it");
    assert!(matches!(
        custody::provision(
            &app.paths,
            &cipher,
            &identity(&mnemonic),
            KeyUnlockPolicy::BackgroundCompatible,
        ),
        Err(CustodyRefused::ProfileLocked)
    ));
    assert!(!custody::custody_file(&app.paths).exists());
    assert_eq!(cipher.seals(), 0);
    drop(held);

    custody::provision(
        &app.paths,
        &cipher,
        &identity(&mnemonic),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("released, it provisions");
    let before = record(&app);
    let held = ProfileLock::acquire(&app.paths, Duration::ZERO).expect("the test holds it");
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
        Err(CustodyRefused::ProfileLocked)
    ));
    assert_eq!(record(&app), before);
    assert_eq!(cipher.seals(), 1);
    custody::unlock(&app.paths, &cipher).expect("the record still opens");
    drop(held);
    custody::restore(
        &app.paths,
        &cipher,
        &phrase,
        &expected,
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("released, it restores");
}

/// Racing first runs for one policy: one stores its identity, every
/// other is refused without sealing, and the stored record opens -- not
/// a record sealed under a key a loser's seal replaced.
#[test]
fn racing_first_provisions_leave_one_record_that_opens() {
    const RUNS: usize = 8;
    let app = new_app();
    let cipher = SoftCipher::default();
    let identities: Vec<ProfileIdentity> = (0..RUNS).map(|_| ProfileIdentity::generate()).collect();
    let start = std::sync::Barrier::new(RUNS);
    let outcomes: Vec<Result<String, String>> = std::thread::scope(|scope| {
        let runs: Vec<_> = identities
            .iter()
            .map(|id| {
                let (app, cipher, start) = (&app, &cipher, &start);
                scope.spawn(move || {
                    start.wait();
                    custody::provision(
                        &app.paths,
                        cipher,
                        id,
                        KeyUnlockPolicy::BackgroundCompatible,
                    )
                    .map(|peer| peer.as_str().to_owned())
                    .map_err(|e| format!("{e:?}"))
                })
            })
            .collect();
        runs.into_iter()
            .map(|run| run.join().expect("joined"))
            .collect()
    });
    let won: Vec<&String> = outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
    assert_eq!(won.len(), 1, "{outcomes:?}");
    for lost in outcomes.iter().filter_map(|o| o.as_ref().err()) {
        assert!(
            lost == "ProfileLocked" || lost == "AlreadyProvisioned",
            "{lost}"
        );
    }
    assert_eq!(cipher.seals(), 1, "only the winner sealed");
    assert_eq!(
        &peer_of(&custody::unlock(&app.paths, &cipher).expect("the record opens")),
        won[0]
    );
}

/// A restore re-seals under a NEW key, so a copy of the record taken
/// before it -- a stolen or backed-up `identity.iwk1` -- no longer opens:
/// recovery retires the old envelope, not only replaces the file. The
/// restored record opens (the control).
#[test]
fn a_record_from_before_a_restore_no_longer_opens() {
    let (mnemonic, frozen) = vectors().remove(0);
    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&mnemonic),
        KeyUnlockPolicy::UserPresence,
    )
    .expect("stored");
    let old = record(&app);
    custody::unlock(&app.paths, &cipher).expect("the old record opens before the restore");

    custody::restore(
        &app.paths,
        &cipher,
        &RecoveryPhrase::parse(&mnemonic).expect("phrase"),
        &TransportIdentity::parse(frozen.clone()).expect("peer"),
        KeyUnlockPolicy::UserPresence,
    )
    .expect("restored");
    let restored = record(&app);
    assert_ne!(restored[..ENVELOPE_LEN], old[..ENVELOPE_LEN]);
    assert_eq!(
        peer_of(&custody::unlock(&app.paths, &cipher).expect("opens")),
        frozen
    );

    put(&app, &old);
    assert_eq!(
        custody::unlock(&app.paths, &cipher).err(),
        Some(UnlockRefused::RecoveryRequired(
            RecoveryCause::Authentication
        ))
    );
}

/// `Seed`'s `Debug` prints nothing of its bytes, so a seed caught in a
/// `{:?}` -- a log line, a panic message, a refusal formatted for the
/// platform's log -- leaks none of them. Two seeds that differ print the
/// same; the bytes are what `expose` gives (the control).
#[test]
fn a_seed_formats_without_its_bytes() {
    let bytes: [u8; 32] = std::array::from_fn(|i| u8::try_from(i).unwrap() ^ 0xA5);
    let seed = Seed::new(bytes);
    let shown = format!("{seed:?} {seed:#?}");
    for b in bytes {
        assert!(!shown.contains(&b.to_string()), "{b} in {shown}");
        assert!(
            !shown.to_lowercase().contains(&format!("{b:02x}")),
            "{b:02x} in {shown}"
        );
    }
    assert_eq!(format!("{seed:?}"), format!("{:?}", Seed::new([0; 32])));
    assert_eq!(seed.expose(), &bytes);
}

/// Each profile's key is its own: provisioning and restoring a second
/// profile under the same policy rotate none of the first's, and both
/// records open.
#[test]
fn a_second_profile_under_the_same_policy_leaves_the_first_openable() {
    let vectors = two();
    let cipher = SoftCipher::default();
    for policy in POLICIES {
        let work = new_app();
        let home = work.profile("home");
        custody::provision(&work.paths, &cipher, &identity(&vectors[0].0), policy)
            .expect("work stored");
        custody::provision(&home, &cipher, &identity(&vectors[1].0), policy).expect("home stored");
        custody::restore(
            &home,
            &cipher,
            &RecoveryPhrase::parse(&vectors[1].0).expect("phrase"),
            &TransportIdentity::parse(vectors[1].1.clone()).expect("peer"),
            policy,
        )
        .expect("home restored");
        assert_eq!(
            peer_of(&custody::unlock(&work.paths, &cipher).expect("work still opens")),
            vectors[0].1
        );
        assert_eq!(
            peer_of(&custody::unlock(&home, &cipher).expect("home opens")),
            vectors[1].1
        );
    }
}

/// A record copied from another profile's directory is opened under THIS
/// profile's key and fails authentication: it never unlocks this profile
/// as the other. The profile's own record opens (the control).
#[test]
fn a_record_copied_from_another_profile_does_not_open() {
    let vectors = two();
    for policy in POLICIES {
        let app = new_app();
        let home = app.profile("home");
        let cipher = SoftCipher::default();
        custody::provision(&app.paths, &cipher, &identity(&vectors[0].0), policy)
            .expect("work stored");
        custody::provision(&home, &cipher, &identity(&vectors[1].0), policy).expect("home stored");
        let own = record(&app);
        let copied = std::fs::read(custody::custody_file(&home)).expect("home's record");
        put(&app, &copied);
        assert_eq!(
            custody::unlock(&app.paths, &cipher).err(),
            Some(UnlockRefused::RecoveryRequired(
                RecoveryCause::Authentication
            ))
        );
        put(&app, &own);
        assert_eq!(
            peer_of(&custody::unlock(&app.paths, &cipher).expect("its own opens")),
            vectors[0].1
        );
    }
}

/// A restore under the OTHER policy retires the profile's old key too: a
/// copy of the record from before it no longer opens, whichever policy
/// it was sealed under. The restored record opens (the control).
#[test]
fn a_restore_under_the_other_policy_retires_the_old_record() {
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
    let old = record(&app);
    custody::restore(
        &app.paths,
        &cipher,
        &RecoveryPhrase::parse(&mnemonic).expect("phrase"),
        &TransportIdentity::parse(frozen.clone()).expect("peer"),
        KeyUnlockPolicy::UserPresence,
    )
    .expect("restored");
    assert_eq!(
        peer_of(&custody::unlock(&app.paths, &cipher).expect("opens")),
        frozen
    );
    put(&app, &old);
    assert_eq!(
        custody::unlock(&app.paths, &cipher).err(),
        Some(UnlockRefused::RecoveryRequired(RecoveryCause::KeyMissing))
    );
}

/// Whose record it is comes from its `PeerId` field, whatever its mode or
/// header: restore refuses to replace a record naming another `PeerId`
/// when it is readable by others or its header is damaged, and refuses a
/// link it cannot read through; the same restores of the record's own
/// identity proceed (the controls).
#[test]
fn restore_asks_whose_record_it_is_whatever_its_mode_or_header() {
    let vectors = two();
    let own = TransportIdentity::parse(vectors[0].1.clone()).expect("peer");
    let other = TransportIdentity::parse(vectors[1].1.clone()).expect("peer");
    let phrase = |i: usize| RecoveryPhrase::parse(&vectors[i].0).expect("phrase");
    let policy = KeyUnlockPolicy::BackgroundCompatible;

    let app = new_app();
    let cipher = SoftCipher::default();
    custody::provision(&app.paths, &cipher, &identity(&vectors[0].0), policy).expect("stored");
    let good = record(&app);
    let path = custody::custody_file(&app.paths);

    // Readable by others.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert!(matches!(
        custody::restore(&app.paths, &cipher, &phrase(1), &other, policy),
        Err(CustodyRefused::RecordNamesOther)
    ));
    assert_eq!(record(&app), good);
    custody::restore(&app.paths, &cipher, &phrase(0), &own, policy).expect("its own, over 0644");

    // A damaged header, the PeerId field intact.
    let mut damaged = record(&app);
    damaged[0] ^= 1;
    put(&app, &damaged);
    assert!(matches!(
        custody::restore(&app.paths, &cipher, &phrase(1), &other, policy),
        Err(CustodyRefused::RecordNamesOther)
    ));
    assert_eq!(record(&app), damaged);
    custody::restore(&app.paths, &cipher, &phrase(0), &own, policy)
        .expect("its own, over a damaged header");

    // A link in the record's place: nothing read through it, nothing written.
    let real = path.with_extension("real");
    std::fs::rename(&path, &real).expect("move");
    std::os::unix::fs::symlink(&real, &path).expect("link");
    assert!(matches!(
        custody::restore(&app.paths, &cipher, &phrase(0), &own, policy),
        Err(CustodyRefused::Unreadable(_))
    ));
    assert!(
        std::fs::symlink_metadata(&path)
            .expect("still there")
            .file_type()
            .is_symlink()
    );
    assert_eq!(cipher.seals(), 3, "the two controls sealed, nothing else");
}

/// One alias per profile and policy: two profiles' aliases never meet,
/// and `of_profile` names exactly the profile's own two.
#[test]
fn aliases_are_one_per_profile_and_policy() {
    let app = new_app();
    let profiles = [
        app.paths.profile().to_owned(),
        "home".to_owned(),
        "work-2".to_owned(),
    ];
    let mut aliases = std::collections::BTreeSet::new();
    for name in &profiles {
        let paths = app.profile(name);
        for policy in POLICIES {
            let key = KeyRef::new(&paths, policy);
            assert!(aliases.insert(key.alias()), "{}", key.alias());
            let own: Vec<String> = key.of_profile().iter().map(KeyRef::alias).collect();
            assert_eq!(
                own,
                POLICIES.map(|p| KeyRef::new(&paths, p).alias()).to_vec()
            );
        }
    }
    assert_eq!(aliases.len(), profiles.len() * POLICIES.len());
}

/// ADR-0042 A 2026-10-10 (ii): a record under another policy than the
/// configured one still unlocks, and `reseal` brings it to the configured
/// policy -- the same identity, the old record retired. Under the policy
/// it already has, nothing is sealed (the control).
#[test]
fn reseal_moves_the_record_to_the_configured_policy_and_retires_the_old() {
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
    let old = record(&app);
    let unlocked = custody::unlock(&app.paths, &cipher).expect("unlocks under its own policy");

    assert!(
        !custody::reseal(
            &app.paths,
            &cipher,
            &unlocked,
            KeyUnlockPolicy::BackgroundCompatible
        )
        .expect("the same policy")
    );
    assert_eq!(cipher.seals(), 1, "nothing sealed for the same policy");
    assert_eq!(record(&app), old);

    assert!(
        custody::reseal(
            &app.paths,
            &cipher,
            &unlocked,
            KeyUnlockPolicy::UserPresence
        )
        .expect("re-sealed")
    );
    assert_eq!(record(&app)[5], slot(KeyUnlockPolicy::UserPresence));
    assert_eq!(
        peer_of(&custody::unlock(&app.paths, &cipher).expect("opens")),
        frozen
    );
    put(&app, &old);
    assert_eq!(
        custody::unlock(&app.paths, &cipher).err(),
        Some(UnlockRefused::RecoveryRequired(RecoveryCause::KeyMissing))
    );
}

/// `reseal` writes nothing for another identity, without a record, or
/// while another holds the profile.
#[test]
fn reseal_refuses_another_identity_no_record_and_a_held_profile() {
    let vectors = two();
    let app = new_app();
    let cipher = SoftCipher::default();
    assert!(matches!(
        custody::reseal(
            &app.paths,
            &cipher,
            &identity(&vectors[0].0),
            KeyUnlockPolicy::UserPresence
        ),
        Err(CustodyRefused::NotProvisioned)
    ));
    custody::provision(
        &app.paths,
        &cipher,
        &identity(&vectors[0].0),
        KeyUnlockPolicy::BackgroundCompatible,
    )
    .expect("stored");
    let before = record(&app);
    assert!(matches!(
        custody::reseal(
            &app.paths,
            &cipher,
            &identity(&vectors[1].0),
            KeyUnlockPolicy::UserPresence
        ),
        Err(CustodyRefused::RecordNamesOther)
    ));
    let held = ProfileLock::acquire(&app.paths, Duration::ZERO).expect("the test holds it");
    assert!(matches!(
        custody::reseal(
            &app.paths,
            &cipher,
            &identity(&vectors[0].0),
            KeyUnlockPolicy::UserPresence
        ),
        Err(CustodyRefused::ProfileLocked)
    ));
    drop(held);
    assert_eq!(record(&app), before);
    assert_eq!(cipher.seals(), 1, "nothing sealed by a refusal");
    custody::reseal(
        &app.paths,
        &cipher,
        &identity(&vectors[0].0),
        KeyUnlockPolicy::UserPresence,
    )
    .expect("the control: its own identity, released");
}
