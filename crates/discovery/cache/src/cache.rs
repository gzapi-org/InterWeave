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

/// Write `json` to `temp`, fsync it, and rename it onto `final_path`.
///
/// REMOVES `temp` ON ANY FAILURE AFTER THIS CALL CREATED IT, and never
/// otherwise. Both halves are load-bearing.
///
/// The first half: the name is unique per attempt, so an abandoned
/// temporary is never reused and never overwritten -- a full disk or a
/// transient error during `write_all` or `sync_all` would leave one file
/// behind per failed flush, forever, in the profile directory. Every
/// fallible step after creation sits in one block whose `Err` runs the
/// cleanup, so adding a step cannot escape it.
///
/// The second half is the one that was missing. `create_new` refuses an
/// existing file, so a failure at THAT step means this call did not
/// create the temporary -- a cross-process name collision, or an entry
/// another writer is mid-flush on. Removing it there deletes a file this
/// invocation never owned and breaks the rename its owner is about to
/// do. Creation succeeding is what confers the right to clean up, so the
/// handle is opened outside the block rather than inside it.
///
/// Enforced by `a_temporary_this_call_did_not_create_is_left_alone` and
/// `a_failure_after_creation_removes_the_temporary`.
fn publish_via_temp(temp: &Path, final_path: &Path, json: &[u8]) -> Result<(), CacheError> {
    use std::io::Write as _;

    // OUTSIDE the cleanup block, deliberately. A failure here is a
    // temporary that belongs to somebody else.
    let mut handle = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)?;

    // From here on the file is ours, so every exit removes it.
    let written = (|| -> Result<(), CacheError> {
        handle.write_all(json)?;
        // Explicit, because `fs::write` does not do this. Without it the
        // rename can land before the bytes, and a crash in that window
        // leaves a correctly-named empty file -- worse than a missing
        // one, since it looks like a valid cache.
        handle.sync_all()?;
        Ok(())
    })();
    drop(handle);

    let published = written.and_then(|()| fs::rename(temp, final_path).map_err(CacheError::from));
    if let Err(error) = published {
        let _ = fs::remove_file(temp);
        return Err(error);
    }
    Ok(())
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
                    // THE STAGE 10 MAPPING, decided 2026-08-30 in the
                    // architecture first (`kademlia-integration.md` §7)
                    // and only then implemented here: a fresh
                    // `interweave/kad` SERVER capability exports as the
                    // exact protocol string a server advertises,
                    // `/interweave/kad/<wire_major>.0.0/<network_hash>`
                    // — role carried by presence, the other three
                    // fields by the string itself. Other families and
                    // roles have no wire form yet and stay unexported
                    // rather than being guessed at.
                    //
                    // A NEGATIVE observation exports as
                    // `supported: false`, never as absence: it is the
                    // suppression signal that stops a targeted lookup
                    // retrying a peer that stopped serving.
                    protocol_observations: r
                        .fresh_capabilities(now_ms, self.limits.ttl_ms())
                        .filter(|c| {
                            c.protocol_family == crate::record::KAD_PROTOCOL_FAMILY
                                && c.role == crate::record::KAD_SERVER_ROLE
                        })
                        .filter_map(|c| {
                            let id = crate::record::kad_server_protocol_id(
                                c.wire_major,
                                &c.network_hash,
                            );
                            Some(interweave_discovery_api::ProtocolObservation {
                                protocol_id: interweave_discovery_api::ProtocolId::parse(id)
                                    .ok()?,
                                supported: c.supported,
                                observed_at: c.observed_at_ms,
                            })
                        })
                        .take(interweave_discovery_api::MAX_PROTOCOL_OBSERVATIONS)
                        .collect(),
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
    /// Returns [`CacheError`] if the directory is not writable, the
    /// rename fails, or the directory cannot be fsynced afterwards — the
    /// last meaning the bytes are on disk and the name pointing at them
    /// may not survive a crash.
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
        publish_via_temp(&temp, &self.path, &json)?;

        // fsync the DIRECTORY, so the rename itself survives a crash.
        // Without it the bytes are durable and the name that points at
        // them may not be, which is the same cold start by a different
        // route.
        //
        // REPORTED, not discarded. This was written as best-effort on
        // the grounds that a flush which has already landed should not
        // fail — but a discarded error does not make the flush more
        // durable, it only makes the caller believe it is. `flush`
        // already returns an error for a failed write or rename, so a
        // failure to make that rename durable belongs in the same
        // channel. What the CALLER does about it is still its decision:
        // for this cache the loss is a cold start, and a caller that
        // knows that can carry on.
        let dir = fs::File::open(parent).map_err(CacheError::Io)?;
        dir.sync_all().map_err(CacheError::Io)?;

        self.dirty = false;
        self.last_write_ms = Some(now_ms);
        Ok(())
    }
}

#[cfg(test)]
mod publish_tests {
    use super::publish_via_temp;
    use std::fs;

    use super::{CacheError, CacheLimits, PeerCache};
    use interweave_transport_api::TransportIdentity;

    /// A directory whose fsync cannot succeed, without a race.
    ///
    /// `0o300` is write plus execute and NO READ: `rename` into it still
    /// works, and `File::open` on the directory itself fails with
    /// EACCES. That is precisely the failure the flush used to discard.
    /// Returns false when the mode did not actually block anything —
    /// running as root, or a filesystem that ignores it — so the test
    /// says so rather than passing vacuously.
    #[cfg(unix)]
    fn make_unreadable(dir: &std::path::Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o300)).expect("chmod");
        fs::File::open(dir).is_err()
    }

    #[cfg(unix)]
    #[test]
    fn a_flush_whose_directory_cannot_be_synced_says_so() {
        // The bytes reach the disk and the NAME pointing at them may not
        // survive a crash. `let _ = dir.sync_all()` reported that as a
        // successful flush, which does not make the rename durable — it
        // only makes the caller believe it is.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache = PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default())
            .expect("a fresh cache loads");
        let peer = TransportIdentity::parse("12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN")
            .expect("valid identity");
        cache
            .record_success(&peer, "/ip4/10.0.0.1/tcp/1", 0)
            .expect("recorded");

        // POSITIVE CONTROL FIRST: the same flush succeeds while the
        // directory is readable, so the failure below is about the
        // permission and not about the cache.
        cache.flush(0).expect("an ordinary flush succeeds");

        cache
            .record_success(&peer, "/ip4/10.0.0.2/tcp/1", 1)
            .expect("recorded");
        if !make_unreadable(dir.path()) {
            // Root, or a filesystem that ignores the mode. Say so rather
            // than assert something this environment cannot show.
            println!("skipped: 0o300 did not block opening the directory here");
            return;
        }
        let refused = cache.flush(1);
        // Restore before the assertion, so a failure does not leave an
        // undeletable temporary directory behind.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod back");
        assert!(
            matches!(refused, Err(CacheError::Io(_))),
            "a flush that could not fsync its directory must not report success: {refused:?}"
        );
    }

    /// THE FINDING. `create_new` refuses an existing file, so a failure
    /// at that step means this call did not create the temporary — a
    /// cross-process name collision, or an entry another writer is
    /// mid-flush on. The cleanup used to run anyway and delete it,
    /// breaking the rename its owner was about to do.
    #[test]
    fn a_temporary_this_call_did_not_create_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let temp = dir.path().join("peers.json.someone-elses.tmp");
        let final_path = dir.path().join("peers.json");
        fs::write(&temp, b"another writer's bytes").expect("pre-create");

        let result = publish_via_temp(&temp, &final_path, b"ours");

        assert!(result.is_err(), "an occupied temp name must refuse");
        assert_eq!(
            fs::read(&temp).expect("the other writer's file still exists"),
            b"another writer's bytes",
            "and its contents are untouched — deleting it would break \
             the rename its owner is about to do"
        );
        assert!(
            !final_path.exists(),
            "and nothing was published from a flush that never wrote"
        );
    }

    /// The other half, which the original did get right and which a
    /// careless fix would lose: once creation succeeds the file IS ours,
    /// so every later failure removes it. Without this the profile
    /// directory accumulates one abandoned temporary per failed flush,
    /// forever, since each name is unique per attempt.
    #[test]
    fn a_failure_after_creation_removes_the_temporary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let temp = dir.path().join("peers.json.ours.tmp");
        // Renaming a file onto a DIRECTORY fails, and it fails after the
        // temporary has been created, written and synced.
        let final_path = dir.path().join("peers.json");
        fs::create_dir(&final_path).expect("make the rename fail");
        fs::write(final_path.join("occupant"), b"x").expect("non-empty");

        let result = publish_via_temp(&temp, &final_path, b"ours");

        assert!(result.is_err(), "the rename cannot succeed");
        assert!(
            !temp.exists(),
            "a temporary this call created is removed when publishing fails"
        );
    }

    #[test]
    fn a_successful_publish_leaves_no_temporary_and_the_bytes_land() {
        let dir = tempfile::tempdir().expect("tempdir");
        let temp = dir.path().join("peers.json.ours.tmp");
        let final_path = dir.path().join("peers.json");

        publish_via_temp(&temp, &final_path, b"the bytes").expect("publishes");

        assert!(!temp.exists(), "the temporary was renamed away");
        assert_eq!(fs::read(&final_path).expect("published"), b"the bytes");
    }
    #[test]
    fn a_server_capability_exports_as_the_frozen_protocol_string() {
        // Round-trip against the FROZEN fixture, not against this
        // crate's own renderer — a mapping that only agrees with itself
        // proves nothing. Every vector in the fixture carries the full
        // protocol string precisely so an implementation can be checked
        // here.
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/kademlia/kad-network-namespace-v1.json"
        ))
        .expect("the frozen fixture parses");

        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache =
            PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default()).expect("loads");
        let peer = TransportIdentity::parse("12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN")
            .expect("valid");
        cache
            .record_success(&peer, "/ip4/10.0.0.1/tcp/1", 1_000)
            .expect("recorded");

        for vector in fixture["vectors"].as_array().expect("vectors") {
            let hash = vector["network_hash"].as_str().expect("hash");
            let frozen = vector["protocol"].as_str().expect("protocol");
            cache
                .record_capability(
                    &peer,
                    crate::record::ProtocolCapabilityObservation {
                        protocol_family: crate::record::KAD_PROTOCOL_FAMILY.to_owned(),
                        wire_major: 1,
                        network_hash: hash.to_owned(),
                        role: crate::record::KAD_SERVER_ROLE.to_owned(),
                        supported: true,
                        observed_at_ms: 1_000,
                    },
                )
                .expect("recorded");
            let candidates = cache.candidates(2_000);
            let exported = candidates
                .iter()
                .find(|c| c.peer_id == peer)
                .expect("the peer is a candidate");
            assert!(
                exported
                    .protocol_observations
                    .iter()
                    .any(|o| o.protocol_id.as_str() == frozen && o.supported),
                "{hash}: expected the frozen string {frozen}, got {:?}",
                exported
                    .protocol_observations
                    .iter()
                    .map(|o| o.protocol_id.as_str())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_negative_capability_exports_as_supported_false_not_absence() {
        // The suppression signal: a peer observed to have STOPPED
        // serving must reach the consumer as `supported: false`, or a
        // targeted lookup keeps retrying a peer the cache knows quit.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache =
            PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default()).expect("loads");
        let peer = TransportIdentity::parse("12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN")
            .expect("valid");
        cache
            .record_success(&peer, "/ip4/10.0.0.1/tcp/1", 1_000)
            .expect("recorded");
        cache
            .record_capability(
                &peer,
                crate::record::ProtocolCapabilityObservation {
                    protocol_family: crate::record::KAD_PROTOCOL_FAMILY.to_owned(),
                    wire_major: 1,
                    network_hash: "ssbtblqj7mexczivog5qfbfjvi".to_owned(),
                    role: crate::record::KAD_SERVER_ROLE.to_owned(),
                    supported: false,
                    observed_at_ms: 1_500,
                },
            )
            .expect("recorded");
        let candidates = cache.candidates(2_000);
        let exported = candidates
            .iter()
            .find(|c| c.peer_id == peer)
            .expect("candidate");
        let observation = exported
            .protocol_observations
            .iter()
            .next()
            .expect("the negative observation is exported, not dropped");
        assert!(!observation.supported);
        assert_eq!(observation.observed_at, 1_500);
    }

    #[test]
    fn only_kad_server_capabilities_have_a_wire_form() {
        // Another family, or another role, has no protocol string yet:
        // exporting a guessed one would freeze a wire-adjacent decision
        // nobody made. They stay stored (supersession still works) and
        // unexported.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache =
            PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default()).expect("loads");
        let peer = TransportIdentity::parse("12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN")
            .expect("valid");
        cache
            .record_success(&peer, "/ip4/10.0.0.1/tcp/1", 1_000)
            .expect("recorded");
        for (family, role) in [
            ("interweave/other", crate::record::KAD_SERVER_ROLE),
            (crate::record::KAD_PROTOCOL_FAMILY, "client"),
        ] {
            cache
                .record_capability(
                    &peer,
                    crate::record::ProtocolCapabilityObservation {
                        protocol_family: family.to_owned(),
                        wire_major: 1,
                        network_hash: "ssbtblqj7mexczivog5qfbfjvi".to_owned(),
                        role: role.to_owned(),
                        supported: true,
                        observed_at_ms: 1_000,
                    },
                )
                .expect("stored");
        }
        let candidates = cache.candidates(2_000);
        let exported = candidates
            .iter()
            .find(|c| c.peer_id == peer)
            .expect("candidate");
        assert!(
            exported.protocol_observations.is_empty(),
            "no wire form exists for these: {:?}",
            exported.protocol_observations
        );
    }

    #[test]
    fn the_record_ttl_empties_the_capability_export_too() {
        // Capability freshness never outlives the enclosing record: a
        // capability observed yesterday on a record that expired this
        // morning is evidence about a peer this cache stopped vouching
        // for. Past the TTL the whole candidate disappears, capability
        // included.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache =
            PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default()).expect("loads");
        let peer = TransportIdentity::parse("12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN")
            .expect("valid");
        cache
            .record_success(&peer, "/ip4/10.0.0.1/tcp/1", 1_000)
            .expect("recorded");
        cache
            .record_capability(
                &peer,
                crate::record::ProtocolCapabilityObservation {
                    protocol_family: crate::record::KAD_PROTOCOL_FAMILY.to_owned(),
                    wire_major: 1,
                    network_hash: "ssbtblqj7mexczivog5qfbfjvi".to_owned(),
                    role: crate::record::KAD_SERVER_ROLE.to_owned(),
                    supported: true,
                    observed_at_ms: 1_000,
                },
            )
            .expect("recorded");
        let ttl = CacheLimits::default().ttl_ms();
        assert!(
            !cache
                .candidates(1_000 + ttl + 1)
                .iter()
                .any(|c| c.peer_id == peer),
            "past the record TTL nothing is exported at all"
        );
    }
}
