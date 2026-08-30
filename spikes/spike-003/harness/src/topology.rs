// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Driving several nodes at once, and remembering what they saw.
//!
//! Every experiment needs the same three things: run N swarms until
//! something settles, record the events that constitute the evidence,
//! and never confuse one node's observation with another's. The event
//! handler here is the single place any of that is written down, so an
//! experiment cannot quietly count something a different way.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::task::Poll;
use std::time::{Duration, Instant};

use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{PeerId, identify, kad};

use crate::node::{Node, SpikeBehaviourEvent};

/// What one node observed, accumulated across every pump.
#[derive(Debug, Default)]
pub struct Observations {
    /// Protocols each peer advertised through Identify, most recent
    /// exchange wins — the design's "a fresh Identify response
    /// supersedes cached evidence", made observable.
    pub identify_protocols: BTreeMap<PeerId, BTreeSet<String>>,
    /// How many Identify exchanges were completed with each peer.
    pub identify_rounds: BTreeMap<PeerId, u64>,
    /// The listen addresses each peer reported through Identify. An
    /// INBOUND connection is a candidate too, and under
    /// `BucketInserts::Manual` this is the only way a seed node ever
    /// learns the peers that dialled it.
    pub identify_listen_addrs: BTreeMap<PeerId, BTreeSet<libp2p::Multiaddr>>,
    /// Peers this node put in its routing table, and when it was told.
    pub routing_updates: Vec<PeerId>,
    /// Peers a query returned, by query id.
    pub query_results: HashMap<kad::QueryId, BTreeSet<PeerId>>,
    /// Query ids that finished, in completion order.
    pub finished_queries: Vec<kad::QueryId>,
    /// Requests a query sent, and how many succeeded — the width of the
    /// walk, which is what disjoint paths is supposed to change.
    pub query_requests: HashMap<kad::QueryId, (u32, u32)>,
    /// Bootstrap queries that COMPLETED, whoever started them.
    pub bootstrap_completions: u64,
    /// Inbound record/provider write attempts, by kind.
    pub record_writes: BTreeMap<String, u64>,
    /// Query ids this node never started — the library's own work.
    pub unattributed_queries: HashSet<kad::QueryId>,
    /// Outgoing connection errors, by the message libp2p reported.
    pub dial_errors: Vec<String>,
    /// Peers this node successfully connected to.
    pub connected: BTreeSet<PeerId>,
    /// Peers this node connected to as the DIALER. A connection it did
    /// not dial was dialled by the other end, which is a different fact
    /// about the gate and must not be confused with one it made.
    pub dialed_out: BTreeSet<PeerId>,
    /// Addresses a query returned per peer — what the provider would
    /// feed back through `AddRoutingAddress`.
    pub learned_addresses: BTreeMap<PeerId, BTreeSet<libp2p::Multiaddr>>,
}

/// Drive every node until `budget` elapses.
///
/// Returns when the budget is spent, not when the swarms go quiet: a
/// spike that stopped at the first idle moment would miss exactly the
/// delayed work — implicit bootstrap, query timeouts — it is here to
/// count.
pub async fn pump(nodes: &mut [Node], budget: Duration) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let slice = deadline.saturating_duration_since(Instant::now());
        let step = slice.min(Duration::from_millis(50));
        let _ = tokio::time::timeout(
            step,
            futures::future::poll_fn(|cx| {
                let mut progressed = false;
                for node in nodes.iter_mut() {
                    loop {
                        let Poll::Ready(Some(event)) = node.swarm.poll_next_unpin(cx) else {
                            break;
                        };
                        record(node, event);
                        progressed = true;
                    }
                }
                if progressed {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }),
        )
        .await;
    }
}

/// Drive until `settled` says the topology has reached the state the
/// experiment is waiting for, or `budget` is spent. Returns whether it
/// settled, so a caller cannot mistake a timeout for a result.
pub async fn pump_until(
    nodes: &mut [Node],
    budget: Duration,
    mut settled: impl FnMut(&mut [Node]) -> bool,
) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if settled(nodes) {
            return true;
        }
        pump(nodes, Duration::from_millis(100)).await;
    }
    settled(nodes)
}

/// The admission pipeline of `kademlia-integration.md` §7, applied to
/// every candidate a node currently holds.
///
/// A candidate is an address plus an authenticated Identify observation
/// that the peer advertises the exact current server protocol; trust is
/// checked before insertion and nothing else may insert. Query results
/// and INBOUND connections are both candidates — the second matters
/// more than it looks, because under `BucketInserts::Manual` it is the
/// only way a seed node learns the peers that dialled it.
pub fn admit_candidates(node: &mut Node, protocol: &str) -> usize {
    admit_candidates_bounded(node, protocol, usize::MAX)
}

/// The same pipeline under a project `max_routing_peers` bound.
///
/// The bound is PROJECT logic — `kademlia-integration.md` §11 puts it
/// "before manual insertion", and rust-libp2p knows nothing about it.
/// `kbucket_size` is the library's own per-bucket cap and does not stand
/// in for it: a table can hold `kbucket_size` entries in each of many
/// buckets and still exceed a total the project meant to enforce.
pub fn admit_candidates_bounded(node: &mut Node, protocol: &str, max_routing: usize) -> usize {
    let trusted = node.trusts();
    let me = node.peer_id;
    let mut candidates: Vec<(PeerId, libp2p::Multiaddr)> = Vec::new();

    // THE EXACT SERVER PROTOCOL, for EVERY candidate — §7's pipeline
    // ends in "exact current Kademlia server protocol advertised" and
    // does not exempt the source. An earlier version applied it only to
    // peers learned through Identify, so anything a QUERY returned was
    // inserted on the strength of being trusted and reachable: a peer
    // with absent, stale, negative or client-mode capability evidence
    // entered the routing table, and the convergence experiments
    // "converged" by admitting peers the documented pipeline refuses.
    //
    // A query result is advisory. It says a peer exists at an address,
    // not that the peer serves this DHT — that is what the authenticated
    // Identify observation says, and only for the exact protocol.
    let serves = |peer: &PeerId| {
        node.observed
            .identify_protocols
            .get(peer)
            .is_some_and(|p| p.contains(protocol))
    };
    for (peer, addrs) in &node.observed.learned_addresses {
        if !serves(peer) {
            continue;
        }
        if let Some(a) = addrs.iter().next() {
            candidates.push((*peer, a.clone()));
        }
    }
    for (peer, addrs) in &node.observed.identify_listen_addrs {
        if !serves(peer) {
            continue;
        }
        if let Some(a) = addrs.iter().next() {
            candidates.push((*peer, a.clone()));
        }
    }

    let mut admitted = 0;
    for (peer, addr) in candidates {
        if peer == me || !trusted.contains(&peer) {
            continue;
        }
        // THE PROJECT BOUND, checked BEFORE insertion. Reading the live
        // table each time rather than counting admissions: a candidate
        // already present is not a new entry, and counting insertions
        // would refuse re-announcements of peers the table already has.
        if node.routing_peers() >= max_routing {
            break;
        }
        if let Some(k) = node.swarm.behaviour_mut().kad.as_mut() {
            k.add_address(&peer, addr);
            admitted += 1;
        }
    }
    admitted
}

fn record(node: &mut Node, event: SwarmEvent<SpikeBehaviourEvent>) {
    match event {
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
        } => {
            node.observed.connected.insert(peer_id);
            if endpoint.is_dialer() {
                node.observed.dialed_out.insert(peer_id);
            }
        }
        SwarmEvent::OutgoingConnectionError { error, .. } => {
            node.observed.dial_errors.push(format!("{error}"));
        }
        SwarmEvent::Behaviour(SpikeBehaviourEvent::Identify(identify::Event::Received {
            peer_id,
            info,
            ..
        })) => {
            // REPLACED, not merged: the design says a fresh Identify
            // response supersedes cached evidence, and a handler that
            // unioned them could never observe a protocol being
            // withdrawn.
            node.observed.identify_protocols.insert(
                peer_id,
                info.protocols.iter().map(ToString::to_string).collect(),
            );
            node.observed
                .identify_listen_addrs
                .insert(peer_id, info.listen_addrs.iter().cloned().collect());
            *node.observed.identify_rounds.entry(peer_id).or_default() += 1;
        }
        SwarmEvent::Behaviour(SpikeBehaviourEvent::Kad(event)) => record_kad(node, event),
        _ => {}
    }
}

fn record_kad(node: &mut Node, event: kad::Event) {
    match event {
        kad::Event::RoutingUpdated { peer, .. } => node.observed.routing_updates.push(peer),
        kad::Event::InboundRequest { request } => {
            let kind = match request {
                kad::InboundRequest::PutRecord { .. } => "PUT_VALUE",
                kad::InboundRequest::AddProvider { .. } => "ADD_PROVIDER",
                kad::InboundRequest::FindNode { .. } => "FIND_NODE",
                kad::InboundRequest::GetProvider { .. } => "GET_PROVIDERS",
                kad::InboundRequest::GetRecord { .. } => "GET_VALUE",
            };
            *node
                .observed
                .record_writes
                .entry(kind.to_owned())
                .or_default() += 1;
        }
        kad::Event::OutboundQueryProgressed {
            id,
            result,
            step,
            stats,
        } => {
            node.observed
                .query_requests
                .insert(id, (stats.num_requests(), stats.num_successes()));
            // A QUERY THIS NODE DID NOT START is the library's own work,
            // which is precisely what the brief says must be counted
            // rather than assumed absent.
            if !node.own_queries.contains_key(&id) {
                node.observed.unattributed_queries.insert(id);
            }
            let mut learned: Vec<(PeerId, BTreeSet<libp2p::Multiaddr>)> = Vec::new();
            let found = node.observed.query_results.entry(id).or_default();
            match &result {
                kad::QueryResult::GetClosestPeers(Ok(ok)) => {
                    found.extend(ok.peers.iter().map(|p| p.peer_id));
                    learned.extend(
                        ok.peers
                            .iter()
                            .map(|p| (p.peer_id, p.addrs.iter().cloned().collect())),
                    );
                }
                kad::QueryResult::GetClosestPeers(Err(
                    kad::GetClosestPeersError::Timeout { peers, .. },
                )) => {
                    found.extend(peers.iter().map(|p| p.peer_id));
                    learned.extend(
                        peers
                            .iter()
                            .map(|p| (p.peer_id, p.addrs.iter().cloned().collect())),
                    );
                }
                kad::QueryResult::Bootstrap(_) => {
                    if step.last {
                        node.observed.bootstrap_completions += 1;
                    }
                }
                _ => {}
            }
            for (peer, addrs) in learned {
                node.observed
                    .learned_addresses
                    .entry(peer)
                    .or_default()
                    .extend(addrs);
            }
            if step.last {
                node.observed.finished_queries.push(id);
            }
        }
        _ => {}
    }
}
