// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! ADR-0053's bounds on the vendored mDNS crate, asserted on its real
//! packet path.
//!
//! # Why every test re-runs itself inside a network namespace
//!
//! The crate skips loopback interfaces, and on the development host the
//! shared interface does not loop multicast back (measured 2026-09-25),
//! so no packet sent on the host network reaches it. Each test therefore
//! re-executes its own binary inside an unprivileged user network
//! namespace (`unshare -rn`) with one dummy interface carrying multicast
//! -- a domain chosen for the test, SPIKE-010's rule -- and runs its body
//! there. Where user namespaces are unavailable the test FAILS, never
//! skips: a skip is a green check that asserted nothing, and ADR-0053
//! rule 9 says so. On CI, the step that allows them is a workflow step.
//!
//! # What is injected
//!
//! Unsolicited mDNS responses, the shape the crate accepts from any host
//! on the domain (ADR-0053 rule 6 keeps them accepted), and raw queries,
//! both sent from a plain UDP socket on the dummy interface.
//! SPIKE-010's flood row measured the unpatched crate the same way:
//! `spikes/spike-010/harness/REPRODUCTION-2026-09-25.log`.
//!
//! # What is not asserted
//!
//! The send buffer's cap (rule 2, 16 packets) is the backstop behind the
//! once-per-second rule and is not reachable while that rule holds; its
//! drop count is read through the runtime but never forced above zero.

#![allow(clippy::expect_used, clippy::panic)]

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::task::Poll;
use std::time::{Duration, Instant};

use futures::future::poll_fn;
use libp2p::PeerId;
use libp2p::identity::Keypair;
use libp2p::mdns;
use libp2p::swarm::{NetworkBehaviour, ToSwarm};

/// Set inside the namespace to its dummy interface's address.
const NETNS_ENV: &str = "INTERWEAVE_MDNS_NETNS";
const IFACE: &str = "10.99.0.1";
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const PEERS_PER_PACKET: usize = 16;

/// Run the calling test's body inside a fresh namespace. Returns `true`
/// when the caller IS the re-executed copy and should run its body,
/// `false` when it is the outer copy, which has already asserted that
/// the inner one passed.
fn in_namespace(test: &str) -> bool {
    if std::env::var(NETNS_ENV).is_ok() {
        return true;
    }
    let exe = std::env::current_exe().expect("the test binary's path");
    let script = format!(
        "set -e; ip link set lo up; ip link add dummy0 type dummy; \
         ip link set dummy0 multicast on up; ip addr add {IFACE}/16 dev dummy0; \
         ip route add 224.0.0.0/4 dev dummy0; \
         {NETNS_ENV}={IFACE} exec \"$0\" --exact \"$1\" --nocapture --test-threads 1"
    );
    let output = std::process::Command::new("unshare")
        .args(["-rn", "sh", "-c", &script])
        .arg(&exe)
        .arg(test)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "cannot run `unshare -rn` ({e}). This test needs an unprivileged user network \
                 namespace, because no packet on the host network reaches the mDNS crate \
                 (ADR-0053 rule 9); it fails rather than skips where that is unavailable."
            )
        });
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "the test inside the namespace did not pass ({}). If the namespace could not be \
         made, user namespaces are unavailable here and this test cannot ask its question \
         (ADR-0053 rule 9).\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        output.status
    );
    false
}

fn iface() -> Ipv4Addr {
    std::env::var(NETNS_ENV)
        .expect("inside the namespace")
        .parse()
        .expect("an IPv4 address")
}

fn peers(n: usize) -> Vec<PeerId> {
    (0..n)
        .map(|_| Keypair::generate_ed25519().public().to_peer_id())
        .collect()
}

fn append_name(out: &mut Vec<u8>, labels: &[&[u8]]) {
    for label in labels {
        out.push(u8::try_from(label.len()).expect("short label"));
        out.extend_from_slice(label);
    }
    out.push(0);
}

fn append_record(out: &mut Vec<u8>, rtype: u16, ttl: u32, rdata: &[u8]) {
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&1_u16.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&u16::try_from(rdata.len()).expect("fits").to_be_bytes());
    out.extend_from_slice(rdata);
}

/// An unsolicited response announcing each peer at one address, with
/// `ttl` seconds. `first` numbers the peers so every address and name is
/// distinct.
fn announcement(peers: &[PeerId], first: usize, ttl: u32) -> Vec<u8> {
    let count = u16::try_from(peers.len()).expect("few");
    let mut out = Vec::with_capacity(4096);
    for field in [0, 0x8400, 0, count, 0, count] {
        out.extend_from_slice(&u16::to_be_bytes(field));
    }
    let names: Vec<String> = (first..first + peers.len())
        .map(|n| format!("bound{n}"))
        .collect();
    for name in &names {
        append_name(&mut out, &[b"_p2p", b"_udp", b"local"]);
        let mut target = Vec::new();
        append_name(&mut target, &[name.as_bytes(), b"local"]);
        append_record(&mut out, 12, ttl, &target);
    }
    for (i, (peer, name)) in peers.iter().zip(&names).enumerate() {
        let n = first + i;
        append_name(&mut out, &[name.as_bytes(), b"local"]);
        let value = format!(
            "dnsaddr=/ip4/10.99.{}.{}/tcp/4001/p2p/{peer}",
            (n / 250) % 250 + 1,
            n % 250 + 1
        );
        let mut rdata = vec![u8::try_from(value.len()).expect("short")];
        rdata.extend_from_slice(value.as_bytes());
        append_record(&mut out, 16, ttl, &rdata);
    }
    out
}

/// A query for `_p2p._udp.local`, the shape the crate answers.
fn query() -> Vec<u8> {
    let mut out = Vec::new();
    for field in [7_u16, 0, 1, 0, 0, 0] {
        out.extend_from_slice(&field.to_be_bytes());
    }
    append_name(&mut out, &[b"_p2p", b"_udp", b"local"]);
    out.extend_from_slice(&12_u16.to_be_bytes()); // PTR
    out.extend_from_slice(&1_u16.to_be_bytes());
    out
}

struct Flood {
    socket: UdpSocket,
}

impl Flood {
    fn new() -> Self {
        let socket = UdpSocket::bind(SocketAddrV4::new(iface(), 0)).expect("bind the flooder");
        socket.set_multicast_loop_v4(true).expect("loop");
        Self { socket }
    }

    fn send(&self, packet: &[u8]) {
        self.socket
            .send_to(packet, SocketAddrV4::new(GROUP, 5353))
            .expect("send to the group");
    }
}

fn behaviour() -> mdns::tokio::Behaviour {
    let config = mdns::Config {
        ttl: Duration::from_secs(360),
        // Long, so the node's own periodic queries stay out of the way.
        query_interval: Duration::from_secs(3600),
        enable_ipv6: false,
    };
    mdns::tokio::Behaviour::new(config, Keypair::generate_ed25519().public().to_peer_id())
        .expect("the interface watcher")
}

/// Poll `behaviour` until it is pending, for at most `budget`; count the
/// pairs it reported expired.
async fn drain(
    behaviour: &mut mdns::tokio::Behaviour,
    budget: Duration,
) -> (usize, Vec<mdns::Event>) {
    let mut events = Vec::new();
    let _ = tokio::time::timeout(
        budget,
        poll_fn(|cx| {
            loop {
                match behaviour.poll(cx) {
                    Poll::Ready(ToSwarm::GenerateEvent(event)) => events.push(event),
                    Poll::Ready(_) => {}
                    Poll::Pending => return Poll::<()>::Pending,
                }
            }
        }),
    )
    .await;
    let expired = events
        .iter()
        .map(|e| match e {
            mdns::Event::Expired(pairs) => pairs.len(),
            _ => 0,
        })
        .sum();
    (expired, events)
}

/// Let the crate see the dummy interface come up and join the group.
async fn settle(behaviour: &mut mdns::tokio::Behaviour) {
    let _ = drain(behaviour, Duration::from_millis(500)).await;
}

/// A consumer's view of the event stream, replayed in order: how many
/// records it would hold, and the most it ever held. The provider is such
/// a consumer at the same capacity, so the most must never exceed the
/// cap -- which holds only if an eviction's `Expired` arrives BEFORE the
/// `Discovered` that caused it (#112, the automated review's P1).
#[derive(Default)]
struct Replay {
    live: usize,
    most: usize,
    expired: usize,
}

impl Replay {
    fn apply(&mut self, events: &[mdns::Event]) {
        for event in events {
            match event {
                mdns::Event::Discovered(pairs) => {
                    self.live += pairs.len();
                    self.most = self.most.max(self.live);
                }
                mdns::Event::Expired(pairs) => {
                    self.live -= pairs.len();
                    self.expired += pairs.len();
                }
                mdns::Event::InterfaceFailed { .. } => {}
            }
        }
    }
}

/// Announce `peers` in packets of sixteen, draining as it goes so the
/// kernel's receive buffer is not what decides how many arrive; every
/// event is replayed into `replay`, in order. Returns the pairs reported
/// expired meanwhile.
async fn announce(
    behaviour: &mut mdns::tokio::Behaviour,
    flood: &Flood,
    peers: &[PeerId],
    first: usize,
    ttl: u32,
    replay: &mut Replay,
) -> usize {
    let before = replay.expired;
    for (i, chunk) in peers.chunks(PEERS_PER_PACKET).enumerate() {
        flood.send(&announcement(chunk, first + i * PEERS_PER_PACKET, ttl));
        replay.apply(&drain(behaviour, Duration::from_millis(2)).await.1);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let (_, events) = drain(behaviour, Duration::from_millis(50)).await;
        let quiet = events.is_empty();
        replay.apply(&events);
        if quiet {
            let (_, events) = drain(behaviour, Duration::from_millis(100)).await;
            let quiet = events.is_empty();
            replay.apply(&events);
            if quiet {
                break;
            }
        }
    }
    replay.expired - before
}

/// Rule 2's record cap, with the eviction reported. Twice the cap is
/// announced; the store stops at the cap, and every eviction is
/// reported as expired so the provider retracts what the crate no longer
/// holds. THE CONTROL is the cap itself announced first: nothing is
/// evicted until the store is full.
#[test]
fn the_record_store_stops_at_its_cap_and_reports_what_it_evicts() {
    if !in_namespace("the_record_store_stops_at_its_cap_and_reports_what_it_evicts") {
        return;
    }
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            let cap = mdns::MAX_DISCOVERED_RECORDS;
            let mut behaviour = behaviour();
            let counts = behaviour.drop_counts();
            let flood = Flood::new();
            settle(&mut behaviour).await;
            let all = peers(cap * 2);
            let mut replay = Replay::default();

            let expired = announce(&mut behaviour, &flood, &all[..cap], 0, 3600, &mut replay).await;
            assert_eq!(
                behaviour.discovered_nodes().len(),
                cap,
                "the control: the cap fits"
            );
            assert_eq!(
                (expired, counts.records_evicted()),
                (0, 0),
                "nothing evicted below it"
            );

            let expired =
                announce(&mut behaviour, &flood, &all[cap..], cap, 3600, &mut replay).await;
            let delivered = cap - usize::try_from(counts.discovered_dropped()).expect("fits");
            assert_eq!(
                behaviour.discovered_nodes().len(),
                cap,
                "the store stops at its cap"
            );
            let evicted = usize::try_from(counts.records_evicted()).expect("fits");
            let refused = usize::try_from(counts.records_refused()).expect("fits");
            assert_eq!(
                evicted + refused,
                delivered,
                "every record past the cap is counted"
            );
            assert_eq!(
                expired, evicted,
                "and every eviction is reported as expired"
            );
            assert_eq!(
                replay.most, cap,
                "and a consumer replaying the events in order never holds more than the cap: \
                 the room is reported before the record that takes it (#112)"
            );
        });
}

/// Rule 3's TTL clamp. A store full of records announced with a one-hour
/// TTL is never the soonest to expire without the clamp, so a
/// legitimate record announced after them -- with the provider's own
/// 120 s -- would be the one refused. With the clamp the flood expires
/// no later than the legitimate record, and it gets in.
#[test]
fn a_long_announced_ttl_does_not_keep_a_legitimate_record_out() {
    if !in_namespace("a_long_announced_ttl_does_not_keep_a_legitimate_record_out") {
        return;
    }
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            let cap = mdns::MAX_DISCOVERED_RECORDS;
            let mut behaviour = behaviour();
            let counts = behaviour.drop_counts();
            let flood = Flood::new();
            settle(&mut behaviour).await;
            let all = peers(cap + 1);
            let mut replay = Replay::default();
            let _ = announce(&mut behaviour, &flood, &all[..cap], 0, 3600, &mut replay).await;
            assert_eq!(behaviour.discovered_nodes().len(), cap);

            let legitimate = all[cap];
            let _ = announce(&mut behaviour, &flood, &[legitimate], cap, 120, &mut replay).await;
            assert!(
                behaviour.discovered_nodes().any(|p| *p == legitimate),
                "the legitimate record is admitted; refused {}",
                counts.records_refused()
            );
            assert_eq!(counts.records_refused(), 0);
            assert_eq!(counts.records_evicted(), 1, "one flood record made room");
        });
}

/// Rule 4. A burst of ten queries inside a second is answered once; the
/// rest are counted. THE CONTROL is that the node does answer: a
/// response from it is seen on the group, so the count is the rule and
/// not a node that never answers.
#[test]
fn an_interface_answers_at_most_once_a_second() {
    if !in_namespace("an_interface_answers_at_most_once_a_second") {
        return;
    }
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            let mut behaviour = behaviour();
            let counts = behaviour.drop_counts();
            let flood = Flood::new();
            // An observer on the group, to see the node's answers.
            let observer = {
                let socket = socket2::Socket::new(
                    socket2::Domain::IPV4,
                    socket2::Type::DGRAM,
                    Some(socket2::Protocol::UDP),
                )
                .expect("socket");
                socket.set_reuse_address(true).expect("reuse");
                socket.set_reuse_port(true).expect("reuse port");
                socket
                    .bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 5353).into())
                    .expect("bind 5353");
                socket
                    .join_multicast_v4(&GROUP, &iface())
                    .expect("join the group");
                socket
                    .set_read_timeout(Some(Duration::from_millis(20)))
                    .expect("timeout");
                UdpSocket::from(socket)
            };
            settle(&mut behaviour).await;
            // Let the node's own probe queries, answered by itself, fall out
            // of the window before the burst.
            let _ = drain(&mut behaviour, Duration::from_millis(1200)).await;
            let before = counts.queries_unanswered();
            let answered_at = Instant::now();
            for _ in 0..10 {
                flood.send(&query());
            }
            let _ = drain(&mut behaviour, Duration::from_millis(300)).await;
            assert!(
                answered_at.elapsed() < Duration::from_secs(1),
                "the burst fits inside one second"
            );
            let unanswered = counts.queries_unanswered() - before;
            assert!(
                unanswered >= 9,
                "nine of ten queries unanswered, got {unanswered}"
            );

            let mut buf = [0_u8; 4096];
            let mut responses = 0;
            while let Ok((len, _)) = observer.recv_from(&mut buf) {
                // A response: the QR bit set in the flags.
                if len > 3 && buf[2] & 0x80 != 0 {
                    responses += 1;
                }
            }
            assert!(responses >= 1, "the control: the node did answer");
        });
}

/// Rule 2's per-interface queue. While the behaviour is not polled, the
/// interface keeps at most `MAX_INTERFACE_DISCOVERED` pairs beyond the
/// behaviour's channel and counts the rest; what it kept arrives when
/// polling resumes.
#[test]
fn an_unpolled_interface_keeps_a_bounded_queue() {
    if !in_namespace("an_unpolled_interface_keeps_a_bounded_queue") {
        return;
    }
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            let mut behaviour = behaviour();
            let counts = behaviour.drop_counts();
            let flood = Flood::new();
            settle(&mut behaviour).await;
            let all = peers(320);
            for (i, chunk) in all.chunks(PEERS_PER_PACKET).enumerate() {
                flood.send(&announcement(chunk, i * PEERS_PER_PACKET, 3600));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            let dropped = usize::try_from(counts.discovered_dropped()).expect("fits");
            assert!(dropped > 0, "an unpolled interface drops past its queue");
            let _ = drain(&mut behaviour, Duration::from_secs(1)).await;
            assert_eq!(
                behaviour.discovered_nodes().len(),
                all.len() - dropped,
                "and everything it kept is delivered"
            );
            assert!(
                all.len() - dropped <= mdns::MAX_INTERFACE_DISCOVERED + 16,
                "what it kept is its queue plus the channel's few"
            );
        });
}

/// Rule 5, through the runtime. An interface whose bind fails -- port
/// 5353 held without address reuse -- reaches the consumer as
/// `SwarmEvent::MdnsInterfaceFailed` naming this node's own interface,
/// where the crate used to log it and move on. And rule 7: the crate's
/// drop counts are readable through the runtime handle.
#[test]
fn a_failed_interface_reaches_the_consumer_and_the_counts_are_readable() {
    if !in_namespace("a_failed_interface_reaches_the_consumer_and_the_counts_are_readable") {
        return;
    }
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            use interweave_transport_libp2p::runtime::mdns_driver::{MdnsDropCounts, MdnsSettings};
            use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};

            // Held WITHOUT SO_REUSEADDR, so the crate's bind fails.
            let _holder = UdpSocket::bind("0.0.0.0:5353").expect("hold 5353");
            let identity = interweave_profile_identity::ProfileIdentity::generate();
            let mut runtime = SwarmRuntime::start(
                &identity,
                SubstrateConfig {
                    mdns: Some(MdnsSettings::default()),
                    ..SubstrateConfig::default()
                },
                interweave_transport_runtime::TrustSources::new(
                    interweave_trust_api::PeerTrustPolicy::new(std::iter::empty()).expect("empty"),
                    interweave_trust_api::InfrastructureSet::default(),
                ),
            )
            .expect("the node starts");
            assert_eq!(
                runtime.mdns_drop_counts(),
                Some(MdnsDropCounts::default()),
                "the counts are readable, and start at zero"
            );
            let expected: std::net::IpAddr = IFACE.parse().expect("ip");
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match tokio::time::timeout(remaining, runtime.next_event()).await {
                    Ok(Some(SwarmEvent::MdnsInterfaceFailed { address, .. })) => {
                        assert_eq!(address, expected, "this node's own interface");
                        break;
                    }
                    Ok(Some(_)) => {}
                    other => panic!("no MdnsInterfaceFailed before the deadline: {other:?}"),
                }
            }
            runtime.shutdown().await.expect("clean shutdown");
        });
}
