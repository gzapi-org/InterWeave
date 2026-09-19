// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `transport/libp2p/CONNECTIVITY.md` §14: what a network change IS to
//! this runtime (step 10).
//!
//! The runtime has one signal for the network it is on: the set of
//! addresses its listeners have bound. A wildcard listener reports each
//! interface's address as it comes and goes, so a Wi-Fi hand-over, a
//! VPN coming up or a hotspot going away arrive here as the bound set
//! changing; a `Listen` or `StopListening` command on a specific
//! address is the same change made by hand, which is how the wire
//! tests raise one on a single host. What the set says nothing about
//! is left out of the comparison: loopback, unspecified and link-local
//! addresses are bound to an interface, not to a network, and one
//! coming or going is not a move (PR #89 round 5 found the opposite
//! mistake: comparing through the PROBE candidate rule removed every
//! private listener too, so a NAT'd profile could not see a move
//! between LANs at all). An interface change that leaves the bound set
//! intact is not seen here; binding an OS network monitor to this
//! detector is the Android step's.
//!
//! Detected ONCE, here, and told to every subsystem that holds
//! network-dependent state -- the AutoNAT client's evidence, the DCUtR
//! wrapper's attempts and cooldowns -- rather than by each of them: the
//! AutoNAT adapter used to compare the set itself, so with the client
//! off a change was seen by nothing. The first bind is not a change;
//! every later difference is, including the set emptying and filling
//! again, since the interface that returns is not known to be the one
//! that left.
//!
//! Pinned by `a_network_change_is_a_difference_in_the_bound_set_after_the_first_bind`
//! and `interface_scoped_addresses_and_only_those_are_left_out_of_the_comparison`;
//! on the wire by `tests/connectivity/tests/network_change.rs`.

use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;

/// The bound set as last observed, without the interface-scoped
/// addresses.
#[derive(Debug, Default)]
pub(super) struct NetworkSet {
    /// Sorted, deduplicated.
    listeners: Vec<String>,
    /// Whether anything has ever been bound: the first bind is not a
    /// change.
    bound_once: bool,
}

/// What changed: the addresses that left the set and those that
/// joined it, each sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NetworkChange {
    /// Bound at the last observation and not now.
    pub removed: Vec<String>,
    /// Bound now and not at the last observation.
    pub added: Vec<String>,
}

impl NetworkSet {
    /// Observe the bound set: `Some` when it differs from the last
    /// observation and something had been bound before.
    pub(super) fn observe<'a>(
        &mut self,
        bound: impl Iterator<Item = &'a Multiaddr>,
    ) -> Option<NetworkChange> {
        let mut now: Vec<String> = bound
            .filter(|a| !is_interface_scoped(a))
            .map(ToString::to_string)
            .collect();
        now.sort_unstable();
        now.dedup();
        let change = if self.bound_once && self.listeners != now {
            Some(NetworkChange {
                removed: self
                    .listeners
                    .iter()
                    .filter(|a| !now.contains(a))
                    .cloned()
                    .collect(),
                added: now
                    .iter()
                    .filter(|a| !self.listeners.contains(a))
                    .cloned()
                    .collect(),
            })
        } else {
            None
        };
        self.bound_once |= !now.is_empty();
        self.listeners = now;
        change
    }
}

/// Whether `address` is bound to an interface rather than a network:
/// loopback, unspecified, or link-local. Such a listener coming or
/// going says nothing about where this profile is, so it is left out
/// of the network-change comparison; everything else, private and
/// CGNAT ranges included, is in. Pinned by
/// `interface_scoped_addresses_and_only_those_are_left_out_of_the_comparison`.
pub(super) fn is_interface_scoped(address: &Multiaddr) -> bool {
    match address.iter().next() {
        Some(Protocol::Ip4(ip)) => ip.is_loopback() || ip.is_unspecified() || ip.is_link_local(),
        Some(Protocol::Ip6(ip)) => {
            ip.is_loopback() || ip.is_unspecified() || (ip.segments()[0] & 0xffc0) == 0xfe80
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn addrs(list: &[&str]) -> Vec<Multiaddr> {
        list.iter().map(|a| a.parse().expect("a literal")).collect()
    }

    #[test]
    fn a_network_change_is_a_difference_in_the_bound_set_after_the_first_bind() {
        let mut set = NetworkSet::default();
        // Nothing bound yet, then the first bind: not a change.
        assert_eq!(set.observe(addrs(&[]).iter()), None);
        let lan_a = "/ip4/192.168.1.5/tcp/4001";
        assert_eq!(set.observe(addrs(&[lan_a]).iter()), None, "the first bind");
        assert_eq!(set.observe(addrs(&[lan_a]).iter()), None, "unchanged");
        // A loopback listener joins: outside the comparison, no change.
        assert_eq!(
            set.observe(addrs(&[lan_a, "/ip4/127.0.0.1/tcp/4001"]).iter()),
            None
        );
        // Another LAN: the move.
        let lan_b = "/ip4/10.0.0.7/tcp/4001";
        assert_eq!(
            set.observe(addrs(&["/ip4/127.0.0.1/tcp/4001", lan_b]).iter()),
            Some(NetworkChange {
                removed: vec![lan_a.to_owned()],
                added: vec![lan_b.to_owned()],
            })
        );
        // The set empties -- the interface went away -- and fills again:
        // both are changes, since what returns is not known to be what
        // left.
        assert_eq!(
            set.observe(addrs(&[]).iter()),
            Some(NetworkChange {
                removed: vec![lan_b.to_owned()],
                added: vec![],
            })
        );
        assert_eq!(
            set.observe(addrs(&[lan_b]).iter()),
            Some(NetworkChange {
                removed: vec![],
                added: vec![lan_b.to_owned()],
            })
        );
        // Duplicates -- two listeners on one address -- are one.
        assert_eq!(set.observe(addrs(&[lan_b, lan_b]).iter()), None);
    }

    #[test]
    fn interface_scoped_addresses_and_only_those_are_left_out_of_the_comparison() {
        for scoped in [
            "/ip4/127.0.0.1/tcp/1",
            "/ip4/0.0.0.0/tcp/1",
            "/ip4/169.254.1.1/tcp/1",
            "/ip6/::1/tcp/1",
            "/ip6/::/tcp/1",
            "/ip6/fe80::1/tcp/1",
        ] {
            assert!(
                is_interface_scoped(&scoped.parse().expect("a literal")),
                "{scoped}"
            );
        }
        for counted in [
            "/ip4/192.168.1.5/tcp/1",
            "/ip4/10.0.0.7/tcp/1",
            "/ip4/100.64.0.1/tcp/1",
            "/ip4/8.8.8.8/tcp/1",
            "/ip6/fd12::1/tcp/1",
            "/ip6/2001:4860:4860::8888/tcp/1",
            "/dns4/example.invalid/tcp/1",
        ] {
            assert!(
                !is_interface_scoped(&counted.parse().expect("a literal")),
                "{counted}"
            );
        }
    }
}
