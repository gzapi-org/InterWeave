// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The bounded advisory peer cache.
//!
//! # Bounded means bounded at every level
//!
//! Peers, addresses per peer, and capability observations per peer all
//! have caps, and every cap evicts by LEAST RECENT USEFULNESS rather
//! than by insertion order. That distinction is not cosmetic: an
//! insertion-ordered cache evicts the entry currently being used and
//! keeps an untouched newer one, which is the opposite of what a cache
//! is for.
//!
//! # Clock-free
//!
//! Every expiry, debounce, and eviction decision takes `now_ms`. A
//! seven-day TTL is then testable in a microsecond, and no test has to
//! sleep to reach an interesting state.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use interweave_discovery_api::CandidatePeer;
use interweave_transport_api::TransportIdentity;
use serde::{Deserialize, Serialize};

use crate::CacheError;
use crate::limits::{CacheLimits, MAX_ADDRESS_BYTES, MAX_CACHE_FILE_BYTES, MAX_LABEL_BYTES};
use crate::record::{AddressObservation, PeerRecord, ProtocolCapabilityObservation};

/// A temporary path beside `path`, unique to this attempt.
///
/// The same construction `profile-config`'s persist module uses, and
/// for the same reason: a fixed `.tmp` name is shared by every process
/// writing the same profile, so two concurrent flushes hold
/// descriptors on one inode and one renames it away while the other is
/// still writing. Process id and a per-process counter separate
/// writers; the `RandomState` component means a name another account
/// could otherwise predict -- and pre-create, turning every write into
/// a failure against `create_new` -- is not computable from outside.
///
/// Not a CSPRNG and does not need to be: the requirement is that the
/// name cannot be computed by someone else, not that it resists
/// cryptanalysis.
fn temp_beside(path: &Path) -> PathBuf {
    use std::hash::{BuildHasher as _, Hasher as _};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    static SEED: std::sync::OnceLock<std::collections::hash_map::RandomState> =
        std::sync::OnceLock::new();

    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut h = SEED
        .get_or_init(std::collections::hash_map::RandomState::new)
        .build_hasher();
    h.write_u64(n);
    h.write_u32(std::process::id());

    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(
        ".{}.{n}.{:016x}.tmp",
        std::process::id(),
        h.finish()
    ));
    PathBuf::from(temp)
}

/// The provider name this cache reports on every candidate it emits.
pub const SOURCE: &str = "peer-cache";

/// The on-disk format version.
pub const FORMAT_VERSION: u32 = 1;

/// Whether the cache is backed by a file it could read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheHealth {
    /// Loaded, or started empty because no file existed yet.
    Healthy,
    /// The file was unreadable and has been quarantined.
    ///
    /// The cache CONTINUES, empty. A corrupt advisory cache must cost a
    /// cold start, never a failed startup — this is the one piece of
    /// state whose loss is genuinely harmless, and treating it as fatal
    /// would make it the least harmless.
    Quarantined {
        /// Where the unreadable file was moved.
        quarantined_to: PathBuf,
        /// Why it could not be read.
        reason: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    peers: Vec<PeerRecord>,
}

/// Bounded advisory persistence for peer reachability.
#[derive(Debug)]
pub struct PeerCache {
    path: PathBuf,
    limits: CacheLimits,
    peers: BTreeMap<String, PeerRecord>,
    health: CacheHealth,
    dirty: bool,
    last_write_ms: Option<u64>,
}

/// One bounded string on the way IN.
///
/// The mirror of the load-path check, and the reason the two exist
/// together: a value only one of them accepts is a file that writes
/// cleanly and refuses to load.
fn bounded(field: &'static str, value: &str, max: usize) -> Result<(), CacheError> {
    if !is_bounded_label(value, max) {
        return Err(CacheError::OutOfBounds {
            field,
            got: value.len(),
            max,
        });
    }
    Ok(())
}

/// Whether a value is inside the bounded format: 1..=`max` bytes of
/// printable ASCII.
///
/// # Why the character set is part of the SIZE bound
///
/// [`MAX_CACHE_FILE_BYTES`] is derived from these limits, and a stored
/// byte is not a serialized byte. Within printable ASCII the worst JSON
/// expansion is `"` and `\` at two bytes each; a control character
/// encodes as six (`\u0000`), so a cache of 128-byte control-character
/// labels passes every length check and serializes to three times the
/// ceiling — `flush` succeeding and the next `load` quarantining, which
/// is the failure this pair of bounds exists to prevent.
///
/// Refusing them costs nothing: these are opaque values compared exactly
/// and never parsed, so a control byte buys a peer no expressiveness it
/// can use — only room to make the file bigger than the format says it
/// can be.
fn is_bounded_label(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

/// Read at most `limit` bytes of UTF-8 from `path`.
///
/// `Ok(None)` when the file is absent. An error when it is larger than
/// `limit` or is not UTF-8 — both are "this is not the format", which
/// for disposable advisory state means quarantine rather than repair.
fn read_capped(path: &Path, limit: u64) -> Result<Option<String>, std::io::Error> {
    use std::io::Read as _;

    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    // One byte past the limit is all it takes to know it is over, and it
    // is the only byte past the limit this ever holds.
    let mut buf = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut buf)?;
    if buf.len() as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("larger than the {limit}-byte ceiling for this format"),
        ));
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// Whether one on-disk record is inside the bounded format.
///
/// Returns the reason it is not, for the quarantine message. Checks the
/// things a `String`/`Vec` shape cannot: a canonical PeerId, bounded
/// labels, the per-peer counts, and timestamps that do not contradict
/// each other.
fn validate_record(record: &PeerRecord, limits: CacheLimits) -> Result<(), String> {
    let id = &record.peer_id;
    TransportIdentity::parse(id.clone())
        .map_err(|e| format!("peer id {id:?} is not canonical: {e}"))?;

    if record.addresses.len() > limits.max_addresses_per_peer() {
        return Err(format!(
            "peer {id} has {} addresses; the cache retains {}",
            record.addresses.len(),
            limits.max_addresses_per_peer()
        ));
    }
    for a in &record.addresses {
        if !is_bounded_label(&a.address, MAX_ADDRESS_BYTES) {
            return Err(format!(
                "peer {id} has a {}-byte address; the limit is 1..={MAX_ADDRESS_BYTES}",
                a.address.len()
            ));
        }
    }

    if record.capabilities.len() > limits.max_capabilities_per_peer() {
        return Err(format!(
            "peer {id} has {} capability observations; the cache retains {}",
            record.capabilities.len(),
            limits.max_capabilities_per_peer()
        ));
    }
    for c in &record.capabilities {
        for (what, value) in [
            ("protocol_family", &c.protocol_family),
            ("network_hash", &c.network_hash),
            ("role", &c.role),
        ] {
            if !is_bounded_label(value, MAX_LABEL_BYTES) {
                return Err(format!(
                    "peer {id} has a {}-byte {what}; the limit is 1..={MAX_LABEL_BYTES}",
                    value.len()
                ));
            }
        }
    }

    // A record that last succeeded before it first succeeded is not
    // merely odd: `last_success_ms` is what the TTL runs from, so the
    // pair decides whether this entry is live.
    if record.last_success_ms < record.first_success_ms {
        return Err(format!(
            "peer {id} last succeeded at {} but first succeeded at {}",
            record.last_success_ms, record.first_success_ms
        ));
    }
    Ok(())
}

impl PeerCache {
    /// Load the cache at `path`, or start empty.
    ///
    /// A missing file is normal — the cache is safe to delete, so its
    /// absence is the expected state on a first run and after a user
    /// clears it.
    ///
    /// An UNREADABLE file is quarantined by rename and the cache starts
    /// empty with [`CacheHealth::Quarantined`]. The bad file is kept
    /// rather than deleted so it can be inspected; the caller is
    /// expected to report degradation and carry on.
    ///
    /// # Errors
    /// Returns [`CacheError`] only if the quarantine rename itself
    /// fails, which means the directory is not writable and the caller
    /// has a real problem rather than a stale cache.
    pub fn load(path: &Path, limits: CacheLimits) -> Result<Self, CacheError> {
        let mut cache = Self {
            path: path.to_path_buf(),
            limits,
            peers: BTreeMap::new(),
            health: CacheHealth::Healthy,
            dirty: false,
            last_write_ms: None,
        };

        // SIZE BEFORE BYTES. This is disposable advisory state that a
        // restore, a copy, or a local edit can replace, and it was being
        // read whole into memory before anything asked how big it was —
        // so the file decided the allocation.
        match fs::metadata(path) {
            Ok(meta) if meta.len() > MAX_CACHE_FILE_BYTES => {
                cache.quarantine(&format!(
                    "{} bytes exceeds the {MAX_CACHE_FILE_BYTES}-byte ceiling for this format",
                    meta.len()
                ))?;
                return Ok(cache);
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(cache),
            Err(e) => {
                cache.quarantine(&e.to_string())?;
                return Ok(cache);
            }
        }

        // And read through the ceiling anyway, because the file may grow
        // between the two calls and a check that can be raced is not a
        // bound. One byte over the limit is enough to know.
        let text = match read_capped(path, MAX_CACHE_FILE_BYTES) {
            Ok(Some(text)) => text,
            Ok(None) => return Ok(cache),
            Err(e) => {
                cache.quarantine(&e.to_string())?;
                return Ok(cache);
            }
        };

        match serde_json::from_str::<CacheFile>(&text) {
            Ok(file) if file.version == FORMAT_VERSION => {
                // EVERY RECORD, BEFORE INSERTION. Syntax and a version
                // number say the file is JSON of about the right shape;
                // they say nothing about whether a peer id is canonical,
                // an address is bounded, or a count is inside the limits
                // this cache advertises. A record that fails is not
                // skipped — the file is quarantined, because a cache
                // half of which was rejected is not the cache the caller
                // thinks it loaded.
                if file.peers.len() > limits.max_peers() {
                    cache.quarantine(&format!(
                        "{} peers exceeds the {} the cache retains",
                        file.peers.len(),
                        limits.max_peers()
                    ))?;
                    return Ok(cache);
                }
                for record in &file.peers {
                    if let Err(reason) = validate_record(record, limits) {
                        cache.quarantine(&reason)?;
                        return Ok(cache);
                    }
                }
                for record in file.peers {
                    cache.peers.insert(record.peer_id.clone(), record);
                }
            }
            Ok(file) => {
                cache.quarantine(&format!(
                    "on-disk format version {} is not {FORMAT_VERSION}",
                    file.version
                ))?;
            }
            Err(e) => cache.quarantine(&e.to_string())?,
        }
        Ok(cache)
    }

    fn quarantine(&mut self, reason: &str) -> Result<(), CacheError> {
        let target = self.path.with_extension("corrupt");
        // Best-effort rename. If the file vanished under us there is
        // nothing to quarantine and nothing has gone wrong.
        match fs::rename(&self.path, &target) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(CacheError::Io(e)),
        }
        self.peers.clear();
        self.health = CacheHealth::Quarantined {
            quarantined_to: target,
            reason: reason.to_owned(),
        };
        Ok(())
    }

    /// Whether the backing file was readable.
    #[must_use]
    pub const fn health(&self) -> &CacheHealth {
        &self.health
    }

    /// How many peers are retained, expired ones included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Whether the cache holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Record that a dial to `peer` at `address` succeeded.
    ///
    /// This is the only thing that extends a record's TTL, which is why
    /// the cache decays toward the peers that actually work rather than
    /// the peers that were once mentioned.
    pub fn record_success(
        &mut self,
        peer: &TransportIdentity,
        address: &str,
        now_ms: u64,
    ) -> Result<(), CacheError> {
        // BOUNDED HERE, not only on the way back in. `load` validates
        // every record, so an over-long address accepted at this point
        // becomes a file `flush` writes and the next `load` quarantines
        // — the cache discarding everything it held because of a value
        // it had already agreed to store.
        bounded("address", address, MAX_ADDRESS_BYTES)?;
        let key = peer.as_str().to_owned();
        let limit = self.limits.max_addresses_per_peer();

        let record = self.peers.entry(key.clone()).or_insert_with(|| PeerRecord {
            peer_id: key,
            addresses: Vec::new(),
            first_success_ms: now_ms,
            last_success_ms: now_ms,
            last_failure_ms: None,
            capabilities: Vec::new(),
        });
        record.last_success_ms = record.last_success_ms.max(now_ms);

        match record.addresses.iter_mut().find(|a| a.address == address) {
            Some(existing) => existing.last_success_ms = existing.last_success_ms.max(now_ms),
            None => record.addresses.push(AddressObservation {
                address: address.to_owned(),
                last_success_ms: now_ms,
            }),
        }
        // Most recently successful first, then drop the tail. The address
        // evicted at the cap is the one that has worked least recently.
        record
            .addresses
            .sort_by_key(|a| core::cmp::Reverse(a.last_success_ms));
        record.addresses.truncate(limit);

        self.dirty = true;
        self.enforce_peer_cap(now_ms);
        Ok(())
    }

    /// Record that a dial to `peer` failed.
    ///
    /// Diagnostic only. A failure does NOT shorten the TTL or drop
    /// addresses: this cache is advisory, and a peer that is merely
    /// offline right now is exactly the peer whose addresses are worth
    /// keeping until they expire on their own.
    pub fn record_failure(&mut self, peer: &TransportIdentity, now_ms: u64) {
        if let Some(record) = self.peers.get_mut(peer.as_str()) {
            record.last_failure_ms = Some(now_ms);
            self.dirty = true;
        }
    }

    /// Record an authenticated protocol capability observation.
    ///
    /// A fresh observation SUPERSEDES an earlier one for the same
    /// family, wire major, network hash, and role — including flipping
    /// `supported` from true to false, which is how a peer that stopped
    /// running a Kademlia server stops being targeted. Superseding
    /// rather than appending is also what stops one peer's changing mind
    /// from consuming the whole capability budget.
    ///
    /// Only call this for facts learned on an AUTHENTICATED connection.
    /// Nothing here can check that, which is why it is stated: an
    /// unauthenticated observation would let a peer write claims about
    /// itself into local state.
    pub fn record_capability(
        &mut self,
        peer: &TransportIdentity,
        observation: ProtocolCapabilityObservation,
    ) -> Result<(), CacheError> {
        bounded(
            "protocol_family",
            &observation.protocol_family,
            MAX_LABEL_BYTES,
        )?;
        bounded("network_hash", &observation.network_hash, MAX_LABEL_BYTES)?;
        bounded("role", &observation.role, MAX_LABEL_BYTES)?;

        let Some(record) = self.peers.get_mut(peer.as_str()) else {
            // No record means no successful connection, so there is
            // nothing for this observation to hang off. Creating one here
            // would mint a reachability record out of a protocol fact.
            return Ok(());
        };

        let same = |o: &ProtocolCapabilityObservation| {
            o.protocol_family == observation.protocol_family
                && o.wire_major == observation.wire_major
                && o.network_hash == observation.network_hash
                && o.role == observation.role
        };
        match record.capabilities.iter_mut().find(|o| same(o)) {
            Some(existing) => *existing = observation,
            None => record.capabilities.push(observation),
        }
        record
            .capabilities
            .sort_by_key(|o| core::cmp::Reverse(o.observed_at_ms));
        record
            .capabilities
            .truncate(self.limits.max_capabilities_per_peer());
        self.dirty = true;
        Ok(())
    }

    /// Every peer still fresh at `now_ms`, as advisory candidates.
    ///
    /// Stale entries are IGNORED here rather than deleted, and compacted
    /// separately by [`PeerCache::compact`]. Reading must not be a write
    /// path: a read that mutated would make every lookup a disk write.
    ///
    /// The result is a [`CandidatePeer`], which is advisory by
    /// definition — holding one implies nothing about trust or current
    /// reachability. A cached peer is never trusted because it was
    /// cached.
    #[must_use]
    pub fn candidates(&self, now_ms: u64) -> Vec<CandidatePeer> {
        self.peers
            .values()
            .filter(|r| r.is_fresh_at(now_ms, self.limits.ttl_ms()))
            .filter_map(|r| {
                let peer = TransportIdentity::parse(r.peer_id.clone()).ok()?;
                Some(CandidatePeer {
                    peer_id: peer,
                    addresses: r.addresses.iter().map(|a| a.address.clone()).collect(),
                    source: SOURCE.to_owned(),
                    observed_at: r.last_success_ms,
                    expires_at: Some(r.expires_at_ms(self.limits.ttl_ms())),
                    // EMPTY, AND THIS IS A KNOWN GAP -- not "this peer
                    // has no observations".
                    //
                    // `providers/peer-cache.md` says the Kademlia
                    // provider may read fresh capability observations
                    // "through normal candidate/hint data", which is
                    // this field. Nothing reads it yet: Kademlia is
                    // Stage 10 and the discovery manager is later, so
                    // the gap is not live.
                    //
                    // It is left empty rather than filled because the
                    // mapping is NOT specified and guessing it here
                    // would freeze a wire-adjacent decision in the
                    // wrong place. A stored observation is
                    // `(protocol_family, wire_major, network_hash,
                    // role)`; a `ProtocolObservation` carries a single
                    // `protocol_id`. ADR-0047 gives the canonical form
                    // `/interweave/kad/1.0.0/<network-hash>`, so three
                    // of those four fields have an evident home and
                    // `role` has none -- and "wire_major 1 means 1.0.0"
                    // is an inference, not something any document
                    // states. Dropping `role` silently would be the
                    // same class of loss as dropping the whole set.
                    //
                    // Whoever opens Stage 10 decides the mapping in the
                    // architecture first, then fills this in.
                    protocol_observations: Default::default(),
                })
            })
            .collect()
    }

    /// The record for one peer, if it is still fresh.
    #[must_use]
    pub fn peer(&self, peer: &TransportIdentity, now_ms: u64) -> Option<&PeerRecord> {
        self.peers
            .get(peer.as_str())
            .filter(|r| r.is_fresh_at(now_ms, self.limits.ttl_ms()))
    }

    /// Drop every expired record.
    ///
    /// Separate from reading, so the read path stays read-only.
    pub fn compact(&mut self, now_ms: u64) -> usize {
        let ttl = self.limits.ttl_ms();
        let before = self.peers.len();
        self.peers.retain(|_, r| r.is_fresh_at(now_ms, ttl));
        let dropped = before - self.peers.len();
        if dropped > 0 {
            self.dirty = true;
        }
        dropped
    }

    /// Evict down to the peer cap, expired records first.
    ///
    /// Expired entries go before live ones regardless of age, because
    /// evicting a working peer while an expired one sits in the file is
    /// strictly worse. Among live entries the least recently successful
    /// goes — the one whose route is least likely to still work.
    fn enforce_peer_cap(&mut self, now_ms: u64) {
        if self.peers.len() <= self.limits.max_peers() {
            return;
        }
        let ttl = self.limits.ttl_ms();
        let mut ranked: Vec<(bool, u64, String)> = self
            .peers
            .values()
            .map(|r| {
                (
                    r.is_fresh_at(now_ms, ttl),
                    r.last_success_ms,
                    r.peer_id.clone(),
                )
            })
            .collect();
        // Expired (false) first, then oldest success first.
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

        let excess = self.peers.len() - self.limits.max_peers();
        for (_, _, key) in ranked.into_iter().take(excess) {
            self.peers.remove(&key);
        }
    }

    /// Whether a write is due under the debounce.
    #[must_use]
    pub fn write_due(&self, now_ms: u64) -> bool {
        self.dirty
            && self
                .last_write_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= self.limits.write_debounce_ms())
    }

    /// Write if the debounce interval has elapsed.
    ///
    /// Returns whether a write happened. A burst of successful dials on
    /// startup would otherwise rewrite the whole file once per
    /// connection.
    ///
    /// # Errors
    /// Returns [`CacheError`] if the write fails.
    pub fn flush_if_due(&mut self, now_ms: u64) -> Result<bool, CacheError> {
        if !self.write_due(now_ms) {
            return Ok(false);
        }
        self.flush(now_ms)?;
        Ok(true)
    }

    /// Write now, regardless of the debounce.
    ///
    /// The write is atomic: a temporary file in the same directory,
    /// fsynced, then renamed over the target. A half-written cache would
    /// be quarantined on the next load, which costs a cold start —
    /// survivable, but avoidable for the price of a rename.
    ///
    /// # Errors
    /// Returns [`CacheError`] if the directory is not writable or the
    /// rename fails.
    pub fn flush(&mut self, now_ms: u64) -> Result<(), CacheError> {
        let file = CacheFile {
            version: FORMAT_VERSION,
            peers: self.peers.values().cloned().collect(),
        };
        let json = serde_json::to_vec_pretty(&file)?;

        // MEASURED BEFORE IT IS PUBLISHED. Every other bound in this
        // crate is on a value; this is the one on the result, and it is
        // what makes "a cache never writes a file it cannot read" true
        // rather than merely argued. The per-value bounds imply it for a
        // cache built through the public API, and implication is not the
        // same as a check -- the derivation behind MAX_CACHE_FILE_BYTES
        // has already been wrong once.
        let size = json.len() as u64;
        if size > MAX_CACHE_FILE_BYTES {
            return Err(CacheError::TooLargeToPublish {
                got: size,
                max: MAX_CACHE_FILE_BYTES,
            });
        }

        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;

        // A UNIQUE NAME, and `create_new`. The temporary used to be
        // `self.path.with_extension("tmp")` opened with `File::create`:
        // one fixed name per profile, shared by every process writing
        // that profile's cache. Two of them flush concurrently and both
        // hold descriptors on the same inode -- one renames it into
        // place while the other is still writing through its own
        // descriptor, so the published file is a splice of two
        // serializations. The peer cache is advisory, so this is not
        // identity corruption; it is a whole cache invalidated and an
        // unexplained cold start, which is exactly the cost the rename
        // was there to avoid.
        //
        // `create_new` refuses an existing file and refuses to follow a
        // symlink, so a name someone pre-created is an error here
        // rather than a write somewhere else.
        let temp = temp_beside(&self.path);
        // EVERY FAILURE AFTER CREATION REMOVES IT, not only a failed
        // rename. The name is unique per attempt, so an abandoned
        // temporary is never reused and never overwritten: a full disk
        // or a transient I/O error during `write_all` or `sync_all`
        // would leave one file behind per failed flush, forever, in the
        // profile directory. Writing this as one fallible block and
        // cleaning up on any `Err` is what makes that impossible to
        // reintroduce by adding a step -- a `?` inside the block cannot
        // escape the cleanup.
        let published = (|| -> Result<(), CacheError> {
            use std::io::Write as _;
            let mut handle = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            handle.write_all(&json)?;
            // Explicit, because `fs::write` does not do this. Without it
            // the rename can land before the bytes, and a crash in that
            // window leaves a correctly-named empty file — which is worse
            // than a missing one, since it looks like a valid cache.
            handle.sync_all()?;
            drop(handle);
            fs::rename(&temp, &self.path)?;
            Ok(())
        })();
        if let Err(error) = published {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }

        // fsync the DIRECTORY, so the rename itself survives a crash.
        // Without it the bytes are durable and the name that points at
        // them may not be, which is the same cold start by a different
        // route. Best-effort: a filesystem that refuses to open a
        // directory for this is not a reason to fail a flush that has
        // already landed.
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }

        self.dirty = false;
        self.last_write_ms = Some(now_ms);
        Ok(())
    }
}
