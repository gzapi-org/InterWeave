// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! ADR-0052 rule 3 (A 2026-10-09): "this node holds a private listener
//! of that family" means a listener that is BOUND and on an IP the host
//! is KNOWN to hold.
//!
//! The two differ while the platform's view has said an IP departed and
//! the listener poll has not caught up -- on Android up to 10 s, or for
//! good if `getifaddrs` answers nothing there. A listener on the
//! departed IP is still bound for that time, and counting it would keep
//! admitting private candidates of a LAN this host has left.
//!
//! The runtime's detector (`runtime/network_change.rs`) knows which
//! bound IPs are no longer held, and publishes them here after every
//! observation; every place that applies rule 3 reads it -- the
//! runtime's learn sites through the detector itself, and the root
//! funnel, which tracks its own listeners from the Swarm's events and
//! so cannot ask the detector directly, through this handle. One record,
//! so the predicate is spelled once: [`HeldListeners::holds`].

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};

use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;

/// The IPs this node's listeners still bind that the host is no longer
/// known to hold, shared between the runtime that writes it and the
/// root funnel that reads it. Bounded by the listeners this PROCESS
/// binds, never by anything a remote party chooses.
#[derive(Clone, Debug, Default)]
pub struct HeldListeners {
    unheld: Arc<Mutex<BTreeSet<IpAddr>>>,
}

impl HeldListeners {
    /// A handle with nothing departed: every bound listener is held.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the set of bound IPs the host no longer holds.
    pub fn set_unheld(&self, unheld: BTreeSet<IpAddr>) {
        *self.unheld.lock().unwrap_or_else(PoisonError::into_inner) = unheld;
    }

    /// Whether a bound listener at `address` counts as this node's for
    /// rule 3: every one does, unless its IP is one the host no longer
    /// holds. An address naming no IP is held.
    #[must_use]
    pub fn holds(&self, address: &Multiaddr) -> bool {
        let ip = match address.iter().next() {
            Some(Protocol::Ip4(ip)) => IpAddr::V4(ip),
            Some(Protocol::Ip6(ip)) => IpAddr::V6(ip),
            _ => return true,
        };
        !self
            .unheld
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&ip)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn a_listener_is_held_unless_its_ip_was_published_as_departed() {
        let held = HeldListeners::new();
        let lan: Multiaddr = "/ip4/192.168.1.5/tcp/4001".parse().expect("valid");
        let other: Multiaddr = "/ip4/10.0.0.7/tcp/4001".parse().expect("valid");
        assert!(held.holds(&lan), "nothing published");
        held.set_unheld(BTreeSet::from(["192.168.1.5".parse().expect("ip")]));
        assert!(!held.holds(&lan), "its IP departed");
        assert!(held.holds(&other), "the control: another IP");
        assert!(
            held.clone()
                .holds(&"/dns4/example.invalid/tcp/1".parse().expect("valid")),
            "no IP, held"
        );
        // A clone is the same record.
        let reader = held.clone();
        held.set_unheld(BTreeSet::new());
        assert!(reader.holds(&lan), "the IP returned");
    }
}
