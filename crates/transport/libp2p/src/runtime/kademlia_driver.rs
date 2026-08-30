// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Swarm-owned Kademlia driver (kademlia-integration.md §20).
//!
//! Consumes [`KademliaCommand`]s from the provider's side of the port
//! and answers with [`KademliaEvent`]s; owns everything libp2p-shaped —
//! `kad::Behaviour`, the §7 admission pipeline, the §4 namespace — so
//! the provider crate never sees a libp2p type.
//!
//! Kademlia here is **peer routing only** (ADR-0009): records are
//! filtered at the behaviour (`StoreInserts::FilterBoth`), inbound
//! write attempts are counted and dropped (§12), and nothing in this
//! module can read or write a record — the port has no command for it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroUsize;
use std::time::Duration;

use libp2p::kad;
use libp2p::kad::store::MemoryStore;
use libp2p::{Multiaddr, PeerId, StreamProtocol, identify};
use sha2::{Digest, Sha256};

use interweave_kademlia_control_api::{
    KademliaCommand, KademliaEvent, KademliaMode, MAX_RESULTS_PER_QUERY, QueryClass, QueryFailure,
};
use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{ConnectionClass, ConnectionManager};

use crate::outbound_gate::strip_peer_suffix;

use super::to_transport_identity;

use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;

/// The identity-multihash envelope of a libp2p Ed25519 public-key
/// protobuf. A `12D3KooW…` PeerId is exactly this envelope around the
/// 32 key bytes, which is what lets a targeted lookup key rebuild the
/// full identity.
const ED25519_ENVELOPE: [u8; 6] = [0x00, 0x24, 0x08, 0x01, 0x12, 0x20];

/// RFC 4648 base32, lower-cased, for the §4 namespace tag.
const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Peers whose Identify advertisement is remembered, at most.
///
/// Bounded because the advertisement map is written by whoever
/// connects: the table it feeds holds `max_routing_peers`, so evidence
/// about four times that is already more than admission can ever use.
const MAX_OBSERVED_PEERS: usize = 1024;

/// Offers waiting for Identify evidence, at most.
const MAX_PENDING_OFFERS: usize = 256;

/// `^[a-z0-9][a-z0-9._-]{0,63}$`, per §4.
#[must_use]
pub fn network_id_is_legal(id: &str) -> bool {
    let mut bytes = id.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    if id.len() > 64 {
        return false;
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// `SHA-256("interweave/kad-network/v1\0" || ASCII(network_id))`,
/// base32 of the first 16 bytes, lower case, unpadded (§4).
///
/// The 16-byte TRUNCATION is load-bearing: hashing to a full digest
/// yields a plausible namespace nobody else computes. Checked against
/// every vector in `fixtures/kademlia/kad-network-namespace-v1.json`.
#[must_use]
pub fn network_hash(network_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"interweave/kad-network/v1\0");
    h.update(network_id.as_bytes());
    let digest = h.finalize();

    let mut out = String::with_capacity(26);
    let mut acc: u32 = 0;
    let mut bits = 0_u32;
    for &b in &digest[..16] {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(BASE32[((acc >> bits) & 0x1f) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(BASE32[((acc << (5 - bits)) & 0x1f) as usize]));
    }
    out
}

/// `/interweave/kad/1.0.0/<network-hash>` (§4).
#[must_use]
pub fn kad_protocol(network_id: &str) -> String {
    format!("/interweave/kad/1.0.0/{}", network_hash(network_id))
}

/// Rebuild the full PeerId a 32-byte targeted-lookup key names.
///
/// The port's key space is the identifier space: for InterWeave
/// identities that is the Ed25519 public key, and the rest of the
/// PeerId is this constant envelope.
#[must_use]
pub fn peer_from_lookup_key(key: [u8; 32]) -> Option<PeerId> {
    let mut bytes = [0_u8; 38];
    bytes[..6].copy_from_slice(&ED25519_ENVELOPE);
    bytes[6..].copy_from_slice(&key);
    PeerId::from_bytes(&bytes).ok()
}

/// Static driver configuration, validated before the Swarm exists.
#[derive(Debug, Clone)]
pub struct KademliaSettings {
    /// Client or server, explicit (§5). Never `auto`.
    pub mode: KademliaMode,
    /// The non-secret deployment namespace (§4).
    pub network_id: String,
    /// K-bucket width (§13).
    pub kbucket_size: NonZeroUsize,
    /// Per-query timeout.
    pub query_timeout: Duration,
    /// Query fan-out.
    pub parallelism: NonZeroUsize,
    /// Whether query paths must be disjoint.
    pub disjoint_query_paths: bool,
    /// Project-level routing-table ceiling, enforced BEFORE manual
    /// insertion (§11) — a population bound, never an address freeze.
    pub max_routing_peers: usize,
    /// Results accepted from one query.
    pub max_results_per_query: NonZeroUsize,
}

impl KademliaSettings {
    /// Refuse a configuration the driver cannot honour.
    ///
    /// # Errors
    /// A static description of the first violated rule.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !network_id_is_legal(&self.network_id) {
            return Err("kademlia network_id is not ^[a-z0-9][a-z0-9._-]{0,63}$");
        }
        if self.max_routing_peers == 0 {
            return Err("kademlia max_routing_peers must admit at least one peer");
        }
        if self.max_results_per_query.get() > MAX_RESULTS_PER_QUERY {
            return Err("kademlia max_results_per_query exceeds the port ceiling");
        }
        Ok(())
    }
}

/// Build the behaviour with the §11 mapping, verbatim from what
/// SPIKE-003 validated against this exact libp2p version.
///
/// # Errors
/// A static description when the derived protocol is not a legal
/// stream protocol — unreachable for settings `validate` accepted, and
/// answered rather than panicked in a transport daemon.
pub fn build_behaviour(
    settings: &KademliaSettings,
    local: PeerId,
) -> Result<kad::Behaviour<MemoryStore>, &'static str> {
    let Ok(protocol) = StreamProtocol::try_from_owned(kad_protocol(&settings.network_id)) else {
        return Err("the derived kademlia protocol is not a legal stream protocol");
    };
    let mut config = kad::Config::new(protocol);
    // MANUAL: merely connecting to a peer must not put it in the DHT
    // routing table (§7); the admission pipeline below is the only way
    // in.
    config.set_kbucket_inserts(kad::BucketInserts::Manual);
    config.set_kbucket_size(settings.kbucket_size);
    config.set_query_timeout(settings.query_timeout);
    config.set_parallelism(settings.parallelism);
    config.disjoint_query_paths(settings.disjoint_query_paths);
    // The provider's scheduler owns bootstrap pacing; inheriting the
    // library's periodic default would run a second, unbudgeted one.
    config.set_periodic_bootstrap_interval(None);
    config.set_caching(kad::Caching::Disabled);
    // Peer routing only: inbound record writes are filtered at the
    // behaviour, before any code of ours could mishandle them (§12).
    config.set_record_filtering(kad::StoreInserts::FilterBoth);
    config.set_publication_interval(None);
    config.set_replication_interval(None);
    config.set_provider_publication_interval(None);
    let mut behaviour = kad::Behaviour::with_config(local, MemoryStore::new(local), config);
    behaviour.set_mode(Some(match settings.mode {
        KademliaMode::Client => kad::Mode::Client,
        KademliaMode::Server => kad::Mode::Server,
    }));
    Ok(behaviour)
}

/// The driver's own bookkeeping beside the behaviour.
#[derive(Debug)]
pub(super) struct KademliaState {
    /// The exact server protocol this network speaks, for Identify
    /// comparison — the FULL string, never a prefix.
    protocol: String,
    max_routing_peers: usize,
    max_results_per_query: usize,
    /// Peers this driver has admitted to the routing table.
    routed: BTreeSet<PeerId>,
    /// Commanded queries in flight, by the class each was issued for.
    queries: HashMap<kad::QueryId, QueryClass>,
    /// Candidates accumulated per query across progress steps.
    results: HashMap<kad::QueryId, Vec<interweave_discovery_api::CandidatePeer>>,
    /// Whether each recently seen peer advertises the exact server
    /// protocol, REPLACED on every Identify — a handler that unioned
    /// could never observe a withdrawal (F5).
    advertises: BTreeMap<PeerId, bool>,
    /// Offered addresses waiting for Identify evidence.
    pending_offers: BTreeMap<PeerId, BTreeSet<Multiaddr>>,
    /// Inbound record writes dropped, counted never stored (§12).
    record_writes_dropped: u64,
    /// Shutting down: new queries are refused.
    stopping: bool,
}

impl KademliaState {
    /// Fresh bookkeeping for one configured driver.
    pub(super) fn new(settings: &KademliaSettings) -> Self {
        Self {
            protocol: kad_protocol(&settings.network_id),
            max_routing_peers: settings.max_routing_peers,
            max_results_per_query: settings.max_results_per_query.get(),
            routed: BTreeSet::new(),
            queries: HashMap::new(),
            results: HashMap::new(),
            advertises: BTreeMap::new(),
            pending_offers: BTreeMap::new(),
            record_writes_dropped: 0,
            stopping: false,
        }
    }

    /// Inbound record writes dropped so far (§12: counted, never stored).
    ///
    /// Test-gated until Stage 12: the §16 diagnostics snapshot is the
    /// production reader, and it was deferred by owner decision until a
    /// consumer exists. The counter itself is maintained regardless.
    #[cfg(test)]
    pub(super) const fn record_writes_dropped(&self) -> u64 {
        self.record_writes_dropped
    }
}

/// Apply one provider command to the behaviour.
///
/// Returns the port events the command produced immediately; most
/// answers arrive later through [`handle_kademlia`].
pub(super) fn handle_command(
    state: &mut KademliaState,
    behaviour: &mut kad::Behaviour<MemoryStore>,
    manager: &ConnectionManager,
    command: KademliaCommand,
    now_ms: u64,
) -> Vec<KademliaEvent> {
    let mut out = Vec::new();
    match command {
        KademliaCommand::SetMode { mode } => {
            // Explicit, never inferred (§5) — and never `auto`.
            behaviour.set_mode(Some(match mode {
                KademliaMode::Client => kad::Mode::Client,
                KademliaMode::Server => kad::Mode::Server,
            }));
        }
        KademliaCommand::OfferRoutingPeer { addresses, peer } => {
            if state.stopping {
                return out;
            }
            let Ok(pid) = peer.as_str().parse::<PeerId>() else {
                return out;
            };
            if manager.is_local_peer(&peer) {
                return out;
            }
            // STASHED, not inserted: an offer is a hint, and §7's
            // pipeline requires authenticated Identify evidence of the
            // exact server protocol before anything reaches the routing
            // table. The addresses wait, bounded, for that evidence.
            if !state.pending_offers.contains_key(&pid)
                && state.pending_offers.len() >= MAX_PENDING_OFFERS
            {
                return out;
            }
            let stash = state.pending_offers.entry(pid).or_default();
            for offered in addresses.as_slice() {
                if stash.len() >= 64 {
                    break;
                }
                if let Ok(addr) = strip_peer_suffix_str(offered.as_str()).parse::<Multiaddr>() {
                    stash.insert(addr);
                }
            }
            out.extend(try_admit(state, behaviour, manager, pid, &peer, now_ms));
        }
        KademliaCommand::StartQuery { class, key } => {
            if state.stopping {
                out.push(KademliaEvent::QueryFailed {
                    class,
                    reason: QueryFailure::ShuttingDown,
                });
                return out;
            }
            match class {
                QueryClass::Bootstrap => match behaviour.bootstrap() {
                    Ok(id) => {
                        state.queries.insert(id, class);
                    }
                    Err(_) => out.push(KademliaEvent::QueryFailed {
                        class,
                        reason: QueryFailure::NoRoutingPeers,
                    }),
                },
                QueryClass::Targeted => match peer_from_lookup_key(key) {
                    Some(target) => {
                        let id = behaviour.get_closest_peers(target);
                        state.queries.insert(id, class);
                    }
                    // The provider refuses untargetable identities
                    // upstream; a key that decodes to no PeerId here is
                    // defence in depth, and the settlement must still
                    // arrive or the budget slot waits forever. "No
                    // routing peers" is the nearest bounded truth: there
                    // is nothing at that key to route toward.
                    None => out.push(KademliaEvent::QueryFailed {
                        class,
                        reason: QueryFailure::NoRoutingPeers,
                    }),
                },
                QueryClass::Exploration => {
                    let results =
                        NonZeroUsize::new(state.max_results_per_query).unwrap_or(NonZeroUsize::MIN);
                    let id = behaviour.get_n_closest_peers(key.to_vec(), results);
                    state.queries.insert(id, class);
                }
            }
        }
        KademliaCommand::Shutdown => {
            state.stopping = true;
            // §Lifecycle: stop new queries, settle in-flight work.
            // Each outstanding commanded query settles as shutting
            // down; a completion arriving later finds no entry and is
            // ignored. Client mode withdraws the server protocol.
            behaviour.set_mode(Some(kad::Mode::Client));
            for (_, class) in state.queries.drain() {
                out.push(KademliaEvent::QueryFailed {
                    class,
                    reason: QueryFailure::ShuttingDown,
                });
            }
            state.results.clear();
            state.pending_offers.clear();
        }
    }
    out
}

/// §7's admission pipeline, driver side: trust, Identify evidence of
/// the EXACT server protocol, the population bound, then — and only
/// then — `add_address`.
fn try_admit(
    state: &mut KademliaState,
    behaviour: &mut kad::Behaviour<MemoryStore>,
    manager: &ConnectionManager,
    pid: PeerId,
    identity: &TransportIdentity,
    now_ms: u64,
) -> Vec<KademliaEvent> {
    if state.stopping {
        return Vec::new();
    }
    // Routing peers are held to data-plane trust (§13's
    // routing_peer_policy). Discovery of anyone else is legal; RETAINING
    // them is not.
    if manager.classify(identity) != ConnectionClass::DataPlaneTrusted {
        return Vec::new();
    }
    if state.advertises.get(&pid) != Some(&true) {
        // No authenticated Identify evidence of the exact server
        // protocol yet: the offer stays stashed until it arrives.
        return Vec::new();
    }
    // A POPULATION bound, never an address freeze (§11): a peer already
    // routed may always refresh its addresses.
    if !state.routed.contains(&pid) && state.routed.len() >= state.max_routing_peers {
        return Vec::new();
    }
    let mut addresses: BTreeSet<Multiaddr> = state.pending_offers.remove(&pid).unwrap_or_default();
    for known in manager.dial_candidates(identity, now_ms) {
        if let Ok(addr) = strip_peer_suffix_str(&known).parse::<Multiaddr>() {
            addresses.insert(addr);
        }
    }
    if addresses.is_empty() {
        return Vec::new();
    }
    // OPTIMISTIC population accounting: `RoutingUpdated` is the truth
    // and arrives via poll, but two admissions inside one command batch
    // would both read the stale count. Counting here can only
    // over-count — a bucket that refuses the insert leaves a phantom —
    // which under-admits, the fail-closed direction. The event handler
    // reconciles from the behaviour's own report.
    state.routed.insert(pid);
    for addr in addresses {
        let _ = behaviour.add_address(&pid, addr);
    }
    Vec::new()
}

/// Strip trailing `/p2p/<peer>` components from an address STRING.
fn strip_peer_suffix_str(address: &str) -> String {
    match address.parse::<Multiaddr>() {
        Ok(addr) => strip_peer_suffix(&addr),
        Err(_) => address.to_owned(),
    }
}

/// What [`handle_kademlia`] did with an event.
pub(super) enum KadHandled {
    /// A Kademlia behaviour event: translated onto the port, nothing
    /// further to do.
    Consumed,
    /// Anything else — including Identify, which this module PEEKS at
    /// and passes on, because the settlement path and the consumer
    /// translation still need it.
    Passed(Box<Libp2pSwarmEvent<crate::behaviour::SubstrateBehaviourEvent>>),
}

/// The driver's view of one Swarm event.
pub(super) fn handle_kademlia(
    event: Libp2pSwarmEvent<crate::behaviour::SubstrateBehaviourEvent>,
    swarm: &mut crate::gated_swarm::GatedSwarm,
    state: &mut KademliaState,
    manager: &ConnectionManager,
    now_ms: u64,
    out: &mut Vec<KademliaEvent>,
) -> KadHandled {
    use crate::behaviour::SubstrateBehaviourEvent;

    // PEEKED BY REFERENCE, passed on whole: Identify feeds three
    // consumers — this pipeline (F3), the address book, and the
    // consumer's `Identified` event — and consuming it here would
    // starve the other two.
    if let Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Identify(
        identify::Event::Received { peer_id, info, .. },
    )) = &event
    {
        observe_identify(
            state,
            swarm,
            manager,
            *peer_id,
            &info.protocols,
            &info.listen_addrs,
            now_ms,
            out,
        );
        return KadHandled::Passed(Box::new(event));
    }
    match event {
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Kad(kad_event)) => {
            handle_kad_event(state, manager, kad_event, now_ms, out);
            KadHandled::Consumed
        }
        other => KadHandled::Passed(Box::new(other)),
    }
}

/// Fold one `kad::Event` onto the port.
fn handle_kad_event(
    state: &mut KademliaState,
    manager: &ConnectionManager,
    event: kad::Event,
    now_ms: u64,
    out: &mut Vec<KademliaEvent>,
) {
    match event {
        kad::Event::RoutingUpdated {
            peer,
            is_new_peer,
            old_peer,
            ..
        } => {
            // THE BEHAVIOUR'S OWN REPORT is the truth the optimistic
            // count in `try_admit` reconciles against.
            if is_new_peer {
                state.routed.insert(peer);
                if let Ok(admitted) = to_transport_identity(&peer) {
                    out.push(KademliaEvent::RoutingPeerAdded { peer: admitted });
                }
            }
            if let Some(evicted) = old_peer
                && state.routed.remove(&evicted)
                && let Ok(departed) = to_transport_identity(&evicted)
            {
                out.push(KademliaEvent::RoutingPeerRemoved { peer: departed });
            }
        }
        kad::Event::InboundRequest { request } => {
            // §12: dropped by `StoreInserts::FilterBoth` before this
            // code runs; COUNTED here, because "dropped and counted" is
            // the contract and a drop nobody can observe is not a
            // policy.
            if matches!(
                request,
                kad::InboundRequest::PutRecord { .. } | kad::InboundRequest::AddProvider { .. }
            ) {
                state.record_writes_dropped += 1;
            }
        }
        kad::Event::OutboundQueryProgressed {
            id, result, step, ..
        } => match result {
            kad::QueryResult::GetClosestPeers(outcome) => {
                if !state.queries.contains_key(&id) {
                    // A closest-peers walk this driver never commanded;
                    // nothing upstream is waiting on it.
                    return;
                }
                let (peers, timed_out) = match &outcome {
                    Ok(ok) => (&ok.peers, false),
                    Err(kad::GetClosestPeersError::Timeout { peers, .. }) => (peers, true),
                };
                accumulate(state, manager, id, peers, now_ms);
                if step.last {
                    let Some(class) = state.queries.remove(&id) else {
                        return;
                    };
                    let found = state.results.remove(&id).unwrap_or_default();
                    if found.is_empty() && timed_out {
                        out.push(KademliaEvent::QueryFailed {
                            class,
                            reason: QueryFailure::TimedOut,
                        });
                    } else if let Ok(candidates) =
                        interweave_kademlia_control_api::ObservedCandidates::new(found)
                    {
                        out.push(KademliaEvent::QueryResults { candidates, class });
                    }
                }
            }
            kad::QueryResult::Bootstrap(outcome) if step.last => {
                {
                    // An UNKNOWN id here is the library's implicit
                    // bootstrap — the one F2 measured, started by a
                    // routing insertion nobody scheduled. Reporting it
                    // as a bootstrap completion is what lets the
                    // provider settle the charge it took for it.
                    let class = state.queries.remove(&id).unwrap_or(QueryClass::Bootstrap);
                    match outcome {
                        // Empty is always within the bound; a bootstrap
                        // completion carries no candidates of its own.
                        Ok(_) => {
                            if let Ok(candidates) =
                                interweave_kademlia_control_api::ObservedCandidates::new([])
                            {
                                out.push(KademliaEvent::QueryResults { candidates, class });
                            }
                        }
                        Err(_) => out.push(KademliaEvent::QueryFailed {
                            class,
                            reason: QueryFailure::TimedOut,
                        }),
                    }
                }
            }
            // No command exists that could start any other query kind
            // (§12: the port has no record verbs).
            _ => {}
        },
        _ => {}
    }
}

/// Accumulate one progress step's peers as bounded candidates.
fn accumulate(
    state: &mut KademliaState,
    manager: &ConnectionManager,
    id: kad::QueryId,
    peers: &[kad::PeerInfo],
    now_ms: u64,
) {
    let cap = state.max_results_per_query;
    let found = state.results.entry(id).or_default();
    for info in peers {
        if found.len() >= cap {
            break;
        }
        let Ok(candidate) = to_transport_identity(&info.peer_id) else {
            continue;
        };
        if manager.is_local_peer(&candidate) {
            // §10: discard self.
            continue;
        }
        let addresses: std::collections::BTreeSet<String> = info
            .addrs
            .iter()
            .map(strip_peer_suffix)
            .filter(|a| !a.is_empty())
            .collect();
        found.push(interweave_discovery_api::CandidatePeer {
            peer_id: candidate,
            addresses,
            source: "kademlia".to_owned(),
            observed_at: now_ms,
            expires_at: None,
            protocol_observations: std::collections::BTreeSet::new(),
        });
    }
}

/// One authenticated Identify observation enters the §7 pipeline (F3).
#[allow(clippy::too_many_arguments)]
fn observe_identify(
    state: &mut KademliaState,
    swarm: &mut crate::gated_swarm::GatedSwarm,
    manager: &ConnectionManager,
    pid: PeerId,
    protocols: &[StreamProtocol],
    listen_addrs: &[Multiaddr],
    now_ms: u64,
    out: &mut Vec<KademliaEvent>,
) {
    let advertises = protocols.iter().any(|p| p.as_ref() == state.protocol);
    // REPLACED, not merged: a handler that unioned advertisements could
    // never observe a withdrawal, and SPIKE-003's F5 measured the
    // withdrawal happening on a real mode change.
    if state.advertises.contains_key(&pid) || state.advertises.len() < MAX_OBSERVED_PEERS {
        state.advertises.insert(pid, advertises);
    }
    if !advertises {
        // Fresh evidence supersedes: a routed peer that stopped
        // advertising the exact server protocol leaves the table.
        if state.routed.remove(&pid) {
            if let Some(behaviour) = swarm.kademlia_mut() {
                behaviour.remove_peer(&pid);
            }
            if let Ok(departed) = to_transport_identity(&pid) {
                out.push(KademliaEvent::RoutingPeerRemoved { peer: departed });
            }
        }
        return;
    }
    let Ok(identity) = to_transport_identity(&pid) else {
        return;
    };
    // The peer's own listen addresses are candidate routes — this is
    // F3's substance: an INBOUND connection's Identify is as much a
    // routing observation as one this node dialled for. Peer-asserted,
    // so they go through the same stash the offers use, never straight
    // to the table.
    if state.pending_offers.contains_key(&pid) || state.pending_offers.len() < MAX_PENDING_OFFERS {
        let stash = state.pending_offers.entry(pid).or_default();
        for addr in listen_addrs {
            if stash.len() >= 64 {
                break;
            }
            if let Ok(bare) = strip_peer_suffix(addr).parse::<Multiaddr>() {
                stash.insert(bare);
            }
        }
    }
    if let Some(behaviour) = swarm.kademlia_mut() {
        out.extend(try_admit(state, behaviour, manager, pid, &identity, now_ms));
    }
}

/// Trust left some peers: routing follows it, immediately (§11).
pub(super) fn apply_revocations(
    state: &mut KademliaState,
    swarm: &mut crate::gated_swarm::GatedSwarm,
    manager: &ConnectionManager,
    revoked: &[interweave_transport_runtime::Revoked],
    out: &mut Vec<KademliaEvent>,
) {
    for entry in revoked {
        // A routing peer is held to DATA-PLANE trust (§13); a peer
        // demoted to infrastructure-only keeps its reachability role
        // and loses its routing seat.
        if manager.classify(&entry.peer) == ConnectionClass::DataPlaneTrusted {
            continue;
        }
        let Ok(pid) = entry.peer.as_str().parse::<PeerId>() else {
            continue;
        };
        state.advertises.remove(&pid);
        state.pending_offers.remove(&pid);
        if state.routed.remove(&pid) {
            if let Some(behaviour) = swarm.kademlia_mut() {
                behaviour.remove_peer(&pid);
            }
            out.push(KademliaEvent::RoutingPeerRemoved {
                peer: entry.peer.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn the_namespace_matches_every_frozen_vector() {
        // Derived from the SPECIFICATION and checked against the frozen
        // fixture, so a derivation that merely agrees with itself — or
        // one that hashes to the full digest instead of the 16-byte
        // truncation — cannot pass.
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/kademlia/kad-network-namespace-v1.json"
        ))
        .expect("fixture parses");
        let vectors = fixture["vectors"].as_array().expect("vectors");
        assert!(!vectors.is_empty(), "an empty fixture proves nothing");
        for vector in vectors {
            let id = vector["network_id"].as_str().expect("network_id");
            let hash = vector["network_hash"].as_str().expect("hash");
            let protocol = vector["protocol"].as_str().expect("protocol");
            assert_eq!(network_hash(id), hash, "{id}");
            assert_eq!(kad_protocol(id), protocol, "{id}");
        }
    }

    #[test]
    fn the_network_id_grammar_is_exact() {
        for legal in ["a", "example-private-network", "net-2.staging_7", "0x"] {
            assert!(network_id_is_legal(legal), "{legal}");
        }
        for illegal in [
            "",
            "A",
            "-leading",
            ".leading",
            "_leading",
            "has space",
            "ünïcode",
        ] {
            assert!(!network_id_is_legal(illegal), "{illegal:?}");
        }
        assert!(network_id_is_legal(&"n".repeat(64)));
        assert!(!network_id_is_legal(&"n".repeat(65)));
    }

    #[test]
    fn a_lookup_key_rebuilds_the_exact_peer() {
        // The other half of the encoding the provider writes: the key
        // is the Ed25519 public key, and the envelope is constant, so
        // reconstruction must be exact and reversible.
        let key: [u8; 32] = core::array::from_fn(|i| u8::try_from(i).expect("fits") ^ 0x5a);
        let peer = peer_from_lookup_key(key).expect("a legal identity multihash");
        let bytes = peer.to_bytes();
        assert_eq!(bytes.len(), 38);
        // LITERALS, not the module's own constant: a test that compares
        // against ED25519_ENVELOPE agrees with any mutation of it for
        // free — the self-referential trap CLAUDE.md's testing rule
        // names. These six bytes are the identity-multihash envelope as
        // the spec fixes it, written out independently.
        assert_eq!(bytes[..6], [0x00, 0x24, 0x08, 0x01, 0x12, 0x20]);
        assert_eq!(bytes[6..], key);
        // And the INDEPENDENT parser agrees: TransportIdentity decodes
        // the base58 and checks the multihash itself, so a bent
        // envelope fails here even if libp2p happens to tolerate it.
        let identity = TransportIdentity::parse(peer.to_base58())
            .expect("the rebuilt identity satisfies the neutral grammar");
        assert!(identity.as_str().starts_with("12D3KooW"));
    }

    #[test]
    fn settings_outside_the_bounds_are_refused() {
        let good = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 256,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
        };
        good.validate().expect("the canonical defaults validate");
        let bad_id = KademliaSettings {
            network_id: "Not-Legal".to_owned(),
            ..good.clone()
        };
        assert!(bad_id.validate().is_err());
        let zero_peers = KademliaSettings {
            max_routing_peers: 0,
            ..good.clone()
        };
        assert!(zero_peers.validate().is_err());
        let over_cap = KademliaSettings {
            max_results_per_query: NonZeroUsize::new(MAX_RESULTS_PER_QUERY + 1).expect("nonzero"),
            ..good
        };
        assert!(over_cap.validate().is_err());
    }

    #[test]
    fn inbound_record_writes_are_counted_never_stored() {
        // §12: FilterBoth already refused the store; the count is the
        // contract's other half — a drop nobody can observe is not a
        // policy.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 256,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let mut out = Vec::new();
        let put = kad::Event::InboundRequest {
            request: kad::InboundRequest::PutRecord {
                source: PeerId::random(),
                connection: libp2p::swarm::ConnectionId::new_unchecked(1),
                record: None,
            },
        };
        handle_kad_event(&mut state, &manager, put, 0, &mut out);
        let add = kad::Event::InboundRequest {
            request: kad::InboundRequest::AddProvider { record: None },
        };
        handle_kad_event(&mut state, &manager, add, 0, &mut out);
        let read = kad::Event::InboundRequest {
            request: kad::InboundRequest::FindNode {
                num_closer_peers: 1,
            },
        };
        handle_kad_event(&mut state, &manager, read, 0, &mut out);
        assert_eq!(
            state.record_writes_dropped(),
            2,
            "both write kinds are counted; a routing read is not a write"
        );
        assert!(out.is_empty(), "counting is not a port event");
    }
}
