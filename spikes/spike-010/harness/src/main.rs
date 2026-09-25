// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SPIKE-010's flood measurement: what the mDNS transport behaviour's
//! record store does under N distinct announcements.
//!
//! # The mechanism, as released
//!
//! `libp2p-mdns 0.49.0` keeps every `(PeerId, address)` it hears in
//! `Behaviour::discovered_nodes`, an uncapped `SmallVec`, and finds an
//! existing pair by a linear scan before each insert
//! (`behaviour.rs:141`, `:328-331`). It accepts an UNSOLICITED response
//! -- a packet with no questions (`query.rs:57`) -- and takes the expiry
//! from the announcer's own TTL. So any host on the multicast domain
//! chooses both how many entries the store holds and how long each
//! stays, and each new one costs a scan of all the others.
//!
//! # What this measures
//!
//! For each target N it announces distinct peers until the store holds
//! N of them, then records the store's size, the wall time, the time
//! spent inside `Behaviour::poll` (where the scan runs) and the
//! process's resident memory. The store's size is read through the
//! crate's own public `discovered_nodes()`; nothing here reaches past the
//! public API.
//!
//! # Where it runs
//!
//! Inside a private network namespace with one dummy interface (see
//! `run.sh`). The crate skips loopback interfaces, and on the
//! development host the shared interface does not loop multicast back
//! (measured 2026-09-25: a probe sent to 224.0.0.251 over it never
//! arrived, the same probe on loopback did). A dummy interface in a
//! namespace nobody else uses is a domain CHOSEN rather than inherited,
//! which is SPIKE-010's own rule for evidence.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::task::Poll;
use std::time::{Duration, Instant};

use futures::future::poll_fn;
use libp2p::identity::Keypair;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{PeerId, mdns};

/// Peers per announcement packet. Each carries one PTR answer and one
/// TXT additional, about 150 bytes, so sixteen stay well inside the
/// crate's 4096-byte receive buffer (`iface.rs`, `recv_buffer`).
const PEERS_PER_PACKET: usize = 16;

/// How long one stage may take before it is recorded as stalled.
const STAGE_TIMEOUT: Duration = Duration::from_secs(120);

/// The announced TTL, in seconds: long enough that nothing expires
/// during the run, so the store's size is the announcements' count.
const TTL_SECS: u32 = 3600;

const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);

fn append_name(out: &mut Vec<u8>, labels: &[&[u8]]) {
    for label in labels {
        out.push(u8::try_from(label.len()).expect("a label is under 64 bytes"));
        out.extend_from_slice(label);
    }
    out.push(0);
}

fn append_record_header(out: &mut Vec<u8>, rtype: u16, ttl: u32, rdata_len: usize) {
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&0x0001_u16.to_be_bytes()); // class IN
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&u16::try_from(rdata_len).expect("rdata fits").to_be_bytes());
}

/// One unsolicited mDNS response announcing `peers`, each at one address,
/// in the shape `MdnsResponse::new` accepts: a PTR answer for
/// `_p2p._udp.local.` per peer, and a TXT additional under the PTR's
/// target holding `dnsaddr=<address>/p2p/<peer>`.
fn announcement(peers: &[(PeerId, u32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4096);
    out.extend_from_slice(&0_u16.to_be_bytes()); // id
    out.extend_from_slice(&0x8400_u16.to_be_bytes()); // response, authoritative
    out.extend_from_slice(&0_u16.to_be_bytes()); // questions: none -- unsolicited
    out.extend_from_slice(&u16::try_from(peers.len()).expect("few").to_be_bytes());
    out.extend_from_slice(&0_u16.to_be_bytes()); // authorities
    out.extend_from_slice(&u16::try_from(peers.len()).expect("few").to_be_bytes());

    let names: Vec<String> = peers.iter().map(|(_, n)| format!("flood{n}")).collect();
    for name in &names {
        append_name(&mut out, &[b"_p2p", b"_udp", b"local"]);
        let mut target = Vec::new();
        append_name(&mut target, &[name.as_bytes(), b"local"]);
        append_record_header(&mut out, 12, TTL_SECS, target.len()); // PTR
        out.extend_from_slice(&target);
    }
    for ((peer, n), name) in peers.iter().zip(&names) {
        append_name(&mut out, &[name.as_bytes(), b"local"]);
        // One distinct address per peer: 10.99.<hi>.<lo>, port 4001.
        let value = format!(
            "dnsaddr=/ip4/10.99.{}.{}/tcp/4001/p2p/{peer}",
            (n / 250) % 250 + 1,
            n % 250 + 1
        );
        let mut rdata = vec![u8::try_from(value.len()).expect("under 256")];
        rdata.extend_from_slice(value.as_bytes());
        append_record_header(&mut out, 16, TTL_SECS, rdata.len()); // TXT
        out.extend_from_slice(&rdata);
    }
    out
}

fn resident_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let iface: Ipv4Addr = std::env::var("FLOOD_IFACE")
        .expect("FLOOD_IFACE: the namespace's dummy interface address (run.sh sets it)")
        .parse()
        .expect("an IPv4 address");
    let targets: Vec<usize> = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "256,1024,4096,16384".to_owned())
        .split(',')
        .map(|t| t.parse().expect("a count"))
        .collect();
    let max = *targets.iter().max().expect("one target");

    let local = Keypair::generate_ed25519().public().to_peer_id();
    let config = mdns::Config {
        ttl: Duration::from_secs(u64::from(TTL_SECS)),
        query_interval: Duration::from_secs(u64::from(TTL_SECS)),
        enable_ipv6: false,
    };
    let mut behaviour =
        mdns::tokio::Behaviour::new(config, local).expect("the interface watcher starts");

    let sender = UdpSocket::bind(SocketAddrV4::new(iface, 0)).expect("bind the flooder");
    sender.set_multicast_loop_v4(true).expect("loop");
    let peers: Vec<(PeerId, u32)> = (0..max)
        .map(|n| {
            (
                Keypair::generate_ed25519().public().to_peer_id(),
                u32::try_from(n).expect("fits"),
            )
        })
        .collect();

    // Let the crate see the interface come up and join the group before
    // anything is sent: poll until its watcher has had a moment.
    let settle = Instant::now() + Duration::from_millis(500);
    while Instant::now() < settle {
        poll_briefly(&mut behaviour).await;
    }

    println!(
        "libp2p-mdns 0.49.0 as released; announcer TTL {TTL_SECS}s; {PEERS_PER_PACKET} peers per packet; iface {iface}"
    );
    println!("target  stored  wall_ms  poll_ms  poll_us_per_new  rss_kib");
    let mut sent = 0_usize;
    let mut poll_total = Duration::ZERO;
    let start_rss = resident_kib();
    println!("0       0       0        0        -                {start_rss}");
    for target in targets {
        let stage = Instant::now();
        let stored_before = behaviour.discovered_nodes().len();
        let poll_before = poll_total;
        while sent < target {
            let end = (sent + PEERS_PER_PACKET).min(target);
            sender
                .send_to(&announcement(&peers[sent..end]), SocketAddrV4::new(MDNS_GROUP, 5353))
                .expect("send");
            sent = end;
            // Drain as we go, so the kernel's receive buffer is not what
            // decides how many arrive.
            poll_total += poll_briefly(&mut behaviour).await;
        }
        while behaviour.discovered_nodes().len() < target && stage.elapsed() < STAGE_TIMEOUT {
            poll_total += poll_briefly(&mut behaviour).await;
        }
        let stored = behaviour.discovered_nodes().len();
        let poll_ms = (poll_total - poll_before).as_secs_f64() * 1e3;
        let new = stored.saturating_sub(stored_before).max(1);
        println!(
            "{target:<7} {stored:<7} {:<8.0} {poll_ms:<8.1} {:<16.2} {}",
            stage.elapsed().as_secs_f64() * 1e3,
            poll_ms * 1e3 / new as f64,
            resident_kib()
        );
        if stored < target {
            println!("STALLED at {stored} of {target} after {STAGE_TIMEOUT:?}");
            break;
        }
    }
}

/// Poll the behaviour until it is pending, for at most a millisecond of
/// wall time; return the time spent inside `poll`.
async fn poll_briefly(behaviour: &mut mdns::tokio::Behaviour) -> Duration {
    let mut inside = Duration::ZERO;
    let _ = tokio::time::timeout(
        Duration::from_millis(1),
        poll_fn(|cx| loop {
            let t = Instant::now();
            let r = behaviour.poll(cx);
            inside += t.elapsed();
            if r.is_pending() {
                return Poll::<()>::Pending;
            }
        }),
    )
    .await;
    inside
}
