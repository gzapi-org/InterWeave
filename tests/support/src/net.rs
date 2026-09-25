// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The host's private-range address, for tests that need two runtimes
//! to learn each other's advertised addresses.
//!
//! # Why not loopback
//!
//! ADR-0052's floor refuses a loopback address whoever supplies it, and
//! since A 2026-09-20 that covers what a peer advertises through
//! Identify, what Kademlia routes and what the relay and AutoNAT drivers
//! learn. Two runtimes on `127.0.0.1` therefore never learn or route
//! each other. Rule 3 admits a private (RFC 1918) address beside a
//! private listener of the same family, which is exactly what two
//! runtimes on this host's own private address are. There is no
//! test-only knob to admit loopback, by design.
//!
//! # Why a host without one FAILS the test
//!
//! An earlier shape returned early with an `eprintln!`, and described
//! that as standing the test down "loudly". It was neither: an early
//! `return` from a `#[test]` is a PASS, and libtest captures a passing
//! test's stderr, so the message was not even shown. On a host whose
//! route to 10/8 is not via an RFC 1918 source address, the evidence
//! these tests are cited for would have disappeared green (#111
//! re-review P2-3). A test that cannot ask its question has not
//! answered it, so this panics and names the reason -- the policy this
//! crate's root states for every harness. The hosted CI runners have a
//! private address.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

/// This host's private-range (RFC 1918) IPv4 address, or a panic that
/// says why the test cannot run here.
///
/// Read off an unconnected UDP socket "connected" to a private
/// destination, which asks the kernel which source address it would
/// use: no packet is sent.
///
/// # Panics
/// When the host has no non-loopback RFC 1918 IPv4 address -- the test
/// that asked cannot produce its evidence on this host, and must not
/// pass as though it had.
#[must_use]
pub fn require_private_interface_v4() -> Ipv4Addr {
    private_interface_v4().unwrap_or_else(|| {
        panic!(
            "this host has no private-range (RFC 1918) IPv4 interface. The test needs one: \
             ADR-0052 refuses a loopback address a peer supplies, so two runtimes on \
             127.0.0.1 never learn or route each other, and there is no knob to admit it. \
             Run it on a host with a private address (the hosted CI runners have one)."
        )
    })
}

fn private_interface_v4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("10.255.255.255:9").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if ip.is_private() && !ip.is_loopback() => Some(ip),
        _ => None,
    }
}
