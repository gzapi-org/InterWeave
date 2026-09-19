// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `transport/libp2p/CONNECTIVITY.md` §12 and `contracts/CONNECTIVITY.md`
//! §6 at the `DialPeer` command: direct first, the relay after a
//! head-start (step 9).
//!
//! The address book holds direct routes and, since step 7, the circuit
//! routes a peer was reached over, sorted known-good first -- which
//! put a circuit ahead of a direct address whenever the circuit had
//! worked more recently. §12 orders them by PATH instead: the direct
//! candidates are dialled at once, and a relayed one only after the
//! head-start has passed with no direct connection completed. A
//! losing attempt is not cancelled -- the pinned Swarm has no way to
//! abandon a dial in flight -- so a direct connection that lands after
//! the circuit is a second path, announced as step 7 announces one,
//! and a circuit that lands after the direct is a redundant relayed
//! connection, which step 9's retirement closes once the direct is
//! stable. The head-start is the relay client's
//! `direct_head_start_ms`, the profile's `relay.client.direct_head_start`,
//! 750 ms by default, SPIKE-004-tunable and not a wire invariant.
//!
//! Pinned by `direct_candidates_are_dialled_first_and_relayed_ones_
//! after_the_head_start` and, over real sockets, by
//! `tests/connectivity/tests/path_race.rs`.

use std::collections::HashMap;

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::DialOrigin;

use super::dialing::book_origin;

/// A peer's candidates split by path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Plan {
    /// Direct routes, in the book's order.
    pub(super) direct: Vec<String>,
    /// Circuit routes, in the book's order.
    pub(super) relayed: Vec<String>,
}

/// Split the book's candidates by path, keeping the book's order
/// within each.
#[must_use]
pub(super) fn plan(candidates: Vec<String>) -> Plan {
    let mut out = Plan::default();
    for address in candidates {
        if book_origin(&address, DialOrigin::Manual) == DialOrigin::RelayCircuit {
            out.relayed.push(address);
        } else {
            out.direct.push(address);
        }
    }
    out
}

/// A relayed dial deferred behind a direct one's head-start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Race {
    /// When the relayed candidates may be dialled.
    pub(super) due_ms: u64,
    /// The circuit routes to try then, in order.
    pub(super) relayed: Vec<String>,
}

/// The races in flight, one per peer: a second `DialPeer` for a peer
/// replaces its race, so the map is bounded by the peers a caller
/// asked for and never holds two entries for one.
#[derive(Debug, Default)]
pub(super) struct Races {
    inner: HashMap<TransportIdentity, Race>,
}

impl Races {
    /// Defer `relayed` for `peer` until `due_ms`.
    pub(super) fn defer(&mut self, peer: TransportIdentity, due_ms: u64, relayed: Vec<String>) {
        self.inner.insert(peer, Race { due_ms, relayed });
    }

    /// The earliest deadline, if any race is waiting.
    #[must_use]
    pub(super) fn next_due_ms(&self) -> Option<u64> {
        self.inner.values().map(|r| r.due_ms).min()
    }

    /// The races whose head-start has passed at `now_ms`, removed.
    pub(super) fn take_due(&mut self, now_ms: u64) -> Vec<(TransportIdentity, Vec<String>)> {
        let due: Vec<TransportIdentity> = self
            .inner
            .iter()
            .filter(|(_, r)| r.due_ms <= now_ms)
            .map(|(p, _)| p.clone())
            .collect();
        due.into_iter()
            .filter_map(|peer| self.inner.remove(&peer).map(|r| (peer, r.relayed)))
            .collect()
    }

    /// Forget `peer`'s race: a direct connection landed, or the peer
    /// went away.
    pub(super) fn forget(&mut self, peer: &TransportIdentity) -> bool {
        self.inner.remove(peer).is_some()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.inner.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    const RELAY: &str = "12D3KooWCLxLXFHqvfsHVLDcNsSpZBQq1M1KMRgQRLLLnHTv7oQD";
    const FAR: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTA";

    #[test]
    fn direct_candidates_are_dialled_first_and_relayed_ones_after_the_head_start() {
        // The book's order -- a circuit known-good first -- is split by
        // path, each half keeping its order.
        let circuit = format!("/ip4/10.0.0.1/tcp/4001/p2p/{RELAY}/p2p-circuit");
        let p = plan(vec![
            circuit.clone(),
            "/ip4/10.0.0.2/tcp/4001".to_owned(),
            "/ip4/10.0.0.3/tcp/4001".to_owned(),
            "not a multiaddr".to_owned(),
        ]);
        assert_eq!(
            p.direct,
            vec![
                "/ip4/10.0.0.2/tcp/4001",
                "/ip4/10.0.0.3/tcp/4001",
                "not a multiaddr"
            ],
            "direct routes, and an unparseable one dialled as before"
        );
        assert_eq!(p.relayed, vec![circuit.clone()]);
        // A race is due when the head-start has passed and not before;
        // one per peer, the later ask replacing the earlier; forgotten
        // when a direct connection lands.
        let peer = TransportIdentity::parse(FAR).expect("valid");
        let mut races = Races::default();
        races.defer(peer.clone(), 1_750, vec![circuit.clone()]);
        assert_eq!(races.next_due_ms(), Some(1_750));
        assert!(races.take_due(1_749).is_empty(), "not before");
        races.defer(peer.clone(), 2_000, vec![circuit.clone()]);
        assert_eq!(races.len(), 1, "one race per peer");
        assert!(
            races.take_due(1_999).is_empty(),
            "the later ask replaced the earlier"
        );
        assert_eq!(
            races.take_due(2_000),
            vec![(peer.clone(), vec![circuit.clone()])]
        );
        assert_eq!(races.next_due_ms(), None);
        races.defer(peer.clone(), 3_000, vec![circuit]);
        assert!(races.forget(&peer));
        assert!(!races.forget(&peer), "gone");
        assert!(races.take_due(u64::MAX).is_empty());
    }
}
