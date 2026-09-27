// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The runtime's status surface (plan §15): the computed
//! `ConnectivitySummary` and, beside it, the dial gate's introspection.
//!
//! Answered by the Swarm task from the state it already owns -- the
//! AutoNAT verdict, the relay client's standing, the per-peer paths, the
//! hole-punch lifecycle and the connection manager -- so a status read
//! is a photograph of one instant, not a join of counters read apart.
//!
//! TWO AUDIENCES, kept apart by type. [`ConnectivitySummary`] is the
//! neutral contract's (`CONNECTIVITY.md` §3), safe for a data-plane
//! client; [`DialGateStatus`] and the diagnostics beside it are
//! observability, outside the neutral contract, for the operator.

use interweave_transport_api::{
    ConnectivitySummary, DirectInboundState, PathReadiness, PeerPath, PreferredPathPolicy,
};

/// One status read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStatus {
    /// The neutral summary (`CONNECTIVITY.md` §3, §4's `connectivity()`).
    pub connectivity: ConnectivitySummary,
    /// The dial gate, as the manager holds it.
    pub dial_gate: DialGateStatus,
    /// `AUTONAT.md` §9: candidate addresses the reachability manager
    /// refused, cumulative. `None` with the AutoNAT client off.
    pub autonat_rejected_candidates: Option<usize>,
    /// `kademlia-integration.md` §12: inbound record writes dropped,
    /// counted and never stored. `None` with Kademlia off.
    pub kademlia_record_writes_dropped: Option<u64>,
    /// Inbound direct requests in flight in the dedup reservation map,
    /// owners and waiters together -- the count the map's global budget
    /// is measured against.
    pub direct_reservations_outstanding: usize,
}

/// The dial gate's introspection (plan §15's Implement tree).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialGateStatus {
    /// The policy revision the manager has published.
    pub revision: u64,
    /// Established connections, as the manager counts them.
    pub connections: usize,
    /// Established connections as the published snapshot's holders see
    /// them -- the behaviours' gates. Apart from `connections` because
    /// the two are distinct counters; a difference is a publish not yet
    /// taken, not an error.
    pub published_connections: usize,
    /// Dials admitted and not yet settled, across every snapshot holder.
    pub published_pending_dials: usize,
    /// Peers awaiting a retry.
    pub scheduled_retries: usize,
    /// Whether the asked-about peer's retry is due, without claiming it.
    /// `None` when no peer was asked about.
    pub peer_retry_due: Option<bool>,
    /// The bounded address table's size.
    pub address_entries: usize,
    /// The bounded peer-backoff table's size.
    pub peer_entries: usize,
}

/// `CONNECTIVITY.md` §3's summary from the state the task holds.
///
/// `reservations` is the relay client's `(active, target)`, `None` with
/// the client off; `direct_inbound` is `Unknown` with the AutoNAT client
/// off, since no evidence has been gathered.
pub(super) fn summarize(
    direct_inbound: DirectInboundState,
    reservations: Option<(usize, usize)>,
    paths: impl IntoIterator<Item = PeerPath>,
    hole_punch_inflight: usize,
    updated_at: u64,
) -> ConnectivitySummary {
    let (active, target) = reservations.unwrap_or((0, 0));
    ConnectivitySummary {
        direct_inbound,
        relay_inbound: relay_inbound(active, target),
        active_relay_reservations: saturate(active),
        target_relay_reservations: saturate(target),
        active_relayed_peer_paths: saturate(
            paths
                .into_iter()
                .filter(|p| *p == PeerPath::Relayed)
                .count(),
        ),
        hole_punch_inflight: saturate(hole_punch_inflight),
        preferred_path_policy: PreferredPathPolicy::DirectFirst,
        updated_at,
    }
}

/// `CONNECTIVITY.md` §3: `ready` when the target is met, `partial` when at
/// least one reservation is active and the target is not, `unavailable`
/// when none is. NO ACTIVE RESERVATION IS `unavailable` EVEN AT A ZERO
/// TARGET: "target met" would read a node with no relayed inbound path as
/// ready, and the contract's `unavailable` clause is about the path.
fn relay_inbound(active: usize, target: usize) -> PathReadiness {
    if active == 0 {
        PathReadiness::Unavailable
    } else if active >= target {
        PathReadiness::Ready
    } else {
        PathReadiness::Partial
    }
}

/// The contract's counters are `u16`; a count past it reads as the
/// ceiling rather than wrapping to a small number.
fn saturate(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::{PathReadiness, PeerPath, relay_inbound, summarize};
    use interweave_transport_api::DirectInboundState;

    #[test]
    fn readiness_follows_the_contracts_three_clauses() {
        assert_eq!(relay_inbound(0, 2), PathReadiness::Unavailable);
        assert_eq!(relay_inbound(1, 2), PathReadiness::Partial);
        assert_eq!(relay_inbound(2, 2), PathReadiness::Ready);
        assert_eq!(relay_inbound(3, 2), PathReadiness::Ready);
        // No path, whatever the target says.
        assert_eq!(relay_inbound(0, 0), PathReadiness::Unavailable);
    }

    #[test]
    fn only_relayed_paths_are_counted_and_counters_saturate() {
        let paths = [PeerPath::Relayed, PeerPath::Direct, PeerPath::Relayed];
        let s = summarize(DirectInboundState::Unknown, Some((1, 2)), paths, 70_000, 5);
        assert_eq!(s.active_relayed_peer_paths, 2);
        assert_eq!(s.active_relay_reservations, 1);
        assert_eq!(s.target_relay_reservations, 2);
        assert_eq!(s.relay_inbound, PathReadiness::Partial);
        assert_eq!(s.hole_punch_inflight, u16::MAX);
        assert_eq!(s.updated_at, 5);
    }

    #[test]
    fn with_every_behaviour_off_the_summary_says_so() {
        let s = summarize(DirectInboundState::Unknown, None, [], 0, 0);
        assert_eq!(s.relay_inbound, PathReadiness::Unavailable);
        assert_eq!(
            (s.active_relay_reservations, s.target_relay_reservations),
            (0, 0)
        );
        assert_eq!(s.direct_inbound, DirectInboundState::Unknown);
    }
}
