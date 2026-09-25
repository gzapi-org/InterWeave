// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! ADR-0052 rule 8's STORE half, made readable: one count of what each
//! store's learn site admitted and refused, by class, held outside the
//! Swarm task.
//!
//! # Why one handle for every store
//!
//! Each store's learn site refuses a peer-supplied address before it is
//! stored (rule 8), and rule 5 keeps the refused address out of every
//! log -- so the count is the only trace a refusal leaves. The #111
//! re-review found the first three stores' counts were write-only: each
//! lived inside the Swarm task, read only by its own unit tests, so in a
//! running node a boundary refusing everything and a peer advertising
//! nothing looked the same. One handle for all of them fixes that as a
//! class rather than one store per round, and gives every store the same
//! vocabulary: the store's name, then [`CandidateRefusal::label`].
//!
//! # Why the verdict and the count are one call
//!
//! [`StoreRefusals::judge`] applies the operator set's boundary AND
//! records the outcome. A hook that judged without counting, or counted
//! a different verdict from the one it acted on, is the drift a separate
//! call would allow.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use interweave_transport_runtime::reachability::CandidateRefusal;
use libp2p::Multiaddr;

use crate::operator_set::OperatorSet;

/// The stores that apply the boundary at their learn site, by the name
/// their counts are filed under.
pub mod store {
    /// The runtime's address book, from Identify's `listen_addrs`.
    pub const ADDRESS_BOOK: &str = "address_book";
    /// Kademlia's offer stash, ahead of `add_address`.
    pub const ROUTING_STASH: &str = "routing_stash";
    /// mDNS candidates, before they become observations.
    pub const MDNS: &str = "mdns";
    /// The AutoNAT client's learned-server list.
    pub const AUTONAT_SERVERS: &str = "autonat_servers";
    /// The relay client's learned-relay list.
    pub const RELAY_RESERVATIONS: &str = "relay_reservations";
    /// Kademlia query results, before they leave the driver.
    pub const QUERY_CANDIDATES: &str = "query_candidates";
}

/// One store's counts.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StoreCounts {
    /// Addresses that passed the boundary and were offered to the store.
    ///
    /// Counted beside the refusals: without it a store refusing
    /// everything and a quiet peer read the same.
    pub admitted: usize,
    /// Those refused, by `CandidateRefusal::label`.
    pub refused: BTreeMap<&'static str, usize>,
}

impl StoreCounts {
    /// Every refusal, whatever its class.
    #[must_use]
    pub fn refused_total(&self) -> usize {
        self.refused.values().sum()
    }
}

/// The shared, readable count for every store (see the module note).
///
/// BOUNDED by construction: its keys are the store names above and the
/// four refusal classes, all `'static`, so nothing a remote party sends
/// can add a key.
#[derive(Debug, Clone, Default)]
pub struct StoreRefusals {
    inner: Arc<Mutex<BTreeMap<&'static str, StoreCounts>>>,
}

impl StoreRefusals {
    /// An empty record.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Judge `address` for `store`: admitted if it came in by the
    /// operator's door or passes the floor with rule 3, and the outcome
    /// recorded either way. Returns whether it may be stored.
    pub fn judge<'a>(
        &self,
        store: &'static str,
        operator: &OperatorSet,
        address: &Multiaddr,
        own_listeners: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        let verdict = operator.admits(address, own_listeners);
        self.record(store, verdict)
    }

    /// Record an outcome a store already judged -- for the one store
    /// (mDNS) that judges with its own sibling predicate. Returns whether
    /// it was admitted.
    pub fn record(&self, store: &'static str, verdict: Result<(), CandidateRefusal>) -> bool {
        let mut counts = self.lock();
        let entry = counts.entry(store).or_default();
        match verdict {
            Ok(()) => {
                entry.admitted += 1;
                true
            }
            Err(class) => {
                *entry.refused.entry(class.label()).or_default() += 1;
                false
            }
        }
    }

    /// One store's counts as they stand (zero if it has recorded
    /// nothing).
    #[must_use]
    pub fn get(&self, store: &'static str) -> StoreCounts {
        self.lock().get(store).cloned().unwrap_or_default()
    }

    /// Every store's counts as they stand.
    #[must_use]
    pub fn snapshot(&self) -> BTreeMap<&'static str, StoreCounts> {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<&'static str, StoreCounts>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn addr(s: &str) -> Multiaddr {
        s.parse().expect("valid")
    }

    /// The verdict and the count are the same fact: what `judge`
    /// returns is what it recorded, for an admission and a refusal.
    #[test]
    fn judge_records_exactly_the_verdict_it_returns() {
        let stores = StoreRefusals::new();
        let operator = OperatorSet::new();
        let none: [&str; 0] = [];

        assert!(stores.judge(
            store::AUTONAT_SERVERS,
            &operator,
            &addr("/ip4/8.8.8.8/tcp/1"),
            none
        ));
        assert!(!stores.judge(
            store::AUTONAT_SERVERS,
            &operator,
            &addr("/ip4/127.0.0.1/tcp/1"),
            none
        ));
        assert!(!stores.judge(
            store::AUTONAT_SERVERS,
            &operator,
            &addr("/dns4/x.example/tcp/1"),
            none
        ));

        let counts = stores.get(store::AUTONAT_SERVERS);
        assert_eq!(counts.admitted, 1);
        assert_eq!(counts.refused.get("special_use").copied(), Some(1));
        assert_eq!(counts.refused.get("not_literal").copied(), Some(1));
    }

    /// Stores are counted apart, and a clone reads the one record -- the
    /// point of a handle held outside the Swarm task.
    #[test]
    fn each_store_is_counted_apart_and_a_clone_sees_the_counts() {
        let stores = StoreRefusals::new();
        let reader = stores.clone();
        let operator = OperatorSet::new();
        let none: [&str; 0] = [];

        let _ = stores.judge(
            store::ADDRESS_BOOK,
            &operator,
            &addr("/ip4/127.0.0.1/tcp/1"),
            none,
        );
        let _ = stores.judge(
            store::RELAY_RESERVATIONS,
            &operator,
            &addr("/ip4/8.8.8.8/tcp/1"),
            none,
        );

        assert_eq!(reader.get(store::ADDRESS_BOOK).refused_total(), 1);
        assert_eq!(reader.get(store::ADDRESS_BOOK).admitted, 0);
        assert_eq!(reader.get(store::RELAY_RESERVATIONS).admitted, 1);
        assert_eq!(reader.get(store::MDNS), StoreCounts::default());
    }

    /// The operator's door reaches every store through `judge`.
    #[test]
    fn an_operator_address_is_admitted_at_a_store_whatever_its_class() {
        let stores = StoreRefusals::new();
        let operator = OperatorSet::new();
        let loopback = addr("/ip4/127.0.0.1/tcp/4001");
        let none: [&str; 0] = [];
        assert!(!stores.judge(store::AUTONAT_SERVERS, &operator, &loopback, none));
        assert!(operator.insert(&loopback));
        assert!(stores.judge(store::AUTONAT_SERVERS, &operator, &loopback, none));
    }
}
