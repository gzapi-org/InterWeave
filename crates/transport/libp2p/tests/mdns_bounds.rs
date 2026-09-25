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
//! - The send buffer's cap (rule 2, 16 packets) is the backstop behind
//!   the once-per-second rule and is not reachable while that rule holds;
//!   its drop count is read through the runtime but never forced above
//!   zero. ADR-0053 rule 9 records it as asserted present, not exercised:
//!   the `const` assertion below, a build failure on drift.
//! - The receive-error report (rule 5) has no test: nothing here makes a
//!   receive fail on a bound UDP socket.
//! - `WatcherFailed` (rule 5) is tested at the crate only for a DEAD
//!   watcher, supplied through the `Provider` seam
//!   (`a_dead_interface_watcher_is_stopped_and_reported_once`): the real
//!   watcher's error cannot be produced here. The re-arm after a watcher
//!   that recovers is asserted by reading; the driver's hold of it is a
//!   unit test.

#![allow(clippy::expect_used, clippy::panic)]

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::task::Poll;
use std::time::{Duration, Instant};

use futures::future::poll_fn;
use libp2p::PeerId;
use libp2p::identity::Keypair;
use libp2p::mdns;
use libp2p::swarm::{NetworkBehaviour, ToSwarm};

/// ADR-0053 rule 9: the send buffer's cap, asserted PRESENT and at the
/// value `resource-limits.md` states. It cannot be exercised while rule
/// 4's once-a-second answer holds (above), so without this a changed
/// value would leave every test green (#112 blind review N8). It reads
/// the constant only: that `queue_packet` still enforces it is checked by
/// reading the vendored patch, not by any test.
const _: () = assert!(mdns::MAX_INTERFACE_SEND_PACKETS == 16);

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

/// One peer in an announcement: its id, the TTL its PTR record carries
/// (which the crate takes as the peer's), and how many addresses it names.
struct Entry {
    peer: PeerId,
    ttl: u32,
    addresses: u16,
}

/// An unsolicited response announcing each entry, the shape the crate
/// accepts from any host on the domain. `first` numbers the entries so
/// every name is distinct. Addresses differ by PORT: the crate rewrites
/// an announced address's IP to the packet's observed source
/// (`_address_translation`), so only the port keeps two addresses of one
/// peer apart.
fn packet(entries: &[Entry], first: usize) -> Vec<u8> {
    let answers = u16::try_from(entries.len()).expect("few");
    let additionals: u16 = entries.iter().map(|e| e.addresses).sum();
    let mut out = Vec::with_capacity(4096);
    for field in [0, 0x8400, 0, answers, 0, additionals] {
        out.extend_from_slice(&u16::to_be_bytes(field));
    }
    let names: Vec<String> = (first..first + entries.len())
        .map(|n| format!("bound{n}"))
        .collect();
    for (entry, name) in entries.iter().zip(&names) {
        append_name(&mut out, &[b"_p2p", b"_udp", b"local"]);
        let mut target = Vec::new();
        append_name(&mut target, &[name.as_bytes(), b"local"]);
        append_record(&mut out, 12, entry.ttl, &target);
    }
    for (entry, name) in entries.iter().zip(&names) {
        for port in 0..entry.addresses {
            append_name(&mut out, &[name.as_bytes(), b"local"]);
            let value = format!(
                "dnsaddr=/ip4/10.99.1.1/tcp/{}/p2p/{}",
                4001 + port,
                entry.peer
            );
            let mut rdata = vec![u8::try_from(value.len()).expect("short")];
            rdata.extend_from_slice(value.as_bytes());
            append_record(&mut out, 16, entry.ttl, &rdata);
        }
    }
    out
}

/// Each peer at one address, all with `ttl`.
fn announcement(peers: &[PeerId], first: usize, ttl: u32) -> Vec<u8> {
    let entries: Vec<Entry> = peers
        .iter()
        .map(|peer| Entry {
            peer: *peer,
            ttl,
            addresses: 1,
        })
        .collect();
    packet(&entries, first)
}

/// The id this test's queries carry; the crate echoes it in its answer.
const QUERY_ID: u16 = 7;

/// The id this test's service-discovery queries carry.
const META_QUERY_ID: u16 = 8;

/// A service-discovery query (`_services._dns-sd._udp.local`), which the
/// crate answers with its service record.
fn meta_query() -> Vec<u8> {
    let mut out = Vec::new();
    for field in [META_QUERY_ID, 0, 1, 0, 0, 0] {
        out.extend_from_slice(&field.to_be_bytes());
    }
    append_name(&mut out, &[b"_services", b"_dns-sd", b"_udp", b"local"]);
    out.extend_from_slice(&12_u16.to_be_bytes()); // PTR
    out.extend_from_slice(&1_u16.to_be_bytes());
    out
}

/// A query for `_p2p._udp.local`, the shape the crate answers.
fn query() -> Vec<u8> {
    let mut out = Vec::new();
    for field in [QUERY_ID, 0, 1, 0, 0, 0] {
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

/// A consumer's view of the event stream, replayed in order, by the
/// exact pairs reported: what it would hold, and the most it ever held.
/// The provider is such a consumer, with the store's shape (ADR-0053 rule
/// 2), so after any sequence of batches it must hold exactly what the
/// store holds and never more than the store's bound. A retraction of a
/// pair it does not hold is a no-op, as in the provider.
#[derive(Default)]
struct Replay {
    held: std::collections::HashSet<(PeerId, libp2p::Multiaddr)>,
    most: usize,
    expired: usize,
    /// Every peer ever named in a `Discovered`.
    ever_discovered: std::collections::HashSet<PeerId>,
    /// Pairs named more than once within one event. `held` is a set, so
    /// a duplicate is invisible to every comparison of it; a consumer
    /// that counts rather than sets would be off by one (#112 blind
    /// review, P3 5 on 9f56dd83).
    duplicates: usize,
}

impl Replay {
    fn apply(&mut self, events: &[mdns::Event]) {
        for event in events {
            if let mdns::Event::Discovered(pairs) | mdns::Event::Expired(pairs) = event {
                let distinct: std::collections::HashSet<_> = pairs.iter().collect();
                self.duplicates += pairs.len() - distinct.len();
            }
            match event {
                mdns::Event::Discovered(pairs) => {
                    self.held.extend(pairs.iter().cloned());
                    self.ever_discovered
                        .extend(pairs.iter().map(|(peer, _)| *peer));
                    self.most = self.most.max(self.held.len());
                }
                mdns::Event::Expired(pairs) => {
                    for pair in pairs {
                        self.held.remove(pair);
                    }
                    self.expired += pairs.len();
                }
                mdns::Event::InterfaceFailed { .. } | mdns::Event::WatcherFailed { .. } => {}
            }
        }
    }

    /// The pairs the consumer holds, sorted.
    fn pairs(&self) -> Vec<(PeerId, String)> {
        let mut pairs: Vec<(PeerId, String)> = self
            .held
            .iter()
            .map(|(peer, address)| (*peer, address.to_string()))
            .collect();
        pairs.sort();
        pairs
    }
}

/// The peers the store holds, one entry per record, sorted.
fn stored(behaviour: &mdns::tokio::Behaviour) -> Vec<PeerId> {
    let mut peers: Vec<PeerId> = behaviour.discovered_nodes().copied().collect();
    peers.sort();
    peers
}

/// The PAIRS the store holds, sorted -- read through the crate's public
/// pending-dial hook, which answers with every address it holds for a
/// peer. Comparing pairs rather than peers is what "exactly what the
/// store holds" means: a consumer holding a different address of the
/// same peer is not in step (#112 re-review N9).
fn stored_pairs(behaviour: &mut mdns::tokio::Behaviour) -> Vec<(PeerId, String)> {
    let mut peers: Vec<PeerId> = behaviour.discovered_nodes().copied().collect();
    peers.sort();
    peers.dedup();
    let mut pairs = Vec::new();
    for peer in peers {
        let addresses = behaviour
            .handle_pending_outbound_connection(
                libp2p::swarm::ConnectionId::new_unchecked(0),
                Some(peer),
                &[],
                libp2p::core::Endpoint::Dialer,
            )
            .expect("the crate never denies");
        pairs.extend(addresses.into_iter().map(|a| (peer, a.to_string())));
    }
    pairs.sort();
    pairs
}

/// A runtime on ONE thread, for the tests that need a packet's pairs to
/// land in one drain: the interface task then runs to its own `Pending`,
/// queuing every pair of the packet, before the behaviour is polled
/// again, so "one batch" is certain rather than likely.
fn one_thread() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// Drain until two consecutive windows are quiet, replaying as it goes.
async fn quiesce(behaviour: &mut mdns::tokio::Behaviour, replay: &mut Replay) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut quiet = 0;
    while quiet < 2 && Instant::now() < deadline {
        let (_, events) = drain(behaviour, Duration::from_millis(60)).await;
        quiet = if events.is_empty() { quiet + 1 } else { 0 };
        replay.apply(&events);
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
    quiesce(behaviour, replay).await;
    replay.expired - before
}

/// Announce `peers` until the store holds every one of them, re-sending
/// what an interface queue dropped under load: a FILL is the setup of a
/// test, and the setup is not what the test asks about. An earlier fill
/// sent once and asserted the result, which read 2027 of 2048 twice in
/// eighty parallel runs (#112 re-review).
async fn fill(
    behaviour: &mut mdns::tokio::Behaviour,
    flood: &Flood,
    peers: &[PeerId],
    ttl: u32,
    replay: &mut Replay,
) {
    let _ = announce(behaviour, flood, peers, 0, ttl, replay).await;
    for _ in 0..5 {
        let held: std::collections::HashSet<PeerId> =
            behaviour.discovered_nodes().copied().collect();
        let missing: Vec<(usize, PeerId)> = peers
            .iter()
            .enumerate()
            .filter(|(_, p)| !held.contains(p))
            .map(|(i, p)| (i, *p))
            .collect();
        if missing.is_empty() {
            return;
        }
        for (i, peer) in missing {
            flood.send(&announcement(&[peer], i, ttl));
            replay.apply(&drain(behaviour, Duration::from_millis(1)).await.1);
        }
        quiesce(behaviour, replay).await;
    }
    panic!("the fill never completed: the store holds fewer than it was given");
}

/// Rule 2's PEER bound, with the eviction reported. Twice the bound in
/// distinct single-address peers is announced; the store stops at the
/// provider's peer bound -- not at a count of records, which is what let
/// the store outgrow the provider before (#112's third review) -- every
/// eviction is reported as expired, and a consumer replaying the events
/// in order holds exactly what the store holds and never more than the
/// bound. THE CONTROL is the bound itself filled first: nothing is evicted
/// until the store is full.
#[test]
fn the_record_store_stops_at_the_providers_peer_bound_and_reports_what_it_evicts() {
    if !in_namespace(
        "the_record_store_stops_at_the_providers_peer_bound_and_reports_what_it_evicts",
    ) {
        return;
    }
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            let cap = mdns::MAX_DISCOVERED_PEERS;
            let mut behaviour = behaviour();
            let counts = behaviour.drop_counts();
            let flood = Flood::new();
            settle(&mut behaviour).await;
            let all = peers(cap * 2);
            let mut replay = Replay::default();

            fill(&mut behaviour, &flood, &all[..cap], 3600, &mut replay).await;
            assert_eq!(stored(&behaviour).len(), cap, "the control: the bound fits");
            assert_eq!(
                (replay.expired, counts.records_evicted()),
                (0, 0),
                "nothing evicted below it"
            );

            let expired =
                announce(&mut behaviour, &flood, &all[cap..], cap, 3600, &mut replay).await;
            let delivered = cap - usize::try_from(counts.discovered_dropped()).expect("fits");
            assert_eq!(
                stored(&behaviour).len(),
                cap,
                "the store stops at the peer bound"
            );
            let evicted = usize::try_from(counts.records_evicted()).expect("fits");
            let refused = usize::try_from(counts.records_refused()).expect("fits");
            assert_eq!(
                evicted + refused,
                delivered,
                "every peer past the bound is counted"
            );
            assert_eq!(
                expired, evicted,
                "and every eviction is reported as expired"
            );
            assert_eq!(
                replay.most, cap,
                "a consumer replaying the events never holds more than the bound: the room is \
                 reported before the record that takes it (#112)"
            );
            assert_eq!(
                replay.pairs(),
                stored_pairs(&mut behaviour),
                "and ends holding exactly the pairs the store holds"
            );
        });
}

/// Rule 2's ADDRESS bound: one peer announcing twenty addresses keeps
/// the provider's eight, and the rest are counted. The per-peer bound is
/// what a count-only cap missed in the other direction: one peer could
/// fill the whole store.
#[test]
fn one_peer_keeps_at_most_the_providers_addresses() {
    if !in_namespace("one_peer_keeps_at_most_the_providers_addresses") {
        return;
    }
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            let bound = mdns::MAX_ADDRESSES_PER_DISCOVERED_PEER;
            let mut behaviour = behaviour();
            let counts = behaviour.drop_counts();
            let flood = Flood::new();
            settle(&mut behaviour).await;
            let peer = peers(1)[0];
            let mut replay = Replay::default();
            let many = u16::try_from(bound + 12).expect("few");
            flood.send(&packet(
                &[Entry {
                    peer,
                    ttl: 3600,
                    addresses: many,
                }],
                0,
            ));
            quiesce(&mut behaviour, &mut replay).await;
            assert_eq!(
                stored(&behaviour).len(),
                bound,
                "the peer keeps the provider's bound"
            );
            assert_eq!(
                counts.records_evicted() + counts.records_refused(),
                12,
                "and the twelve past it are counted"
            );
            assert_eq!(
                replay.pairs(),
                stored_pairs(&mut behaviour),
                "the consumer agrees"
            );
        });
}

/// #112 re-review N1: a pair added and evicted within ONE drained batch
/// is reported as neither. The store is full with one record due to go
/// soonest; one packet then brings two new peers, the first announcing a
/// shorter TTL than the second, so the second evicts the first within
/// the same batch. A consumer replaying the events must end holding
/// exactly what the store holds; reported unnetted, it keeps the first
/// peer, which the store evicted and will never retract.
#[test]
fn a_record_added_and_evicted_in_one_batch_is_reported_as_neither() {
    if !in_namespace("a_record_added_and_evicted_in_one_batch_is_reported_as_neither") {
        return;
    }
    one_thread().block_on(async {
        let cap = mdns::MAX_DISCOVERED_PEERS;
        let mut behaviour = behaviour();
        let counts = behaviour.drop_counts();
        let flood = Flood::new();
        settle(&mut behaviour).await;
        let all = peers(cap + 2);
        let mut replay = Replay::default();
        // cap - 1 peers at the clamped TTL, and one planted to go
        // first.
        fill(&mut behaviour, &flood, &all[..cap - 1], 3600, &mut replay).await;
        let planted = all[cap - 1];
        flood.send(&announcement(&[planted], cap - 1, 10));
        quiesce(&mut behaviour, &mut replay).await;
        assert_eq!(
            stored(&behaviour).len(),
            cap,
            "full, with one due to go first"
        );

        let (first, second) = (all[cap], all[cap + 1]);
        flood.send(&packet(
            &[
                Entry {
                    peer: first,
                    ttl: 60,
                    addresses: 1,
                },
                Entry {
                    peer: second,
                    ttl: 90,
                    addresses: 1,
                },
            ],
            cap,
        ));
        quiesce(&mut behaviour, &mut replay).await;
        let held = stored(&behaviour);
        assert!(held.contains(&second) && !held.contains(&first) && !held.contains(&planted));
        assert_eq!(
            counts.records_evicted(),
            2,
            "the planted peer, then the first"
        );
        assert!(
            !replay.ever_discovered.contains(&first),
            "one batch: the first peer was never reported at all"
        );
        assert_eq!(
            replay.pairs(),
            stored_pairs(&mut behaviour),
            "the consumer holds exactly what the store holds: the first peer was never \
                 reported, rather than reported and then retracted out of order"
        );
    });
}

/// #112's fourth review (the automated review's P1, the blind review's
/// N5): a pair that changes state THREE times in one batch. One response
/// repeating a peer across its PTR records does it. Netting by set
/// membership, which dropped every pair seen on both sides, reported
/// such a pair as neither, so the consumer MISSED a record the store held
/// or KEPT one it had evicted. Both cases, each on a fresh behaviour; in
/// each the consumer must end holding exactly the pairs the store holds.
#[test]
fn a_pair_with_three_transitions_in_one_batch_ends_as_the_store_holds_it() {
    if !in_namespace("a_pair_with_three_transitions_in_one_batch_ends_as_the_store_holds_it") {
        return;
    }
    one_thread().block_on(async {
        let cap = mdns::MAX_DISCOVERED_PEERS;
        let one = |peer: PeerId, ttl: u32| Entry {
            peer,
            ttl,
            addresses: 1,
        };
        for case in ["missing", "phantom"] {
            let mut behaviour = behaviour();
            let flood = Flood::new();
            settle(&mut behaviour).await;
            let all = peers(cap + 2);
            let mut replay = Replay::default();
            fill(&mut behaviour, &flood, &all[..cap - 1], 3600, &mut replay).await;
            let planted = all[cap - 1];
            flood.send(&announcement(&[planted], cap - 1, 10));
            quiesce(&mut behaviour, &mut replay).await;
            let (f, s) = (all[cap], all[cap + 1]);
            // MISSING: F is added (evicting the planted peer), evicted by S,
            // then added again, evicting S -- held at the end, first an add.
            // PHANTOM: the planted peer is evicted by F, added again with a
            // longer TTL, evicted again by S -- absent at the end, first an
            // eviction.
            let entries = if case == "missing" {
                [one(f, 60), one(s, 90), one(f, 100)]
            } else {
                [one(f, 60), one(planted, 70), one(s, 80)]
            };
            flood.send(&packet(&entries, cap));
            quiesce(&mut behaviour, &mut replay).await;
            // ONE BATCH, this test's own control, and a partial one: the
            // pair added and evicted inside the packet was never reported.
            // Four of the ways to split the packet across drains would
            // report it and are caught here. Two are not -- "missing" as
            // [F] then [S, F], "phantom" as [F, planted] then [S] -- they
            // report exactly what one batch does, and set netting would
            // pass them too; for those, one batch rests on `one_thread()`
            // alone (#112 blind review, P3 2 on 05f86ce0).
            let transient = if case == "missing" { s } else { f };
            assert!(
                !replay.ever_discovered.contains(&transient),
                "{case}: one batch -- the transient peer was never reported"
            );
            assert_eq!(replay.duplicates, 0, "{case}: no event names a pair twice");
            assert_eq!(
                replay.pairs(),
                stored_pairs(&mut behaviour),
                "{case}: the consumer holds exactly the pairs the store holds"
            );
        }
    });
}

/// Rule 3's TTL clamp. A store full of peers announced with a one-hour
/// TTL is never the soonest to go without the clamp, so a legitimate
/// peer announced after them -- with the provider's own 120 s -- would be
/// the one refused. With the clamp the flood goes no later than the
/// legitimate peer, and it gets in.
#[test]
fn a_long_announced_ttl_does_not_keep_a_legitimate_record_out() {
    if !in_namespace("a_long_announced_ttl_does_not_keep_a_legitimate_record_out") {
        return;
    }
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            let cap = mdns::MAX_DISCOVERED_PEERS;
            let mut behaviour = behaviour();
            let counts = behaviour.drop_counts();
            let flood = Flood::new();
            settle(&mut behaviour).await;
            let all = peers(cap + 1);
            let mut replay = Replay::default();
            fill(&mut behaviour, &flood, &all[..cap], 3600, &mut replay).await;
            assert_eq!(stored(&behaviour).len(), cap);

            let legitimate = all[cap];
            let _ = announce(&mut behaviour, &flood, &[legitimate], cap, 120, &mut replay).await;
            assert!(
                stored(&behaviour).contains(&legitimate),
                "the legitimate peer is admitted; refused {}",
                counts.records_refused()
            );
            assert_eq!(counts.records_refused(), 0);
            assert_eq!(counts.records_evicted(), 1, "one flood peer made room");
        });
}

/// Rule 4. A burst of ten queries inside a second is answered exactly
/// once, and nine are counted unanswered. THE CONTROL is that one answer,
/// to THIS test's query id, seen on the group: the count is the rule, not
/// a node that never answers.
///
/// THE BURST IS TIMED OFF THE NODE'S OWN PROBES. The crate sends a query
/// when an interface comes up and again a second later (its probe
/// interval doubles from 500 ms, reset to 1 s, then 2 s), and the
/// multicast loop hands each back to its own receive socket, which
/// answers it. An earlier version fired its burst 1.2 s after settling --
/// inside the window of the second self-answer -- so all ten were
/// refused, and its control was met by the self-answers (#112 blind
/// review F1). So the test waits for the second self-answer, fires 1.1 s
/// after it and before the next probe, and counts only its own id.
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
            // Responses on the group: (arrival, query id).
            let mut buf = [0_u8; 4096];
            let mut responses = |observer: &UdpSocket| {
                let mut seen = Vec::new();
                while let Ok((len, _)) = observer.recv_from(&mut buf) {
                    // A response: the QR bit set in the flags.
                    if len > 3 && buf[2] & 0x80 != 0 {
                        seen.push((Instant::now(), u16::from_be_bytes([buf[0], buf[1]])));
                    }
                }
                seen
            };
            let mut self_answers = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(5);
            while self_answers.len() < 2 && Instant::now() < deadline {
                let _ = drain(&mut behaviour, Duration::from_millis(10)).await;
                self_answers.extend(
                    responses(&observer)
                        .into_iter()
                        .filter(|(_, id)| *id != QUERY_ID)
                        .map(|(at, _)| at),
                );
            }
            let last = *self_answers
                .get(1)
                .expect("the node answered its own two probes within 5 s");
            let fire_at = last + Duration::from_millis(1100);
            while Instant::now() < fire_at {
                let _ = drain(&mut behaviour, Duration::from_millis(10)).await;
            }
            let _ = responses(&observer);

            let before = counts.queries_unanswered();
            let burst = Instant::now();
            for _ in 0..10 {
                flood.send(&query());
            }
            let _ = drain(&mut behaviour, Duration::from_millis(300)).await;
            let ours = responses(&observer)
                .into_iter()
                .filter(|(_, id)| *id == QUERY_ID)
                .count();
            assert!(
                burst.duration_since(last) < Duration::from_millis(1900),
                "the burst came before the node's next probe"
            );
            assert_eq!(ours, 1, "the control: exactly one of the ten is answered");
            assert_eq!(
                counts.queries_unanswered() - before,
                9,
                "and the other nine are counted, not answered"
            );

            // THE WINDOW RUNS FROM THE LAST ANSWER, not the last query. A
            // query half a second after the burst is refused; one 1.2 s
            // after the burst is answered, whatever was refused between.
            // A sliding window -- every query, refused or not, restarting
            // the second -- answers neither, and the burst alone cannot
            // tell the two apart.
            let at = |offset_ms: u64| burst + Duration::from_millis(offset_ms);
            while Instant::now() < at(500) {
                let _ = drain(&mut behaviour, Duration::from_millis(10)).await;
            }
            flood.send(&query());
            while Instant::now() < at(1200) {
                let _ = drain(&mut behaviour, Duration::from_millis(10)).await;
            }
            let refused_between = responses(&observer)
                .into_iter()
                .filter(|(_, id)| *id == QUERY_ID)
                .count();
            assert_eq!(refused_between, 0, "the query at +0.5 s is refused");
            flood.send(&query());
            let _ = drain(&mut behaviour, Duration::from_millis(300)).await;
            let answered_after = responses(&observer)
                .into_iter()
                .filter(|(_, id)| *id == QUERY_ID)
                .count();
            assert_eq!(
                answered_after, 1,
                "the query at +1.2 s is answered: the window ran from the answer"
            );

            // ONE SLOT PER ANSWER (ADR-0053 rule 4, RFC 6762 section 6). At
            // +2.4 s the peer slot is free again (its last answer was at
            // +1.2 s). A meta-query sent first must not spend it: the peer
            // query right behind it is answered, and so is the meta-query,
            // each on its own slot. One slot per interface refused the peer
            // query, which let a service browser starve discovery.
            while Instant::now() < at(2400) {
                let _ = drain(&mut behaviour, Duration::from_millis(10)).await;
            }
            let _ = responses(&observer);
            flood.send(&meta_query());
            flood.send(&query());
            let _ = drain(&mut behaviour, Duration::from_millis(300)).await;
            let seen = responses(&observer);
            let peer_answers = seen.iter().filter(|(_, id)| *id == QUERY_ID).count();
            let service_answers = seen.iter().filter(|(_, id)| *id == META_QUERY_ID).count();
            assert_eq!(
                (peer_answers, service_answers),
                (1, 1),
                "a meta-query and a peer query in the same second are each answered"
            );
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

/// Rule 5's send path: an interface whose multicast send fails reports it,
/// through the channel from the interface task to the behaviour, as
/// `InterfaceFailed` for this node's own address. An nftables rule inside
/// the namespace drops output to the group, which makes the send itself
/// fail with `EPERM`. A deleted route or a downed interface does NOT
/// (measured: the socket's bound source address still routes the send),
/// so neither is used. Needs `nft`; without it this fails rather than
/// skips. (The bind path is the runtime test below; it does not use the
/// channel.)
#[test]
fn a_failed_send_reaches_the_behaviour_as_an_event() {
    if !in_namespace("a_failed_send_reaches_the_behaviour_as_an_event") {
        return;
    }
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            for rule in [
                "add table inet mdnsbounds",
                "add chain inet mdnsbounds out { type filter hook output priority 0 ; }",
                "add rule inet mdnsbounds out ip daddr 224.0.0.251 drop",
            ] {
                let status = std::process::Command::new("nft")
                    .args(rule.split(' '))
                    .status()
                    .expect("`nft` runs: this test needs nftables to make a send fail");
                assert!(status.success(), "nft {rule}");
            }
            let mut behaviour = behaviour();
            let expected: std::net::IpAddr = IFACE.parse().expect("ip");
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut seen = Vec::new();
            while Instant::now() < deadline {
                let (_, events) = drain(&mut behaviour, Duration::from_millis(50)).await;
                seen.extend(events);
                if seen.iter().any(|e| {
                    matches!(e, mdns::Event::InterfaceFailed { address, .. } if *address == expected)
                }) {
                    return;
                }
            }
            panic!("no InterfaceFailed for a send with no route: {seen:?}");
        });
}

/// Rules 4 and 5 together: a failed answer takes its once-a-second slot
/// like a sent one, so a host flooding queries at an interface whose
/// sends fail gets at most one failed answer a second -- and so at most
/// one `InterfaceFailed` for it -- not one per query. The nftables rule
/// drops every packet to the group except the flooder's, so the node's
/// answers and its own probes fail while the queries still arrive. THE
/// CONTROL is that failures are reported at all: a node that stopped
/// sending would pass the bound.
#[test]
fn a_flood_of_queries_at_a_failing_interface_fails_at_most_once_a_second() {
    if !in_namespace("a_flood_of_queries_at_a_failing_interface_fails_at_most_once_a_second") {
        return;
    }
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            let flood = Flood::new();
            let port = flood
                .socket
                .local_addr()
                .expect("the flooder's port")
                .port();
            for rule in [
                "add table inet mdnsbounds".to_owned(),
                "add chain inet mdnsbounds out { type filter hook output priority 0 ; }".to_owned(),
                format!(
                    "add rule inet mdnsbounds out ip daddr 224.0.0.251 udp sport != {port} drop"
                ),
            ] {
                let status = std::process::Command::new("nft")
                    .args(rule.split(' '))
                    .status()
                    .expect("`nft` runs: this test needs nftables to make a send fail");
                assert!(status.success(), "nft {rule}");
            }
            let mut behaviour = behaviour();
            let counts = behaviour.drop_counts();
            let _ = drain(&mut behaviour, Duration::from_millis(200)).await;

            let mut failures = 0_u64;
            let dropped_before = counts.failures_dropped();
            let unanswered_before = counts.queries_unanswered();
            let started = Instant::now();
            for _ in 0..20 {
                flood.send(&query());
                let (_, events) = drain(&mut behaviour, Duration::from_millis(50)).await;
                failures += events
                    .iter()
                    .filter(|e| matches!(e, mdns::Event::InterfaceFailed { .. }))
                    .count() as u64;
            }
            let elapsed = started.elapsed();
            let failures = failures + counts.failures_dropped() - dropped_before;
            assert!(failures > 0, "the sends really fail");
            // Two answers' slots over at most two seconds, and the node's
            // own probes (500 ms doubling) inside the same window.
            assert!(
                elapsed < Duration::from_secs(2) && failures <= 7,
                "{failures} failures in {elapsed:?} for 20 queries"
            );
            assert!(
                counts.queries_unanswered() - unanswered_before >= 15,
                "the failed answer held its slot, so the rest were refused"
            );
        });
}

/// Polls of `DeadWatcher`, the one test that uses it.
static WATCHER_POLLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// An interface watcher whose netlink connection has ended: `Err` on every
/// poll, as if-watch 3.2.2 does then. Bounded at a thousand so that a
/// behaviour which keeps polling it returns, and the test can count.
#[derive(Debug)]
struct DeadWatcher;

impl futures::Stream for DeadWatcher {
    type Item = std::io::Result<if_watch::IfEvent>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if WATCHER_POLLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 1000 {
            Poll::Ready(Some(Err(std::io::Error::other("netlink connection ended"))))
        } else {
            Poll::Pending
        }
    }
}

/// The tokio runtime with `DeadWatcher` as its interface watcher.
enum DeadWatcherProvider {}

impl mdns::Provider for DeadWatcherProvider {
    type Socket = <mdns::tokio::Tokio as mdns::Provider>::Socket;
    type Timer = <mdns::tokio::Tokio as mdns::Provider>::Timer;
    type Watcher = DeadWatcher;
    type TaskHandle = <mdns::tokio::Tokio as mdns::Provider>::TaskHandle;

    fn new_watcher() -> Result<Self::Watcher, std::io::Error> {
        Ok(DeadWatcher)
    }

    fn spawn(task: impl std::future::Future<Output = ()> + Send + 'static) -> Self::TaskHandle {
        <mdns::tokio::Tokio as mdns::Provider>::spawn(task)
    }
}

/// Rule 5's dead-watcher stop, measured through the `Provider` seam. A
/// watcher that returns `Err` on every poll is polled twice and then no
/// more, and `WatcherFailed` is reported exactly once and actually
/// RETURNED -- where the unpatched loop polled it until it stopped
/// erroring, which a real dead watcher never does, and returned nothing.
/// No namespace: no packet is involved, and the watcher is the test's own.
#[test]
fn a_dead_interface_watcher_is_stopped_and_reported_once() {
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            let config = mdns::Config {
                ttl: Duration::from_secs(360),
                query_interval: Duration::from_secs(3600),
                enable_ipv6: false,
            };
            let mut behaviour = mdns::Behaviour::<DeadWatcherProvider>::new(
                config,
                Keypair::generate_ed25519().public().to_peer_id(),
            )
            .expect("the test's own watcher");
            let mut failures = 0;
            for _ in 0..5 {
                poll_fn(|cx| {
                    while let Poll::Ready(event) = behaviour.poll(cx) {
                        if matches!(
                            event,
                            ToSwarm::GenerateEvent(mdns::Event::WatcherFailed { .. })
                        ) {
                            failures += 1;
                        }
                    }
                    Poll::Ready(())
                })
                .await;
            }
            assert_eq!(
                WATCHER_POLLS.load(std::sync::atomic::Ordering::SeqCst),
                2,
                "polled twice, then no more, over five polls of the behaviour"
            );
            assert_eq!(failures, 1, "reported once, and returned");
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

/// Drain `runtime`'s events for `ms` milliseconds, so its outbox never
/// holds up the Swarm it polls.
async fn pump(runtime: &mut interweave_transport_libp2p::SwarmRuntime, ms: u64) {
    let _ = tokio::time::timeout(Duration::from_millis(ms), async {
        while runtime.next_event().await.is_some() {}
    })
    .await;
}

/// Rule 7, read where an operator reads it: the crate's drop counts
/// through `SwarmRuntime::mdns_drop_counts`, driven NON-ZERO. Peers past
/// the provider's peer bound are announced and a burst of ten queries
/// sent.
/// Then two counters are read with values of their own -- evictions at
/// least one, unanswered queries nine to eleven -- so a handle
/// disconnected from the crate, a readout returning zeros, or those two
/// fields swapped in the readout fails. Swaps among the fields that are
/// zero here are NOT caught; an earlier version claimed every swap was
/// (#112 re-review N3) (#112 blind review F2: the only earlier read compared against
/// all zeros).
#[test]
fn the_runtime_reads_the_crates_own_drop_counts() {
    if !in_namespace("the_runtime_reads_the_crates_own_drop_counts") {
        return;
    }
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            use interweave_transport_libp2p::runtime::mdns_driver::MdnsSettings;
            use interweave_transport_libp2p::{SubstrateConfig, SwarmRuntime};

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
            let flood = Flood::new();
            let all = peers(mdns::MAX_DISCOVERED_PEERS + 64);
            pump(&mut runtime, 500).await;
            for (i, chunk) in all.chunks(PEERS_PER_PACKET).enumerate() {
                flood.send(&announcement(chunk, i * PEERS_PER_PACKET, 3600));
                pump(&mut runtime, 2).await;
            }
            // WAIT FOR AN EVICTION, sending further peers past the bound
            // while none has shown: under load the interface queue can drop
            // more than the peers past the bound, and then the store never
            // fills. An earlier version waited on an exact sum load could
            // overshoot, and failed twice in eighty parallel runs (#112
            // re-review N2).
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut more = all.len();
            loop {
                let c = runtime.mdns_drop_counts().expect("mDNS runs");
                if c.records_evicted >= 1 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "the flood past the bound never showed in the runtime's counts: {c:?}"
                );
                flood.send(&announcement(&peers(PEERS_PER_PACKET), more, 3600));
                more += PEERS_PER_PACKET;
                pump(&mut runtime, 50).await;
            }
            for _ in 0..10 {
                flood.send(&query());
            }
            pump(&mut runtime, 300).await;

            let c = runtime.mdns_drop_counts().expect("mDNS runs");
            assert_eq!(
                c.records_refused, 0,
                "equal TTLs, later: an eviction, never a refusal"
            );
            assert!(
                c.records_evicted >= 1,
                "evictions read through the runtime: {c:?}"
            );
            // Nine or ten of the burst, plus at most one of the node's own
            // probes if it landed inside the same second.
            assert!(
                (9..=11).contains(&c.queries_unanswered),
                "at most one of the ten answered, read through the runtime: {c:?}"
            );
            assert_eq!((c.packets_dropped, c.failures_dropped), (0, 0), "{c:?}");
            runtime.shutdown().await.expect("clean shutdown");
        });
}
