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
use crate::limits::CacheLimits;
use crate::record::{AddressObservation, PeerRecord, ProtocolCapabilityObservation};

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

        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(cache),
            Err(e) => {
                cache.quarantine(&e.to_string())?;
                return Ok(cache);
            }
        };

        match serde_json::from_str::<CacheFile>(&text) {
            Ok(file) if file.version == FORMAT_VERSION => {
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
    pub fn record_success(&mut self, peer: &TransportIdentity, address: &str, now_ms: u64) {
        let key = peer.as_str().to_owned();
        let limit = self.limits.max_addresses_per_peer;

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
    ) {
        let Some(record) = self.peers.get_mut(peer.as_str()) else {
            // No record means no successful connection, so there is
            // nothing for this observation to hang off. Creating one here
            // would mint a reachability record out of a protocol fact.
            return;
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
            .truncate(self.limits.max_capabilities_per_peer);
        self.dirty = true;
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
            .filter(|r| r.is_fresh_at(now_ms, self.limits.ttl_ms))
            .filter_map(|r| {
                let peer = TransportIdentity::parse(r.peer_id.clone()).ok()?;
                Some(CandidatePeer {
                    peer_id: peer,
                    addresses: r.addresses.iter().map(|a| a.address.clone()).collect(),
                    source: SOURCE.to_owned(),
                    observed_at: r.last_success_ms,
                    expires_at: Some(r.expires_at_ms(self.limits.ttl_ms)),
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
            .filter(|r| r.is_fresh_at(now_ms, self.limits.ttl_ms))
    }

    /// Drop every expired record.
    ///
    /// Separate from reading, so the read path stays read-only.
    pub fn compact(&mut self, now_ms: u64) -> usize {
        let ttl = self.limits.ttl_ms;
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
        if self.peers.len() <= self.limits.max_peers {
            return;
        }
        let ttl = self.limits.ttl_ms;
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

        let excess = self.peers.len() - self.limits.max_peers;
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
                .is_none_or(|last| now_ms.saturating_sub(last) >= self.limits.write_debounce_ms)
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

        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temp = self.path.with_extension("tmp");
        {
            use std::io::Write as _;
            let mut handle = fs::File::create(&temp)?;
            handle.write_all(&json)?;
            // Explicit, because `fs::write` does not do this. Without it
            // the rename can land before the bytes, and a crash in that
            // window leaves a correctly-named empty file — which is worse
            // than a missing one, since it looks like a valid cache.
            handle.sync_all()?;
        }
        fs::rename(&temp, &self.path)?;

        self.dirty = false;
        self.last_write_ms = Some(now_ms);
        Ok(())
    }
}
