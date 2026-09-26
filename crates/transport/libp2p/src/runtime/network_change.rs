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
//! that left -- and only a removal INVALIDATES what was known
//! (`NetworkChange::invalidates`): an addition is reported and offered.
//!
//! Pinned by `a_network_change_is_a_difference_in_the_bound_set_after_the_first_bind`
//! and `interface_scoped_addresses_and_only_those_are_left_out_of_the_comparison`;
//! on the wire by `tests/connectivity/tests/dcutr.rs`'s
//! `a_network_change_lifts_the_cooldown_and_keeps_the_reservation`.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

use libp2p::Multiaddr;
use libp2p::core::ConnectedPoint;
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

impl NetworkChange {
    /// Whether what this profile knew about its reachability is stale:
    /// only when an address LEFT the set. §14 item 1 invalidates the
    /// evidence "for removed direct addresses"; an addition alone -- an
    /// interface coming up, a VPN, a wildcard listener reporting one
    /// more of a multi-homed host's addresses at startup -- says
    /// nothing against the addresses still held, so it is reported and
    /// its address becomes a candidate on the next tick, and no
    /// evidence is forgotten and no attempt given up for it. Pinned by
    /// `a_network_change_is_a_difference_in_the_bound_set_after_the_first_bind`
    /// and, on the wire, by `dcutr.rs`'s network-change test (an added
    /// listener leaves the cooldown standing; a removed one lifts it).
    #[must_use]
    pub(super) fn invalidates(&self) -> bool {
        !self.removed.is_empty()
    }
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

/// The IPs a removal took off this host: those of the removed
/// addresses that no address still bound carries -- two listeners on
/// one IP (TCP and QUIC, or two ports) losing one keep the IP, and a
/// connection from it is still over a live interface
/// (`transport/libp2p/CONNECTIVITY.md` §14 item 5). Pinned by
/// `a_removed_ip_departs_only_when_no_bound_address_still_carries_it`.
pub(super) fn departed_ips<'a>(
    change: &NetworkChange,
    bound: impl Iterator<Item = &'a Multiaddr>,
) -> BTreeSet<IpAddr> {
    let still: BTreeSet<IpAddr> = bound.filter_map(first_ip).collect();
    change
        .removed
        .iter()
        .filter_map(|a| a.parse::<Multiaddr>().ok())
        .filter_map(|a| first_ip(&a))
        .filter(|ip| !still.contains(ip))
        .collect()
}

/// The IP a connection runs from on this host, or `None` where it
/// cannot be known.
///
/// An INBOUND connection's is its endpoint's local address. An
/// OUTBOUND one's is not in anything libp2p reports -- its endpoint
/// holds the remote only -- so it is read from the kernel: the source
/// IP the routing table picks for that remote, found by `connect` on a
/// UDP socket, which sends nothing. That is the IP the TCP dial itself
/// ran from because `libp2p-tcp` 0.45.0 binds every dial to the
/// UNSPECIFIED address (only the port is reused, `lib.rs:111-128`,
/// `:379-403`), leaving the source to the same routing decision, taken
/// milliseconds earlier. A relayed connection is `None`: it runs over
/// the relay's connection, which is closed by its own local IP and
/// takes the circuit with it; so is a remote given by name (`/dns4`),
/// whose resolved IP libp2p does not report -- those are left to the
/// relay control connection's keepalive. Pinned on the wire by
/// `tests/connectivity/tests/network_change.rs`.
pub(super) fn local_ip_of(endpoint: &ConnectedPoint) -> Option<IpAddr> {
    if endpoint.is_relayed() {
        return None;
    }
    match endpoint {
        ConnectedPoint::Listener { local_addr, .. } => first_ip(local_addr),
        ConnectedPoint::Dialer { address, .. } => first_ip(address).and_then(route_source),
    }
}

/// The source IP this host would use toward `remote` now.
fn route_source(remote: IpAddr) -> Option<IpAddr> {
    let any: SocketAddr = match remote {
        IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        IpAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let socket = UdpSocket::bind(any).ok()?;
    // The port is arbitrary: a UDP `connect` only fixes the peer.
    socket.connect((remote, 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

fn first_ip(address: &Multiaddr) -> Option<IpAddr> {
    match address.iter().next()? {
        Protocol::Ip4(ip) => Some(IpAddr::V4(ip)),
        Protocol::Ip6(ip) => Some(IpAddr::V6(ip)),
        _ => None,
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
        // A second address joins: a change, reported, and it
        // invalidates nothing -- what was known about `lan_a` stands.
        let vpn = "/ip4/10.8.0.2/tcp/4001";
        let joined = set.observe(addrs(&[lan_a, vpn]).iter()).expect("a change");
        assert_eq!(
            joined,
            NetworkChange {
                removed: vec![],
                added: vec![vpn.to_owned()],
            }
        );
        assert!(!joined.invalidates(), "an addition alone is not a move");
        assert_eq!(
            set.observe(addrs(&[lan_a]).iter()).map(|c| c.invalidates()),
            Some(true)
        );
        // Another LAN: the move, and it invalidates.
        let lan_b = "/ip4/10.0.0.7/tcp/4001";
        let moved = set
            .observe(addrs(&["/ip4/127.0.0.1/tcp/4001", lan_b]).iter())
            .expect("a change");
        assert_eq!(
            moved,
            NetworkChange {
                removed: vec![lan_a.to_owned()],
                added: vec![lan_b.to_owned()],
            }
        );
        assert!(moved.invalidates());
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
    fn a_removed_ip_departs_only_when_no_bound_address_still_carries_it() {
        let change = NetworkChange {
            removed: vec![
                "/ip4/192.168.1.5/tcp/4001".to_owned(),
                "/ip4/10.0.0.7/tcp/4001".to_owned(),
            ],
            added: vec![],
        };
        // 192.168.1.5 is still bound on another port; 10.0.0.7 is not.
        let bound = addrs(&["/ip4/192.168.1.5/tcp/4002", "/ip4/127.0.0.1/tcp/1"]);
        let departed = departed_ips(&change, bound.iter());
        assert_eq!(
            departed.into_iter().collect::<Vec<_>>(),
            vec!["10.0.0.7".parse::<IpAddr>().expect("an ip")]
        );
        // Nothing bound: both depart.
        assert_eq!(departed_ips(&change, std::iter::empty()).len(), 2);
    }

    #[test]
    fn a_connections_local_ip_is_its_listener_address_or_the_route_toward_its_remote() {
        let inbound = ConnectedPoint::Listener {
            local_addr: "/ip4/192.168.1.5/tcp/4001".parse().expect("valid"),
            send_back_addr: "/ip4/192.168.1.9/tcp/5555".parse().expect("valid"),
        };
        assert_eq!(
            local_ip_of(&inbound),
            Some("192.168.1.5".parse().expect("ip"))
        );
        // Toward loopback the kernel's route runs from loopback.
        let outbound = ConnectedPoint::Dialer {
            address: "/ip4/127.0.0.1/tcp/4001".parse().expect("valid"),
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::Reuse,
        };
        assert_eq!(
            local_ip_of(&outbound),
            Some("127.0.0.1".parse().expect("ip"))
        );
        // A circuit and a name are not known.
        let circuit = ConnectedPoint::Dialer {
            address: "/ip4/127.0.0.1/tcp/4001/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN/p2p-circuit"
                .parse()
                .expect("valid"),
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::Reuse,
        };
        assert_eq!(local_ip_of(&circuit), None);
        let named = ConnectedPoint::Dialer {
            address: "/dns4/example.invalid/tcp/4001".parse().expect("valid"),
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::Reuse,
        };
        assert_eq!(local_ip_of(&named), None);
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
