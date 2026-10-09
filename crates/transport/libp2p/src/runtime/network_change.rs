// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `transport/libp2p/CONNECTIVITY.md` §14: what a network change IS to
//! this runtime (step 10; the platform's view since §20 step 5).
//!
//! A network change is a move in the set of IP addresses this host is
//! known to hold. Two sources report that set, and both feed this ONE
//! detector (architect-cto's ruling of 2026-10-09, relay seq 33736):
//!
//! - the LISTENERS: the addresses they have bound. A wildcard listener
//!   reports each interface's address as it comes and goes, so a Wi-Fi
//!   hand-over, a VPN coming up or a hotspot going away arrive as the
//!   bound set changing; a `Listen` or `StopListening` command that adds
//!   or removes an IP is the same change made by hand, which is how the
//!   wire tests raise one on a single host. On Android `if-watch`
//!   polls `getifaddrs` every 10 s (its netlink backend is Linux-only),
//!   and whether that answers an app at all is a device question;
//! - the PLATFORM's view (`NetworkView`), a snapshot of every usable
//!   address the OS reports, handed in by the host's own network
//!   monitor. Until the first view arrives the platform's view is
//!   unknown, and the listeners are the only source.
//!
//! Compared as IPs, not as listener addresses: two listeners on one IP
//! (two ports, TCP and QUIC) losing one leave the host on the same
//! network, and a view names no ports. Each source's observation is
//! compared with THAT SOURCE's previous one, and the difference is
//! applied to the one known set; a change is reported only when the
//! known set moves. So the platform's view reports a hand-over at once
//! and the listener poll that sees the same move 10 s later finds
//! nothing left to report -- §14's handling runs once. Where the two
//! sources disagree for good (an address one reports and the other
//! never does), neither keeps re-reporting it, since each only applies
//! its own differences. Where the VIEW lags the listeners across two
//! moves -- an address gone and back on the listeners before the view
//! saw it go -- the late view reports a move that already reversed and
//! the next view reverses it again: a bounded spurious pair, never a
//! stuck set. The listeners lagging the view cannot do that, since the
//! view protects what it names (below).
//! A Wi-Fi reconnect that returns the same addresses (SPIKE-008 L7, a
//! new network id on the same Wi-Fi) is NO change here: the host passes
//! a view only when its addresses differ, and the connections that
//! survived prove themselves by their own keepalives.
//!
//! What a set says nothing about is left out of the comparison:
//! loopback, unspecified and link-local addresses are bound to an
//! interface, not to a network, and one coming or going is not a move
//! (PR #89 round 5 found the opposite mistake: comparing through the
//! PROBE candidate rule removed every private listener too, so a NAT'd
//! profile could not see a move between LANs at all).
//!
//! Detected ONCE, here, and told to every subsystem that holds
//! network-dependent state -- the AutoNAT client's evidence, the DCUtR
//! wrapper's attempts and cooldowns, the dial backoff -- rather than by
//! each of them: the AutoNAT adapter used to compare the set itself, so
//! with the client off a change was seen by nothing. The first time the
//! set fills is not a change -- nothing is reported or invalidated --
//! but it runs an addition's lift ([`NetworkSet::take_filled`]): a
//! runtime started offline dialled and failed before it, and coming
//! online is exactly the move the lift is for (ADR-0011 A 2026-10-09).
//! Every later difference is a change, including the set emptying and
//! filling again, since the interface that returns is not known to be
//! the one that left -- and only a removal INVALIDATES what was known
//! (`NetworkChange::invalidates`): an addition is reported, offered,
//! and makes the peers held off dialable once.
//!
//! THE VIEW, ONCE PRESENT, IS AUTHORITATIVE for the addresses it names
//! (the same ruling): the listeners add to the known set and never
//! remove an address the view still holds -- a listener closed while
//! the platform still reports its address leaves the set intact, and
//! the view's own later departure of it is the removal. Without that,
//! a one-sided drop took the address out of the set while the platform
//! held it, and its real departure later was no removal at all, so
//! connections made from it in between survived it.
//!
//! Pinned by `a_network_change_is_a_move_of_the_known_ip_set_after_it_first_fills`,
//! `the_two_sources_report_one_move_once` and
//! `interface_scoped_addresses_and_only_those_are_left_out_of_the_comparison`;
//! on the wire by `tests/connectivity/tests/network_change.rs` and
//! `dcutr.rs`'s `a_network_change_lifts_the_cooldown_and_keeps_the_reservation`.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

use libp2p::Multiaddr;
use libp2p::core::ConnectedPoint;
use libp2p::multiaddr::Protocol;

use super::messages::PeerPath;

/// The IP set this host is known to hold, and what each source last
/// said of it -- without the interface-scoped addresses.
#[derive(Debug, Default)]
pub(super) struct NetworkSet {
    /// The one set a change is a move of.
    known: BTreeSet<IpAddr>,
    /// The IPs the listeners had bound at their last observation.
    listeners: BTreeSet<IpAddr>,
    /// The platform's last view; `None` until the first, when the
    /// platform's view is unknown rather than empty.
    platform: Option<BTreeSet<IpAddr>>,
    /// Whether the known set has ever held an address: the first time
    /// it fills is not a change.
    filled_once: bool,
    /// The first fill happened and its lift has not been taken.
    filled_untaken: bool,
    /// Where the bound IPs the host no longer holds are published, for
    /// rule 3 at the root funnel ([`crate::held_listeners`]).
    published: crate::held_listeners::HeldListeners,
}

/// What changed: the IPs that left the known set and those that joined
/// it, each sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NetworkChange {
    /// Known at the last move and not now: the IPs that DEPARTED this
    /// host.
    pub removed: Vec<IpAddr>,
    /// Known now and not at the last move.
    pub added: Vec<IpAddr>,
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
    /// `a_network_change_is_a_move_of_the_known_ip_set_after_it_first_fills`
    /// and, on the wire, by `dcutr.rs`'s network-change test (an added
    /// address leaves the cooldown standing; a removed one lifts it).
    #[must_use]
    pub(super) fn invalidates(&self) -> bool {
        !self.removed.is_empty()
    }

    /// Whether an address JOINED: the move after which a peer held off
    /// by failures on the old network is worth one dial at once
    /// (`ConnectionManager::network_added`).
    #[must_use]
    pub(super) fn adds(&self) -> bool {
        !self.added.is_empty()
    }
}

impl NetworkSet {
    /// Observe the listeners' bound set: `Some` when it moved the known
    /// set after that set first filled.
    pub(super) fn observe_listeners<'a>(
        &mut self,
        bound: impl Iterator<Item = &'a Multiaddr>,
    ) -> Option<NetworkChange> {
        let now: BTreeSet<IpAddr> = bound
            .filter(|a| !is_interface_scoped(a))
            .filter_map(first_ip)
            .collect();
        let before = std::mem::replace(&mut self.listeners, now.clone());
        let held_by_view = self.platform.clone().unwrap_or_default();
        self.apply(&before, &now, &held_by_view)
    }

    /// Observe the platform's view: `Some` when it moved the known set
    /// after that set first filled. The first view is compared with
    /// nothing, so it can add what the listeners have not bound and
    /// remove nothing: before it the platform said nothing, which is
    /// not the same as saying an address was gone.
    pub(super) fn observe_view(&mut self, addresses: &[IpAddr]) -> Option<NetworkChange> {
        let now: BTreeSet<IpAddr> = addresses
            .iter()
            .copied()
            .filter(|ip| !is_interface_scoped_ip(*ip))
            .collect();
        let before = self.platform.replace(now.clone()).unwrap_or_default();
        self.apply(&before, &now, &BTreeSet::new())
    }

    /// A detector that publishes the bound IPs the host no longer holds
    /// to `held` after every observation.
    pub(super) fn publishing_to(held: crate::held_listeners::HeldListeners) -> Self {
        Self {
            published: held,
            ..Self::default()
        }
    }

    /// The own listeners rule 3 counts (ADR-0052 A 2026-10-09): those of
    /// `bound` this host [`holds`](Self::holds), as strings.
    pub(super) fn own_listeners<'a>(
        &self,
        bound: impl Iterator<Item = &'a Multiaddr>,
    ) -> Vec<String> {
        bound
            .filter(|a| self.holds(a))
            .map(ToString::to_string)
            .collect()
    }

    /// Whether the first fill of the known set has happened since this
    /// was last asked: the runtime runs an addition's lift for it,
    /// though it reported no change. `true` once.
    pub(super) fn take_filled(&mut self) -> bool {
        std::mem::take(&mut self.filled_untaken)
    }

    /// Whether `address` is on an IP this host is known to hold, or is
    /// interface-scoped or names no IP: what may still be offered as a
    /// candidate. A listener on an IP the view has said departed is
    /// bound until the listener poll catches up, and must not be offered
    /// meanwhile.
    pub(super) fn holds(&self, address: &Multiaddr) -> bool {
        first_ip(address).is_none_or(|ip| is_interface_scoped_ip(ip) || self.known.contains(&ip))
    }

    /// Apply one source's difference to the known set; an IP in
    /// `protected` is never removed.
    fn apply(
        &mut self,
        before: &BTreeSet<IpAddr>,
        now: &BTreeSet<IpAddr>,
        protected: &BTreeSet<IpAddr>,
    ) -> Option<NetworkChange> {
        let removed: Vec<IpAddr> = before
            .difference(now)
            .filter(|ip| !protected.contains(*ip))
            .filter(|ip| self.known.remove(*ip))
            .copied()
            .collect();
        let added: Vec<IpAddr> = now
            .difference(before)
            .filter(|ip| self.known.insert(**ip))
            .copied()
            .collect();
        let moved = !removed.is_empty() || !added.is_empty();
        let change = (moved && self.filled_once).then_some(NetworkChange { removed, added });
        if !self.filled_once && !self.known.is_empty() {
            self.filled_once = true;
            self.filled_untaken = true;
        }
        self.published
            .set_unheld(self.listeners.difference(&self.known).copied().collect());
        change
    }
}

/// Whether a removal whose departed IPs are `departed` -- the change's
/// `removed`, which the known set no longer holds, so two listeners on
/// one IP losing one depart nothing -- closes a connection running from
/// `local_ip` over `path` (`transport/libp2p/CONNECTIVITY.md` §14 item
/// 5).
///
/// A known local IP closes when it departed. An UNKNOWN one on a direct
/// connection -- an outbound dial by name, whose resolved address libp2p
/// does not report ([`local_ip_of`]) -- closes on ANY departure: a relay
/// control connection configured by name is exactly what the rule is
/// for, and leaving it to the keepalive left it standing some 45 s
/// (#129 review F3); closing it when it may have survived costs one
/// reconnect. A relayed connection is never closed here: it runs over
/// its relay's connection, which is judged by its own IP and takes the
/// circuit with it. Pinned by
/// `a_removal_closes_a_departed_ip_and_an_unknown_direct_one_only`.
pub(super) fn closes(
    local_ip: Option<IpAddr>,
    path: PeerPath,
    departed: &BTreeSet<IpAddr>,
) -> bool {
    match local_ip {
        Some(ip) => departed.contains(&ip),
        None => path == PeerPath::Direct && !departed.is_empty(),
    }
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
/// whose resolved IP libp2p does not report -- [`closes`] says what a
/// removal does with each. Pinned on the wire by
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
    first_ip(address).is_some_and(is_interface_scoped_ip)
}

/// [`is_interface_scoped`] for an IP, as the platform's view names one.
fn is_interface_scoped_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_unspecified() || ip.is_link_local(),
        IpAddr::V6(ip) => {
            ip.is_loopback() || ip.is_unspecified() || (ip.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn addrs(list: &[&str]) -> Vec<Multiaddr> {
        list.iter().map(|a| a.parse().expect("a literal")).collect()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("an ip")
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "compared with what `observe_*` returns"
    )]
    fn change(removed: &[&str], added: &[&str]) -> Option<NetworkChange> {
        Some(NetworkChange {
            removed: removed.iter().map(|a| ip(a)).collect(),
            added: added.iter().map(|a| ip(a)).collect(),
        })
    }

    #[test]
    fn a_network_change_is_a_move_of_the_known_ip_set_after_it_first_fills() {
        let mut set = NetworkSet::default();
        // Nothing bound yet, then the first bind: not a change.
        assert_eq!(set.observe_listeners(addrs(&[]).iter()), None);
        let lan_a = "/ip4/192.168.1.5/tcp/4001";
        assert_eq!(
            set.observe_listeners(addrs(&[lan_a]).iter()),
            None,
            "the first bind"
        );
        assert_eq!(
            set.observe_listeners(addrs(&[lan_a]).iter()),
            None,
            "unchanged"
        );
        // A loopback listener joins: outside the comparison, no change.
        assert_eq!(
            set.observe_listeners(addrs(&[lan_a, "/ip4/127.0.0.1/tcp/4001"]).iter()),
            None
        );
        // A second PORT on the same IP comes and goes: the host is on the
        // same network, so neither is a change.
        let lan_a_quic = "/ip4/192.168.1.5/udp/4001/quic-v1";
        assert_eq!(
            set.observe_listeners(addrs(&[lan_a, lan_a_quic]).iter()),
            None,
            "a second listener on a known IP"
        );
        assert_eq!(
            set.observe_listeners(addrs(&[lan_a_quic]).iter()),
            None,
            "one of two listeners on one IP going"
        );
        // A second IP joins: a change, reported, and it invalidates
        // nothing -- what was known about 192.168.1.5 stands.
        let vpn = "/ip4/10.8.0.2/tcp/4001";
        let joined = set.observe_listeners(addrs(&[lan_a, vpn]).iter());
        assert_eq!(joined, change(&[], &["10.8.0.2"]));
        let joined = joined.expect("a change");
        assert!(!joined.invalidates(), "an addition alone is not a move");
        assert!(joined.adds());
        let left = set
            .observe_listeners(addrs(&[lan_a]).iter())
            .expect("a change");
        assert!(left.invalidates() && !left.adds());
        // Another LAN: the move, and it invalidates.
        let lan_b = "/ip4/10.0.0.7/tcp/4001";
        assert_eq!(
            set.observe_listeners(addrs(&["/ip4/127.0.0.1/tcp/4001", lan_b]).iter()),
            change(&["192.168.1.5"], &["10.0.0.7"])
        );
        // The set empties -- the interface went away -- and fills again:
        // both are changes, since what returns is not known to be what
        // left.
        assert_eq!(
            set.observe_listeners(addrs(&[]).iter()),
            change(&["10.0.0.7"], &[])
        );
        assert_eq!(
            set.observe_listeners(addrs(&[lan_b]).iter()),
            change(&[], &["10.0.0.7"])
        );
        // Duplicates -- two listeners on one address -- are one.
        assert_eq!(set.observe_listeners(addrs(&[lan_b, lan_b]).iter()), None);
    }

    #[test]
    fn the_two_sources_report_one_move_once() {
        let mut set = NetworkSet::default();
        let wifi = "/ip4/192.168.1.5/tcp/0";
        assert_eq!(
            set.observe_listeners(addrs(&[wifi]).iter()),
            None,
            "the first bind"
        );
        // The first view agrees: nothing moved. Its interface-scoped
        // addresses are left out as the listeners' are.
        assert_eq!(
            set.observe_view(&[ip("192.168.1.5"), ip("127.0.0.1"), ip("fe80::1")]),
            None
        );
        // THE HAND-OVER, seen by the view first: reported at once ...
        assert_eq!(
            set.observe_view(&[ip("100.64.3.9")]),
            change(&["192.168.1.5"], &["100.64.3.9"])
        );
        // ... and the listener poll that sees the same move later finds
        // nothing left to report.
        assert_eq!(
            set.observe_listeners(addrs(&["/ip4/100.64.3.9/tcp/0"]).iter()),
            None,
            "the late poll"
        );
        // The same view twice is no change.
        assert_eq!(set.observe_view(&[ip("100.64.3.9")]), None, "idempotent");
        // OFFLINE: an empty view removes everything it named.
        assert_eq!(set.observe_view(&[]), change(&["100.64.3.9"], &[]));
        assert_eq!(
            set.observe_listeners(addrs(&[]).iter()),
            None,
            "the late poll"
        );
        // Back, seen by the listeners first this time.
        assert_eq!(
            set.observe_listeners(addrs(&[wifi]).iter()),
            change(&[], &["192.168.1.5"])
        );
        assert_eq!(
            set.observe_view(&[ip("192.168.1.5")]),
            None,
            "the late view"
        );
        // A lasting disagreement -- an address only the view names --
        // is reported once and not again.
        assert_eq!(
            set.observe_view(&[ip("192.168.1.5"), ip("10.8.0.2")]),
            change(&[], &["10.8.0.2"])
        );
        assert_eq!(set.observe_listeners(addrs(&[wifi]).iter()), None);
        assert_eq!(set.observe_listeners(addrs(&[wifi]).iter()), None);
    }

    #[test]
    fn a_first_view_before_any_bind_fills_the_set_and_is_not_a_change() {
        let mut set = NetworkSet::default();
        assert_eq!(
            set.observe_view(&[ip("192.168.1.5")]),
            None,
            "the first fill"
        );
        assert_eq!(
            set.observe_listeners(addrs(&["/ip4/192.168.1.5/tcp/0"]).iter()),
            None,
            "the listener binds what the view already said"
        );
        assert_eq!(
            set.observe_view(&[]),
            change(&["192.168.1.5"], &[]),
            "and the set it filled moves"
        );
    }

    #[test]
    fn the_first_fill_is_no_change_and_its_lift_is_taken_once() {
        let mut set = NetworkSet::default();
        // Offline at start: loopback only, and an empty first view.
        assert_eq!(
            set.observe_listeners(addrs(&["/ip4/127.0.0.1/tcp/0"]).iter()),
            None
        );
        assert_eq!(set.observe_view(&[]), None);
        assert!(!set.take_filled(), "nothing has filled");
        // Online: the first fill, reported as no change ...
        assert_eq!(set.observe_view(&[ip("192.168.1.5")]), None);
        // ... whose lift is taken once.
        assert!(set.take_filled());
        assert!(!set.take_filled(), "once");
        // A later addition is a change, and no fill.
        assert_eq!(
            set.observe_view(&[ip("192.168.1.5"), ip("10.8.0.2")]),
            change(&[], &["10.8.0.2"])
        );
        assert!(!set.take_filled());
        // Emptied and refilled: a change each way, never a first fill.
        let _ = set.observe_view(&[]);
        assert!(set.observe_view(&[ip("192.168.1.5")]).is_some());
        assert!(!set.take_filled());
    }

    #[test]
    fn the_view_once_present_holds_what_it_names_against_the_listeners() {
        let mut set = NetworkSet::default();
        let x = "/ip4/192.168.1.5/tcp/0";
        let _ = set.observe_listeners(addrs(&[x]).iter());
        assert_eq!(set.observe_view(&[ip("192.168.1.5")]), None);
        // The listener drops X while the view still holds it: no
        // removal, and X is still held.
        assert_eq!(set.observe_listeners(addrs(&[]).iter()), None);
        assert!(set.holds(&x.parse().expect("valid")));
        // The view then drops X: that is the removal.
        assert_eq!(set.observe_view(&[]), change(&["192.168.1.5"], &[]));
        // The control: an address the view never named is removed by
        // the listeners as ever.
        let y = "/ip4/10.0.0.7/tcp/0";
        assert_eq!(
            set.observe_listeners(addrs(&[y]).iter()),
            change(&[], &["10.0.0.7"])
        );
        assert_eq!(
            set.observe_listeners(addrs(&[]).iter()),
            change(&["10.0.0.7"], &[])
        );
    }

    #[test]
    fn a_listener_is_held_while_its_ip_is_known() {
        let mut set = NetworkSet::default();
        let x: Multiaddr = "/ip4/192.168.1.5/tcp/4001".parse().expect("valid");
        let _ = set.observe_listeners(std::iter::once(&x));
        assert!(set.holds(&x));
        let _ = set.observe_view(&[ip("192.168.1.5")]);
        let _ = set.observe_view(&[]);
        assert!(!set.holds(&x), "the view said it departed");
        for always in [
            "/ip4/127.0.0.1/tcp/1",
            "/ip6/fe80::1/tcp/1",
            "/dns4/example.invalid/tcp/1",
        ] {
            assert!(set.holds(&always.parse().expect("valid")), "{always}");
        }
    }

    #[test]
    fn the_detector_publishes_the_bound_ips_it_no_longer_holds() {
        let held = crate::held_listeners::HeldListeners::new();
        let mut set = NetworkSet::publishing_to(held.clone());
        let x: Multiaddr = "/ip4/192.168.1.5/tcp/4001".parse().expect("valid");
        let loopback: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("valid");
        let bound = [x.clone(), loopback.clone()];
        let _ = set.observe_listeners(bound.iter());
        let _ = set.observe_view(&[ip("192.168.1.5")]);
        assert!(held.holds(&x), "the control: held");
        assert_eq!(
            set.own_listeners(bound.iter()),
            vec![x.to_string(), loopback.to_string()]
        );
        // The view says X departed; its listener is still bound.
        let _ = set.observe_view(&[]);
        assert!(!held.holds(&x), "published as no longer held");
        assert_eq!(
            set.own_listeners(bound.iter()),
            vec![loopback.to_string()],
            "and not this node's for rule 3"
        );
        // The poll catches up: X is no longer bound, nothing is unheld.
        let _ = set.observe_listeners(std::iter::once(&loopback));
        assert!(held.holds(&x), "unheld means bound and not held");
    }

    /// The module note's "bounded spurious pair, never a stuck set": the
    /// VIEW lagging the listeners across two moves -- X gone and back on
    /// the listeners before the view saw it go -- reports a move that
    /// already reversed, and the next view reverses it again; X ends held.
    #[test]
    fn a_view_lagging_two_moves_makes_one_spurious_pair_and_ends_held() {
        let mut set = NetworkSet::default();
        let x: Multiaddr = "/ip4/192.168.1.5/tcp/4001".parse().expect("valid");
        let _ = set.observe_listeners(std::iter::once(&x));
        let _ = set.observe_view(&[ip("192.168.1.5")]);
        // The listeners lose X and regain it: the view holds X, so no
        // change either way.
        assert_eq!(set.observe_listeners(std::iter::empty()), None);
        assert_eq!(set.observe_listeners(std::iter::once(&x)), None);
        // The late view drops X: the spurious removal ...
        assert_eq!(set.observe_view(&[]), change(&["192.168.1.5"], &[]));
        // ... and the next view names it again: the spurious addition.
        assert_eq!(
            set.observe_view(&[ip("192.168.1.5")]),
            change(&[], &["192.168.1.5"])
        );
        assert!(set.holds(&x), "never a stuck set");
        assert_eq!(set.observe_view(&[ip("192.168.1.5")]), None, "and quiet");
    }

    #[test]
    fn a_first_view_removes_nothing_it_never_named() {
        let mut set = NetworkSet::default();
        let _ = set.observe_listeners(addrs(&["/ip4/192.168.1.5/tcp/0"]).iter());
        // The platform's first word omits what the listeners bound: it
        // adds what it names, and says nothing of the rest -- before it,
        // the platform's view was unknown, not empty.
        assert_eq!(
            set.observe_view(&[ip("10.8.0.2")]),
            change(&[], &["10.8.0.2"])
        );
        // Its second word is compared with its first: dropping what it
        // named is a removal, and what it never named still stands.
        assert_eq!(set.observe_view(&[]), change(&["10.8.0.2"], &[]));
    }

    #[test]
    fn a_removal_closes_a_departed_ip_and_an_unknown_direct_one_only() {
        let lan: IpAddr = "192.168.1.5".parse().expect("ip");
        let other: IpAddr = "10.0.0.7".parse().expect("ip");
        let departed = BTreeSet::from([lan]);
        let none = BTreeSet::new();
        assert!(closes(Some(lan), PeerPath::Direct, &departed));
        assert!(
            !closes(Some(other), PeerPath::Direct, &departed),
            "still bound"
        );
        assert!(
            closes(None, PeerPath::Direct, &departed),
            "a named dial, on any departure"
        );
        assert!(
            !closes(None, PeerPath::Direct, &none),
            "an addition departs nothing"
        );
        assert!(
            !closes(None, PeerPath::Relayed, &departed),
            "a circuit follows its relay"
        );
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
        // AND TOWARD A REMOTE ELSEWHERE, where the two differ: what is
        // recorded is this host's source, never the remote's own IP --
        // `None` where no route exists, an address of this host where one
        // does (#129 review F4: toward loopback the two coincide and a
        // mutation reading the remote passed).
        let remote: IpAddr = "192.0.2.1".parse().expect("ip");
        let elsewhere = ConnectedPoint::Dialer {
            address: "/ip4/192.0.2.1/tcp/4001".parse().expect("valid"),
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::Reuse,
        };
        assert_ne!(local_ip_of(&elsewhere), Some(remote));
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
