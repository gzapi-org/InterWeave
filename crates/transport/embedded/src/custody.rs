// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The profile's identity at rest on a device (plan §20 step 6): the
//! exact 32-byte Ed25519 seed wrapped by a platform key in the IWK1 v1
//! envelope SPIKE-009's Result adopted, and unwrapped into the
//! [`ProfileIdentity`] an [`EmbeddedLaunch`](crate::EmbeddedLaunch)
//! takes -- or refused.
//!
//! ```text
//! envelope  = "IWK1" (4) | version 0x01 (1) | policy (1) | iv (12) | ciphertext (32) | tag (16)
//! aad       = "IWK1" | version | policy | the profile's PeerId (UTF-8)
//! record    = envelope (66) | the profile's PeerId (UTF-8)     -- one file, CUSTODY_FILE
//! ```
//!
//! The cipher is the platform's, behind [`SeedCipher`]: on Android an
//! AES-256-GCM key in `AndroidKeyStore`, whose IV the Keystore chooses
//! and this module stores. Everything around it is here, so no adapter
//! decides it: the header is checked BEFORE any cipher call, the header
//! and the `PeerId` are associated data (authenticated, never trusted), and
//! the seed that comes back must re-derive the record's `PeerId`.
//!
//! WHAT THIS NEVER DOES is make an identity. [`unlock`] answers the
//! stored identity or why there is none, and no refusal of it leads to a
//! new key: [`UnlockRefused::RecoveryRequired`] is where the app shows
//! its recovery flow (step 7). [`provision`] refuses a profile that
//! already has a record, and [`restore`] refuses a phrase for any other
//! `PeerId` than the one asked for, so the profile's `PeerId` never changes
//! through this module.

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

use interweave_profile_config::runtime::KeyUnlockPolicy;
use interweave_profile_config::{
    PersistError, ProfileLock, ProfilePaths, create_private_exclusive_within,
    resolve_owned_private_dir_within, write_private_atomic_within,
};
use interweave_profile_identity::{ProfileIdentity, RecoveryPhrase};
use interweave_transport_api::TransportIdentity;

/// The envelope's magic.
pub const MAGIC: &[u8; 4] = b"IWK1";
/// The one version this build reads and writes.
pub const VERSION: u8 = 1;
/// Magic, version and policy.
pub const HEADER_LEN: usize = 6;
/// The AES-GCM IV, as the Keystore makes it.
pub const IV_LEN: usize = 12;
/// The Ed25519 seed (ADR-0033: the BIP-39 entropy IS the seed).
pub const SEED_LEN: usize = 32;
/// The AES-GCM tag.
pub const TAG_LEN: usize = 16;
/// Ciphertext and tag, as the cipher returns them.
pub const SEALED_LEN: usize = SEED_LEN + TAG_LEN;
/// The whole envelope: 66 bytes.
pub const ENVELOPE_LEN: usize = HEADER_LEN + IV_LEN + SEALED_LEN;
/// The custody record's name, in the profile's identity directory.
pub const CUSTODY_FILE: &str = "identity.iwk1";
/// The longest `PeerId` a record may carry. An Ed25519 `PeerId` is 52
/// characters; the bound is so a planted file is not read whole before
/// it is refused, not a measurement of any `PeerId`.
const PEER_MAX: usize = 128;

/// The 32 seed bytes, cleared when dropped, however they are dropped.
/// No `Clone`, and a `Debug` that prints nothing of them.
pub struct Seed([u8; SEED_LEN]);

impl Seed {
    /// Take ownership of seed bytes; the caller's copy is its own to clear.
    #[must_use]
    pub fn new(bytes: [u8; SEED_LEN]) -> Self {
        Self(bytes)
    }

    /// The bytes, for the cipher that wraps them.
    #[must_use]
    pub fn expose(&self) -> &[u8; SEED_LEN] {
        &self.0
    }
}

impl Drop for Seed {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl std::fmt::Debug for Seed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Seed(..)")
    }
}

/// What a cipher returned from a seal: the IV it chose and the
/// ciphertext with the tag appended. Lengths are checked here, not
/// trusted: a cipher returning another IV or tag size is refused, never
/// framed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// The IV the cipher chose.
    pub iv: Vec<u8>,
    /// Ciphertext followed by the tag.
    pub sealed: Vec<u8>,
}

/// Why the platform cipher did not seal or open. CLOSED: the adapter maps
/// every platform failure to one of these, and a new cause is a change of
/// this contract (ADR-0042), not of the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CipherFailure {
    /// The wrapping key exists and the platform has invalidated it -- a
    /// credential change, a biometric enrollment (SPIKE-009 D6a/D6b).
    KeyInvalidated,
    /// No wrapping key for the policy.
    KeyMissing,
    /// The key needs the person present and they have not authenticated
    /// within its window (SPIKE-009 D5).
    UserNotAuthenticated,
    /// The cipher refused the ciphertext or its associated data.
    Authentication,
    /// Anything else the platform raised; for the log only, and NOT a
    /// verdict on the stored identity.
    Unavailable(String),
}

/// Which wrapping key a seal or an open uses: the PROFILE's, for one
/// policy -- computed here from the profile's paths, never read from a
/// record, and used by the adapter as given ([`KeyRef::alias`]). One key
/// per profile is what keeps profiles apart: sealing one rotates no
/// other profile's key, and a record copied into another profile's
/// directory is opened under that profile's key and fails
/// authentication (`a_record_copied_from_another_profile_does_not_open`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRef {
    profile: String,
    policy: KeyUnlockPolicy,
}

impl KeyRef {
    /// The key of `paths`' profile for `policy`.
    #[must_use]
    pub fn new(paths: &ProfilePaths, policy: KeyUnlockPolicy) -> Self {
        Self {
            profile: paths.profile().to_owned(),
            policy,
        }
    }

    /// The profile's name.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// The policy the key is generated under.
    #[must_use]
    pub fn policy(&self) -> KeyUnlockPolicy {
        self.policy
    }

    /// The Keystore alias, `interweave.iwk1.<profile>.<policy>`: the one
    /// name the adapter generates, deletes and opens under, so no adapter
    /// derives it differently. A profile name holds no `.`
    /// (`ProfilePaths`' rule: letters, digits, `-` and `_`), so two
    /// profiles' aliases never meet (`aliases_are_one_per_profile_and_policy`).
    #[must_use]
    pub fn alias(&self) -> String {
        format!(
            "interweave.iwk1.{}.{}",
            self.profile,
            policy_name(self.policy)
        )
    }

    /// Every key this profile may hold, one per policy: what a seal
    /// deletes before it generates.
    #[must_use]
    pub fn of_profile(&self) -> [KeyRef; 2] {
        [
            KeyUnlockPolicy::BackgroundCompatible,
            KeyUnlockPolicy::UserPresence,
        ]
        .map(|policy| KeyRef {
            profile: self.profile.clone(),
            policy,
        })
    }
}

fn policy_name(policy: KeyUnlockPolicy) -> &'static str {
    match policy {
        KeyUnlockPolicy::BackgroundCompatible => "background-compatible",
        KeyUnlockPolicy::UserPresence => "user-presence",
    }
}

/// The platform's wrapping cipher: on Android, AES-256-GCM keys in
/// `AndroidKeyStore`, one per profile and policy, named by [`KeyRef`]
/// (the seam agreed with the app's adapter, rust-ui-dev's 01a12567,
/// keyed by profile after #256's review). `Send + Sync`, and its
/// calls may block: the adapter reaches the platform from whatever thread
/// asks.
///
/// THE SEED'S WIPING IS BEST-EFFORT AND BOUNDED BY THE PLATFORM: on
/// Android the Keystore's cipher hands the plaintext over as a Java
/// `byte[]`, which the adapter fills with zeros once it is copied into a
/// [`Seed`] -- the only wipe Java allows -- so the seed lives in the
/// Java heap for the length of one call, and what the collector copied
/// meanwhile is not reached. A limit, not a zeroization claim.
pub trait SeedCipher: Send + Sync {
    /// Encrypt `seed` with `aad` under a key the cipher GENERATES at
    /// `key`'s alias for this seal, after deleting EVERY key of `key`'s
    /// profile ([`KeyRef::of_profile`], both policies) and no other
    /// profile's -- so a restore after [`CipherFailure::KeyInvalidated`]
    /// never reuses the invalidated key, and an envelope of this profile
    /// sealed before, under either policy, no longer opens. Only
    /// [`provision`] and [`restore`] seal.
    ///
    /// # Errors
    /// [`CipherFailure`].
    fn seal(&self, key: &KeyRef, aad: &[u8], seed: &Seed) -> Result<Sealed, CipherFailure>;

    /// Decrypt `sealed` (ciphertext and tag) with `iv` and `aad` under the
    /// key at `key`'s alias.
    ///
    /// # Errors
    /// [`CipherFailure`].
    fn open(
        &self,
        key: &KeyRef,
        iv: &[u8; IV_LEN],
        sealed: &[u8; SEALED_LEN],
        aad: &[u8],
    ) -> Result<Seed, CipherFailure>;
}

/// Why a stored record was refused before any cipher call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordRefused {
    /// Too short to hold an envelope and a `PeerId`, or longer than any.
    Length,
    /// Not `IWK1`.
    Magic,
    /// Not version 1.
    Version,
    /// Neither policy byte.
    Policy,
    /// The `PeerId` after the envelope is not one.
    PeerId,
    /// Not a regular file, or readable by anyone but its owner.
    NotPrivate,
}

/// Why the stored identity cannot be used, and so why the app must
/// enter recovery. None of these leads to a new key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryCause {
    /// [`CipherFailure::KeyInvalidated`].
    KeyInvalidated,
    /// [`CipherFailure::KeyMissing`].
    KeyMissing,
    /// The record was refused before the cipher.
    Record(RecordRefused),
    /// The cipher refused the envelope: tampered, another key's, another
    /// profile's, or a policy byte swapped.
    Authentication,
    /// The envelope authenticated and its seed derives another `PeerId`.
    WrongIdentity,
}

/// Why [`unlock`] gave no identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockRefused {
    /// No record: a profile that has never been provisioned. The app
    /// chooses between creating and restoring; nothing here creates.
    Unprovisioned,
    /// The record exists and cannot give its identity back. The app
    /// enters recovery; the record is left as it was.
    RecoveryRequired(RecoveryCause),
    /// The key needs the person; stay offline until they authenticate.
    UserNotAuthenticated,
    /// The cipher or the storage failed for a reason that says nothing
    /// about the record; try again, decide nothing.
    Unavailable(String),
}

/// Why [`provision`] or [`restore`] wrote nothing.
#[derive(Debug)]
pub enum CustodyRefused {
    /// [`provision`] found a record: the profile has an identity.
    AlreadyProvisioned,
    /// [`reseal`] found no record to re-seal.
    NotProvisioned,
    /// Another holder has the profile -- a running host, or a provision
    /// or restore in flight -- so nothing was sealed.
    ProfileLocked,
    /// [`restore`]'s phrase restores another `PeerId` than the one asked.
    OtherIdentity,
    /// [`restore`] was asked for one `PeerId` and the record names another.
    RecordNamesOther,
    /// The identity or the phrase could not give its seed or `PeerId`.
    Identity(String),
    /// The cipher refused to seal.
    Cipher(CipherFailure),
    /// The cipher returned an IV or sealed bytes of another length.
    CipherShape,
    /// The existing record could not be read, so whose it is is unknown.
    Unreadable(String),
    /// The record could not be written.
    Storage(PersistError),
}

impl std::fmt::Display for UnlockRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unprovisioned => f.write_str("the profile has no stored identity"),
            Self::RecoveryRequired(cause) => {
                write!(
                    f,
                    "the stored identity cannot be used ({cause:?}); recovery is required"
                )
            }
            Self::UserNotAuthenticated => f.write_str("the key needs the user to authenticate"),
            Self::Unavailable(detail) => write!(f, "the identity is unavailable: {detail}"),
        }
    }
}

impl std::error::Error for UnlockRefused {}

impl std::fmt::Display for CustodyRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyProvisioned => f.write_str("the profile already has a stored identity"),
            Self::NotProvisioned => f.write_str("the profile has no stored identity to re-seal"),
            Self::ProfileLocked => f.write_str("the profile is held by another host or flow"),
            Self::OtherIdentity => f.write_str("the phrase restores another identity"),
            Self::RecordNamesOther => f.write_str("the stored record names another identity"),
            Self::Identity(detail) => write!(f, "the identity is unusable: {detail}"),
            Self::Cipher(failure) => write!(f, "the cipher refused to seal: {failure:?}"),
            Self::CipherShape => f.write_str("the cipher returned bytes of another length"),
            Self::Unreadable(detail) => write!(f, "the stored record cannot be read: {detail}"),
            Self::Storage(e) => write!(f, "the record could not be written: {e}"),
        }
    }
}

impl std::error::Error for CustodyRefused {}

fn policy_byte(policy: KeyUnlockPolicy) -> u8 {
    match policy {
        KeyUnlockPolicy::BackgroundCompatible => 0,
        KeyUnlockPolicy::UserPresence => 1,
    }
}

fn policy_of(byte: u8) -> Option<KeyUnlockPolicy> {
    match byte {
        0 => Some(KeyUnlockPolicy::BackgroundCompatible),
        1 => Some(KeyUnlockPolicy::UserPresence),
        _ => None,
    }
}

/// The associated data for `policy` and the profile `peer`.
#[must_use]
pub fn aad(policy: KeyUnlockPolicy, peer: &TransportIdentity) -> Vec<u8> {
    let mut a = Vec::with_capacity(HEADER_LEN + peer.as_str().len());
    a.extend_from_slice(MAGIC);
    a.push(VERSION);
    a.push(policy_byte(policy));
    a.extend_from_slice(peer.as_str().as_bytes());
    a
}

/// A record's parts, its header checked and nothing yet authenticated.
struct Parsed {
    policy: KeyUnlockPolicy,
    iv: [u8; IV_LEN],
    sealed: [u8; SEALED_LEN],
    peer: TransportIdentity,
}

/// Length, magic, version, policy, then the `PeerId`: SPIKE-009's order,
/// all before any cipher call.
fn parse(record: &[u8]) -> Result<Parsed, RecordRefused> {
    if record.len() <= ENVELOPE_LEN || record.len() > ENVELOPE_LEN + PEER_MAX {
        return Err(RecordRefused::Length);
    }
    if &record[..4] != MAGIC {
        return Err(RecordRefused::Magic);
    }
    if record[4] != VERSION {
        return Err(RecordRefused::Version);
    }
    let policy = policy_of(record[5]).ok_or(RecordRefused::Policy)?;
    let peer = std::str::from_utf8(&record[ENVELOPE_LEN..])
        .ok()
        .and_then(|text| TransportIdentity::parse(text).ok())
        .ok_or(RecordRefused::PeerId)?;
    let mut iv = [0u8; IV_LEN];
    iv.copy_from_slice(&record[HEADER_LEN..HEADER_LEN + IV_LEN]);
    let mut sealed = [0u8; SEALED_LEN];
    sealed.copy_from_slice(&record[HEADER_LEN + IV_LEN..ENVELOPE_LEN]);
    Ok(Parsed {
        policy,
        iv,
        sealed,
        peer,
    })
}

/// The record's path for `paths`.
#[must_use]
pub fn custody_file(paths: &ProfilePaths) -> PathBuf {
    paths.identity_dir().join(CUSTODY_FILE)
}

/// What reading the record found.
enum Read {
    Absent,
    /// A regular file's bytes, and whether only its owner may read it.
    /// A record that is not private is refused by [`unlock`]; its
    /// `PeerId` field still answers whose it is, for [`restore`].
    Bytes {
        bytes: Vec<u8>,
        private: bool,
    },
}

/// Read the record under its directory as judged (ADR-0028), as the
/// identity loader reads a key: the directory owner-only and this uid's,
/// the entry not a link, the open handle the same inode, and no more read
/// than a record can hold. Whether the file is owner-only is REPORTED,
/// not refused here: [`unlock`] refuses it, [`restore`] still asks it
/// whose it is. A link or another kind of entry is refused outright --
/// nothing is read through it.
fn read_record(paths: &ProfilePaths) -> Result<Read, UnlockRefused> {
    let io = |e: std::io::Error| UnlockRefused::Unavailable(e.to_string());
    let dir = match resolve_owned_private_dir_within(paths.identity_dir(), paths.boundary()) {
        Ok(dir) => dir,
        Err(PersistError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Read::Absent);
        }
        Err(e) => return Err(UnlockRefused::Unavailable(e.to_string())),
    };
    let path = dir.join(CUSTODY_FILE);
    let here = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Read::Absent),
        Err(e) => return Err(io(e)),
    };
    let not_private =
        UnlockRefused::RecoveryRequired(RecoveryCause::Record(RecordRefused::NotPrivate));
    if !here.file_type().is_file() {
        return Err(not_private);
    }
    let file = std::fs::File::open(&path).map_err(io)?;
    let opened = file.metadata().map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // The same inode the link check saw: belt and braces behind the
        // private directory, which already stops the entry being swapped,
        // and NO TEST REACHES IT -- a swap between two syscalls cannot be
        // scheduled -- as in the identity loader.
        if (opened.dev(), opened.ino()) != (here.dev(), here.ino()) || !opened.is_file() {
            return Err(not_private);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = opened;
        return Err(UnlockRefused::Unavailable(
            "owner-only permissions cannot be checked on this platform".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(ENVELOPE_LEN + PEER_MAX);
    // One past the bound, so an oversized record is seen as one.
    let limit = u64::try_from(ENVELOPE_LEN + PEER_MAX + 1).unwrap_or(u64::MAX);
    file.take(limit).read_to_end(&mut bytes).map_err(io)?;
    #[cfg(unix)]
    let private = {
        use std::os::unix::fs::PermissionsExt as _;
        // Group and other hold no permission bit, read as the mask it is,
        // as `profile-config`'s `is_owner_only` reads it.
        #[expect(clippy::verbose_bit_mask, reason = "a permission mask reads as one")]
        let owner_only = opened.permissions().mode() & 0o077 == 0;
        owner_only
    };
    #[cfg(not(unix))]
    let private = false;
    Ok(Read::Bytes { bytes, private })
}

/// The bytes of a record [`unlock`] may use: its own, owner-only.
fn private_bytes(paths: &ProfilePaths) -> Result<Option<Vec<u8>>, UnlockRefused> {
    match read_record(paths)? {
        Read::Absent => Ok(None),
        Read::Bytes { private: false, .. } => Err(UnlockRefused::RecoveryRequired(
            RecoveryCause::Record(RecordRefused::NotPrivate),
        )),
        Read::Bytes {
            bytes,
            private: true,
        } => Ok(Some(bytes)),
    }
}

/// Whose record `bytes` is, from its `PeerId` field ALONE: a header
/// damaged, a policy swapped or a ciphertext flipped leaves the field
/// readable, and the field is public. `None` only when no `PeerId` can
/// be read there.
fn named_peer(bytes: &[u8]) -> Option<TransportIdentity> {
    if bytes.len() <= ENVELOPE_LEN || bytes.len() > ENVELOPE_LEN + PEER_MAX {
        return None;
    }
    std::str::from_utf8(&bytes[ENVELOPE_LEN..])
        .ok()
        .and_then(|text| TransportIdentity::parse(text).ok())
}

/// The `PeerId` `seed` derives through the production derivation, and
/// the identity it is.
fn identity_of(seed: &Seed) -> Option<(ProfileIdentity, TransportIdentity)> {
    let phrase = RecoveryPhrase::from_entropy(seed.expose()).ok()?;
    let identity = ProfileIdentity::from_phrase(&phrase).ok()?;
    let peer = identity.transport_identity().ok()?;
    Some((identity, peer))
}

/// The `PeerId` the profile's record names, read without the cipher: the
/// `expected` a recovery flow passes to [`restore`] when the person has
/// not typed one. `None` when there is no record; a record refused for
/// its shape names nobody, and says why.
///
/// # Errors
/// [`UnlockRefused`], as [`unlock`] would answer before the cipher.
pub fn recorded_identity(paths: &ProfilePaths) -> Result<Option<TransportIdentity>, UnlockRefused> {
    match private_bytes(paths)? {
        None => Ok(None),
        Some(bytes) => parse(&bytes)
            .map(|parsed| Some(parsed.peer))
            .map_err(|refused| UnlockRefused::RecoveryRequired(RecoveryCause::Record(refused))),
    }
}

/// The profile's stored identity, unwrapped by `cipher` and checked to
/// be the record's `PeerId` -- or why not. Reads only: on every refusal
/// the record is left exactly as it was.
///
/// # Errors
/// [`UnlockRefused`].
pub fn unlock(
    paths: &ProfilePaths,
    cipher: &dyn SeedCipher,
) -> Result<ProfileIdentity, UnlockRefused> {
    let Some(record) = private_bytes(paths)? else {
        return Err(UnlockRefused::Unprovisioned);
    };
    let parsed = parse(&record)
        .map_err(|refused| UnlockRefused::RecoveryRequired(RecoveryCause::Record(refused)))?;
    let seed = cipher
        .open(
            &KeyRef::new(paths, parsed.policy),
            &parsed.iv,
            &parsed.sealed,
            &aad(parsed.policy, &parsed.peer),
        )
        .map_err(|failure| match failure {
            CipherFailure::KeyInvalidated => {
                UnlockRefused::RecoveryRequired(RecoveryCause::KeyInvalidated)
            }
            CipherFailure::KeyMissing => UnlockRefused::RecoveryRequired(RecoveryCause::KeyMissing),
            CipherFailure::Authentication => {
                UnlockRefused::RecoveryRequired(RecoveryCause::Authentication)
            }
            CipherFailure::UserNotAuthenticated => UnlockRefused::UserNotAuthenticated,
            CipherFailure::Unavailable(detail) => UnlockRefused::Unavailable(detail),
        })?;
    match identity_of(&seed) {
        Some((identity, peer)) if peer.as_str() == parsed.peer.as_str() => Ok(identity),
        _ => Err(UnlockRefused::RecoveryRequired(
            RecoveryCause::WrongIdentity,
        )),
    }
}

/// Seal `identity`'s seed for `policy` and frame the record.
fn seal_record(
    paths: &ProfilePaths,
    cipher: &dyn SeedCipher,
    policy: KeyUnlockPolicy,
    identity: &ProfileIdentity,
) -> Result<(TransportIdentity, Vec<u8>), CustodyRefused> {
    let unusable =
        |e: interweave_profile_identity::IdentityError| CustodyRefused::Identity(e.to_string());
    let peer = identity.transport_identity().map_err(unusable)?;
    let seed = Seed::new(
        identity
            .recovery_phrase()
            .map_err(unusable)?
            .expose_entropy()
            .map_err(unusable)?,
    );
    let Sealed { iv, sealed } = cipher
        .seal(&KeyRef::new(paths, policy), &aad(policy, &peer), &seed)
        .map_err(CustodyRefused::Cipher)?;
    if iv.len() != IV_LEN || sealed.len() != SEALED_LEN {
        return Err(CustodyRefused::CipherShape);
    }
    let mut record = Vec::with_capacity(ENVELOPE_LEN + peer.as_str().len());
    record.extend_from_slice(MAGIC);
    record.push(VERSION);
    record.push(policy_byte(policy));
    record.extend_from_slice(&iv);
    record.extend_from_slice(&sealed);
    record.extend_from_slice(peer.as_str().as_bytes());
    Ok((peer, record))
}

/// Store a NEW profile's identity: sealed for `policy` and written
/// owner-only under the profile's lock ([`hold`]), refusing if the
/// profile already has a record. The lock keeps a second first run from
/// sealing at all; the exclusive create is what refuses a record that
/// appeared anyway.
///
/// # Errors
/// [`CustodyRefused::AlreadyProvisioned`] when a record exists,
/// [`CustodyRefused::ProfileLocked`] while another holds the profile;
/// the others as named.
pub fn provision(
    paths: &ProfilePaths,
    cipher: &dyn SeedCipher,
    identity: &ProfileIdentity,
    policy: KeyUnlockPolicy,
) -> Result<TransportIdentity, CustodyRefused> {
    let _held = hold(paths)?;
    // Checked before sealing as well, because a seal replaces the
    // profile's wrapping keys: sealing first would leave the existing
    // record unopenable even though the write is then refused.
    if custody_present(paths)? {
        return Err(CustodyRefused::AlreadyProvisioned);
    }
    let (peer, record) = seal_record(paths, cipher, policy, identity)?;
    match create_private_exclusive_within(&custody_file(paths), &record, paths.boundary()) {
        Ok(()) => Ok(peer),
        Err(PersistError::AlreadyExists) => Err(CustodyRefused::AlreadyProvisioned),
        Err(e) => Err(CustodyRefused::Storage(e)),
    }
}

/// The profile's lock, taken without waiting, for the whole of a flow
/// that seals. A seal rotates the profile's keys, so check-seal-create is
/// not atomic by itself: two first runs could both pass the check, the
/// second seal replacing the key the first's record -- the one the
/// exclusive create keeps -- was sealed under, and BOTH lose. Held, the
/// second is refused before it seals; and a running host holds it, so
/// recovery runs with the profile exclusively locked, as the custody
/// doc requires. A kernel lock: a flow that dies releases it.
fn hold(paths: &ProfilePaths) -> Result<ProfileLock, CustodyRefused> {
    ProfileLock::acquire(paths, Duration::ZERO).map_err(|e| match e {
        PersistError::ProfileLocked { .. } => CustodyRefused::ProfileLocked,
        e => CustodyRefused::Storage(e),
    })
}

/// Whether a record entry exists, whatever it holds.
fn custody_present(paths: &ProfilePaths) -> Result<bool, CustodyRefused> {
    match std::fs::symlink_metadata(custody_file(paths)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(CustodyRefused::Storage(PersistError::Io(e))),
    }
}

/// Re-store the identity `phrase` restores, sealed for `policy` under a
/// fresh wrapping key -- ONLY if it restores `expected` (ADR-0033's
/// mandatory expected `PeerId`), and only over a record whose `PeerId`
/// field names `expected` or holds no `PeerId` at all -- whatever its
/// mode, header or ciphertext, since those say nothing about whose it
/// is. A record that cannot be read (a link, another kind of entry, a
/// directory refused) is not replaced: it may name another. Where `expected`
/// comes from is the recovery flow's (step 7). Under the profile's
/// lock, as [`provision`]: refused while a host runs.
///
/// # Errors
/// [`CustodyRefused::OtherIdentity`] for a phrase restoring another
/// `PeerId`; [`CustodyRefused::RecordNamesOther`] when the record names
/// another; [`CustodyRefused::Unreadable`] when it cannot be read;
/// nothing is written in any of these.
pub fn restore(
    paths: &ProfilePaths,
    cipher: &dyn SeedCipher,
    phrase: &RecoveryPhrase,
    expected: &TransportIdentity,
    policy: KeyUnlockPolicy,
) -> Result<ProfileIdentity, CustodyRefused> {
    if ProfileIdentity::verify_phrase(phrase, expected).is_err() {
        return Err(CustodyRefused::OtherIdentity);
    }
    let _held = hold(paths)?;
    // Whose record this is comes from its PeerId field alone (`named_peer`),
    // read whatever the file's mode or the envelope's state; only a record
    // with no PeerId there names nobody. One that cannot be read at all may
    // name another, so nothing is written over it.
    match read_record(paths) {
        Ok(Read::Absent) => {}
        Ok(Read::Bytes { bytes, .. }) => {
            if named_peer(&bytes).is_some_and(|named| named.as_str() != expected.as_str()) {
                return Err(CustodyRefused::RecordNamesOther);
            }
        }
        Err(e) => return Err(CustodyRefused::Unreadable(e.to_string())),
    }
    let identity = ProfileIdentity::from_phrase(phrase)
        .map_err(|e| CustodyRefused::Identity(e.to_string()))?;
    let (_, record) = seal_record(paths, cipher, policy, &identity)?;
    write_private_atomic_within(&custody_file(paths), &record, paths.boundary())
        .map_err(CustodyRefused::Storage)?;
    Ok(identity)
}

/// Bring the record's policy to the profile's configured one: ADR-0042 A
/// 2026-10-10 (ii). The record's policy byte governs OPENING, so a
/// profile whose operator changed `key_unlock_policy` still unlocks; its
/// caller then calls this with the identity [`unlock`] gave back, and the
/// record is sealed again under `policy` -- the seal deleting the
/// profile's keys, the old record retired as a restore retires it.
/// `Ok(false)` and nothing sealed when the record is already under
/// `policy`. Under the profile's lock, as [`provision`]: called after
/// unlock and before the host starts, which takes the lock itself.
///
/// # Errors
/// [`CustodyRefused::NotProvisioned`] with no record;
/// [`CustodyRefused::RecordNamesOther`] when the record does not name
/// `identity`; [`CustodyRefused::Unreadable`] for a record [`unlock`]
/// would refuse before its cipher; nothing is written in any of these.
pub fn reseal(
    paths: &ProfilePaths,
    cipher: &dyn SeedCipher,
    identity: &ProfileIdentity,
    policy: KeyUnlockPolicy,
) -> Result<bool, CustodyRefused> {
    let _held = hold(paths)?;
    let bytes = match private_bytes(paths) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Err(CustodyRefused::NotProvisioned),
        Err(e) => return Err(CustodyRefused::Unreadable(e.to_string())),
    };
    let parsed =
        parse(&bytes).map_err(|refused| CustodyRefused::Unreadable(format!("{refused:?}")))?;
    let own = identity
        .transport_identity()
        .map_err(|e| CustodyRefused::Identity(e.to_string()))?;
    if parsed.peer.as_str() != own.as_str() {
        return Err(CustodyRefused::RecordNamesOther);
    }
    if parsed.policy == policy {
        return Ok(false);
    }
    let (_, record) = seal_record(paths, cipher, policy, identity)?;
    write_private_atomic_within(&custody_file(paths), &record, paths.boundary())
        .map_err(CustodyRefused::Storage)?;
    Ok(true)
}
