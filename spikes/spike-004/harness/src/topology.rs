// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Driving several spike nodes at once, and remembering what they saw.
//!
//! The pump follows SPIKE-003's: it runs until its budget is spent
//! rather than until the swarms go quiet, because a spike that stopped
//! at the first idle moment would miss exactly the delayed work —
//! probe intervals, reservation renewals, hole-punch retries — it
//! exists to count.

use std::collections::{BTreeMap, BTreeSet};
use std::task::Poll;
use std::time::{Duration, Instant};

use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId};

use crate::node::{Node, SpikeBehaviourEvent};

/// What a node observed while being pumped.
#[derive(Debug, Default, Clone)]
pub struct Observed {
    /// Every behaviour event, as a stable label plus a detail string.
    pub events: Vec<(&'static str, String)>,
    /// Addresses the swarm decided are externally reachable.
    pub external_addresses: BTreeSet<Multiaddr>,
    /// Peers currently connected.
    pub connected: BTreeSet<PeerId>,
    /// Outgoing dial failures, by the error's rendering.
    pub dial_failures: BTreeMap<String, u64>,
    /// Protocols each peer told us it supports, via Identify.
    pub identify_protocols: BTreeMap<PeerId, BTreeSet<String>>,
}

impl Observed {
    /// How many events carry this label.
    #[must_use]
    pub fn count(&self, label: &str) -> usize {
        self.events.iter().filter(|(l, _)| *l == label).count()
    }

    /// The detail strings for one label.
    #[must_use]
    pub fn details(&self, label: &str) -> Vec<String> {
        self.events
            .iter()
            .filter(|(l, _)| *l == label)
            .map(|(_, d)| d.clone())
            .collect()
    }
}

fn record(node: &mut Node, event: SwarmEvent<SpikeBehaviourEvent>) {
    let seen = &mut node.observed;
    match event {
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            seen.connected.insert(peer_id);
            seen.events
                .push(("connection-established", peer_id.to_string()));
        }
        SwarmEvent::ConnectionClosed { peer_id, .. } => {
            seen.connected.remove(&peer_id);
            seen.events.push(("connection-closed", peer_id.to_string()));
        }
        SwarmEvent::OutgoingConnectionError { error, peer_id, .. } => {
            *seen
                .dial_failures
                .entry(format!("{error}"))
                .or_insert(0) += 1;
            seen.events
                .push(("dial-failed", format!("{peer_id:?}: {error}")));
        }
        SwarmEvent::ExternalAddrConfirmed { address } => {
            seen.external_addresses.insert(address.clone());
            seen.events
                .push(("external-addr-confirmed", address.to_string()));
        }
        SwarmEvent::ExternalAddrExpired { address } => {
            seen.external_addresses.remove(&address);
            seen.events
                .push(("external-addr-expired", address.to_string()));
        }
        SwarmEvent::NewExternalAddrCandidate { address } => {
            seen.events
                .push(("external-addr-candidate", address.to_string()));
        }
        SwarmEvent::Behaviour(event) => record_behaviour(seen, event),
        _ => {}
    }
}

fn record_behaviour(seen: &mut Observed, event: SpikeBehaviourEvent) {
    match event {
        SpikeBehaviourEvent::Gate(never) => libp2p::core::util::unreachable(never),
        SpikeBehaviourEvent::Identify(identify::Event::Received { peer_id, info, .. }) => {
            seen.identify_protocols.insert(
                peer_id,
                info.protocols.iter().map(ToString::to_string).collect(),
            );
            seen.events.push(("identify-received", peer_id.to_string()));
        }
        SpikeBehaviourEvent::Identify(_) => {}
        SpikeBehaviourEvent::AutonatClient(e) => {
            seen.events.push((
                "autonat-client",
                format!(
                    "server={} tested={} bytes={} result={:?}",
                    e.server, e.tested_addr, e.bytes_sent, e.result
                ),
            ));
        }
        SpikeBehaviourEvent::AutonatServer(e) => {
            seen.events.push(("autonat-server", format!("{e:?}")));
        }
        SpikeBehaviourEvent::RelayClient(e) => {
            let label = match &e {
                relay::client::Event::ReservationReqAccepted { .. } => "relay-reservation-accepted",
                relay::client::Event::OutboundCircuitEstablished { .. } => "relay-circuit-outbound",
                relay::client::Event::InboundCircuitEstablished { .. } => "relay-circuit-inbound",
            };
            seen.events.push((label, format!("{e:?}")));
        }
        SpikeBehaviourEvent::RelayServer(e) => {
            seen.events.push(("relay-server", format!("{e:?}")));
        }
        SpikeBehaviourEvent::Dcutr(e) => {
            seen.events.push(("dcutr", format!("{e:?}")));
        }
    }
}

use libp2p::{identify, relay};

/// Drive every node until `budget` elapses.
pub async fn pump(nodes: &mut [&mut Node], budget: Duration) {
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
                if progressed { Poll::Ready(()) } else { Poll::Pending }
            }),
        )
        .await;
    }
}

/// Drive until `settled` holds or `budget` is spent. Returns whether it
/// settled, so a caller cannot mistake a timeout for a result.
pub async fn pump_until(
    nodes: &mut [&mut Node],
    budget: Duration,
    mut settled: impl FnMut(&[&mut Node]) -> bool,
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
