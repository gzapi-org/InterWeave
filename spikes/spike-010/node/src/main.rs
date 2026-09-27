// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SPIKE-010's node: one `SwarmRuntime` with mDNS on, composed with the
//! mDNS and static discovery providers and a `DiscoveryManager`, printing
//! what each layer sees.
//!
//! It composes what Stage 12's composition root will -- the runtime's
//! `MdnsDiscovered`/`MdnsExpired` pushed into the mDNS provider, both
//! providers drained into the manager, and an interface failure mapped
//! onto the provider's degraded state (`providers/mdns.md` §Failure) --
//! because nothing else in the tree composes them yet. What runs is the
//! substrate and the two providers at the pinned revision; this file only
//! wires and reports.
//!
//! # Output, and why it is text
//!
//! One line per fact, the row scripts matching on it: `ID <peer>`,
//! `LISTEN <addr>`, `EV <ms> <SwarmEvent Debug>`; on the report interval
//! `CAND <ms> <peer> sources=<a,b> addrs=<a b>` per candidate the
//! manager holds, `HEALTH <ms> mdns=<h> static=<h>` and `STORE <ms>
//! mdns admitted=<n> refused=<class:n,..>` (the runtime's learn-site
//! counts, ADR-0052); `DIALPEER <ms> <peer> <outcome>` for a dial that
//! asks the Swarm's address book, and `ADD <ms> <peer> <addr> <outcome>`
//! for an address given it through the command path.
//!
//! # Usage
//!
//! ```text
//! node keygen <path>
//! node run --identity <path> [options]
//!   --listen <multiaddr>              # repeatable
//!   --data <peer>                     # data-plane trust, repeatable
//!   --static <peer>@<multiaddr>       # a static-provider entry, repeatable
//!   --dial-peer <peer>                # after --dial-after-ms: DialPeer, the address book's own dial
//!   --add-address <peer>@<multiaddr>  # after --add-after-ms: the command path's address,
//!                                     # then every --dial-peer again
//!   --dial-after-ms N --add-after-ms N --report-every-ms N --run-for-s N
//! node announce --from-ip <ip> --peer <peer> --addr <multiaddr> [--ttl-s N] [--count N]
//!   # one unsolicited mDNS response per second naming <peer> at <addr>
//!   # (without /p2p), sent to 224.0.0.251:5353 from <ip>: bound to this
//!   # host's interface address, because a multicast send with no route
//!   # is sent out of the interface that owns its source address
//! ```

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use interweave_discovery_api::DiscoveryProvider;
use interweave_discovery_mdns::{MdnsDiscovery, SOURCE as MDNS};
use interweave_discovery_static::{SOURCE as STATIC, StaticBootstrapDiscovery, StaticEntry};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::mdns_driver::MdnsSettings;
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_transport_runtime::discovery::DiscoveryManager;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match args.first().map(String::as_str) {
        Some("keygen") => keygen(&args[1..]),
        Some("run") => run(&args[1..]).await,
        Some("announce") => announce(&args[1..]).await,
        _ => Err("usage: node keygen <path> | node run ... | node announce ...".to_owned()),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("node: {why}");
            ExitCode::from(2)
        }
    }
}

fn keygen(args: &[String]) -> Result<(), String> {
    let [path] = args else {
        return Err("keygen takes exactly one path".to_owned());
    };
    let identity = ProfileIdentity::generate();
    identity
        .save(&PathBuf::from(path))
        .map_err(|e| format!("saving {path}: {e:?}"))?;
    println!("{}", peer_of(&identity)?.as_str());
    Ok(())
}

fn peer_of(identity: &ProfileIdentity) -> Result<TransportIdentity, String> {
    identity
        .transport_identity()
        .map_err(|e| format!("peer id: {e:?}"))
}

#[derive(Default)]
struct Flags {
    identity: Option<PathBuf>,
    listen: Vec<Multiaddr>,
    data: Vec<TransportIdentity>,
    statics: Vec<(TransportIdentity, Multiaddr)>,
    dial_peer: Vec<TransportIdentity>,
    add_address: Vec<(TransportIdentity, Multiaddr)>,
    dial_after_ms: u64,
    add_after_ms: u64,
    report_every_ms: u64,
    run_for_s: Option<u64>,
}

fn parse(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags {
        report_every_ms: 1_000,
        ..Flags::default()
    };
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let mut value = || {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--identity" => flags.identity = Some(PathBuf::from(value()?)),
            "--listen" => flags.listen.push(addr(&value()?)?),
            "--data" => flags.data.push(peer(&value()?)?),
            "--static" => flags.statics.push(peer_at(&value()?)?),
            "--dial-peer" => flags.dial_peer.push(peer(&value()?)?),
            "--add-address" => flags.add_address.push(peer_at(&value()?)?),
            "--dial-after-ms" => flags.dial_after_ms = number(&value()?)?,
            "--add-after-ms" => flags.add_after_ms = number(&value()?)?,
            "--report-every-ms" => flags.report_every_ms = number(&value()?)?,
            "--run-for-s" => flags.run_for_s = Some(number(&value()?)?),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if flags.report_every_ms == 0 {
        return Err("--report-every-ms must be positive".to_owned());
    }
    Ok(flags)
}

fn number<T: std::str::FromStr>(text: &str) -> Result<T, String> {
    text.parse().map_err(|_| format!("not a number: {text}"))
}

fn addr(text: &str) -> Result<Multiaddr, String> {
    text.parse().map_err(|e| format!("not a multiaddr ({text}): {e}"))
}

fn peer(text: &str) -> Result<TransportIdentity, String> {
    TransportIdentity::parse(text).map_err(|e| format!("not a peer id ({text}): {e:?}"))
}

fn peer_at(text: &str) -> Result<(TransportIdentity, Multiaddr), String> {
    let (p, a) = text
        .split_once('@')
        .ok_or_else(|| format!("expected <peer>@<multiaddr>: {text}"))?;
    Ok((peer(p)?, addr(a)?))
}

/// The providers and the manager above them: what Stage 12's composer
/// will own.
struct Discovery {
    mdns: MdnsDiscovery,
    statics: StaticBootstrapDiscovery,
    manager: DiscoveryManager,
    trust: PeerTrustPolicy,
}

impl Discovery {
    fn new(statics: Vec<StaticEntry>, trust: PeerTrustPolicy) -> Result<Self, String> {
        let mut mdns = MdnsDiscovery::new();
        let mut statics =
            StaticBootstrapDiscovery::new(statics).map_err(|e| format!("static: {e:?}"))?;
        let mut manager = DiscoveryManager::new();
        manager
            .register(mdns.descriptor(), 0)
            .map_err(|e| format!("register mdns: {e:?}"))?;
        manager
            .register(statics.descriptor(), 0)
            .map_err(|e| format!("register static: {e:?}"))?;
        mdns.start(0).map_err(|e| format!("start mdns: {e:?}"))?;
        statics.start(0).map_err(|e| format!("start static: {e:?}"))?;
        let mut discovery = Self {
            mdns,
            statics,
            manager,
            trust,
        };
        discovery.pump(0);
        Ok(discovery)
    }

    /// Drain both providers into the manager. A refusal is printed, never
    /// swallowed: it is what a conformance row would look for.
    fn pump(&mut self, now: u64) {
        for (name, events) in [
            (MDNS, self.mdns.drain_events(now, 256)),
            (STATIC, self.statics.drain_events(now, 256)),
        ] {
            for event in events {
                if let Err(why) = self.manager.on_event(name, event, now, &self.trust) {
                    println!("REJECTED {now} {name} {why:?}");
                }
            }
        }
    }

    fn report(&mut self, now: u64, runtime: &SwarmRuntime) {
        self.manager.sweep(now);
        for candidate in self.manager.candidates(now) {
            let sources: Vec<&str> = candidate.sources.iter().map(String::as_str).collect();
            let addrs: Vec<String> = candidate
                .addresses
                .iter()
                .map(|a| format!("{a:?}"))
                .collect();
            println!(
                "CAND {now} {} sources={} addrs={}",
                candidate.peer_id.as_str(),
                sources.join(","),
                addrs.join(" ")
            );
        }
        println!(
            "HEALTH {now} mdns={:?} static={:?}",
            self.manager.provider_health(MDNS),
            self.manager.provider_health(STATIC),
        );
        if let Some(counts) = runtime
            .store_refusals()
            .get(interweave_transport_libp2p::store_refusals::store::MDNS)
        {
            let refused: Vec<String> = counts
                .refused
                .iter()
                .map(|(class, n)| format!("{class}:{n}"))
                .collect();
            println!(
                "STORE {now} mdns admitted={} refused={} over_bound={}",
                counts.admitted,
                refused.join(","),
                counts.over_bound
            );
        }
    }
}

fn config() -> SubstrateConfig {
    SubstrateConfig {
        mdns: Some(MdnsSettings {
            // Short, so a row sees a query within its window; the default
            // 90 s is the clamp's cache-maintenance point, not a limit.
            query_interval_ms: 5_000,
            ..MdnsSettings::default()
        }),
        ..SubstrateConfig::default()
    }
}

async fn run(args: &[String]) -> Result<(), String> {
    let flags = parse(args)?;
    let path = flags
        .identity
        .clone()
        .ok_or_else(|| "--identity is required".to_owned())?;
    let identity = ProfileIdentity::load(&path).map_err(|e| format!("loading identity: {e:?}"))?;
    println!("ID {}", peer_of(&identity)?.as_str());

    let policy = || {
        PeerTrustPolicy::new(flags.data.iter().cloned()).map_err(|e| format!("data trust: {e:?}"))
    };
    let trust = TrustSources::new(
        policy()?,
        InfrastructureSet::new(std::iter::empty()).map_err(|e| format!("infra: {e:?}"))?,
    );
    let statics = flags
        .statics
        .iter()
        .map(|(p, a)| {
            StaticEntry::new(p.clone(), format!("{a}/p2p/{}", p.as_str()))
                .map_err(|e| format!("static entry: {e:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut discovery = Discovery::new(statics, policy()?)?;
    let mut runtime =
        SwarmRuntime::start(&identity, config(), trust).map_err(|e| format!("start: {e:?}"))?;
    for address in &flags.listen {
        let bound = runtime
            .listen(address.clone())
            .await
            .map_err(|e| format!("listen {address}: {e:?}"))?;
        println!("LISTEN {bound}");
    }

    let started = Instant::now();
    let ms = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut report = tokio::time::interval(Duration::from_millis(flags.report_every_ms));
    let dial_at = tokio::time::sleep(Duration::from_millis(flags.dial_after_ms));
    tokio::pin!(dial_at);
    let mut dialled = flags.dial_peer.is_empty();
    let add_at = tokio::time::sleep(Duration::from_millis(flags.add_after_ms));
    tokio::pin!(add_at);
    let mut added = flags.add_address.is_empty();
    let stop_at = tokio::time::sleep(flags.run_for_s.map_or(Duration::MAX, Duration::from_secs));
    tokio::pin!(stop_at);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("SIGTERM handler: {e}"))?;
    let mut peers: BTreeSet<TransportIdentity> = BTreeSet::new();

    loop {
        tokio::select! {
            event = runtime.next_event() => {
                let Some(event) = event else {
                    println!("END {} runtime closed", ms());
                    return Ok(());
                };
                let now = ms();
                match &event {
                    SwarmEvent::Connected { peer, .. } => { peers.insert(peer.clone()); }
                    SwarmEvent::Disconnected { peer, .. } => { peers.remove(peer); }
                    SwarmEvent::MdnsDiscovered { candidates } => {
                        for candidate in candidates {
                            for address in &candidate.addresses {
                                let _ = discovery.mdns.push_discovered(
                                    candidate.peer_id.as_str(), address, now,
                                );
                            }
                        }
                    }
                    SwarmEvent::MdnsExpired { expired } => {
                        for (peer, address) in expired {
                            let _ = discovery.mdns.push_expired(peer.as_str(), address, now);
                        }
                    }
                    // THE DEGRADED SIGNAL (`providers/mdns.md` §Failure):
                    // what Stage 12's composer will map onto the provider.
                    SwarmEvent::MdnsInterfaceFailed { .. }
                    | SwarmEvent::MdnsWatcherFailed { .. }
                    | SwarmEvent::MdnsRebuildFailed { .. }
                    | SwarmEvent::MdnsUnavailable { .. } => {
                        discovery.mdns.report_backend_down(now);
                    }
                    _ => {}
                }
                discovery.pump(now);
                println!("EV {now} {event:?}");
            }
            _ = report.tick() => {
                let now = ms();
                discovery.pump(now);
                discovery.report(now, &runtime);
                println!("RES {now} peers={}", peers.len());
            }
            () = &mut dial_at, if !dialled => {
                dialled = true;
                for peer in &flags.dial_peer {
                    let outcome = runtime.dial_peer(peer.clone()).await;
                    println!("DIALPEER {} {} {outcome:?}", ms(), peer.as_str());
                }
            }
            () = &mut add_at, if !added => {
                added = true;
                for (peer, address) in &flags.add_address {
                    let outcome = runtime.add_address(peer.clone(), address.clone()).await;
                    println!("ADD {} {} {address} {outcome:?}", ms(), peer.as_str());
                }
                // THE CONTROL for the address-book row: the same dial, now
                // that the command path has given the book an address.
                for peer in &flags.dial_peer {
                    let outcome = runtime.dial_peer(peer.clone()).await;
                    println!("DIALPEER {} {} {outcome:?}", ms(), peer.as_str());
                }
            }
            () = &mut stop_at => {
                println!("END {} run-for elapsed", ms());
                return Ok(());
            }
            _ = terminate.recv() => {
                println!("END {} SIGTERM", ms());
                return Ok(());
            }
        }
    }
}

/// One unsolicited mDNS response naming `peer` at `address`: the shape the
/// crate accepts from any host on the domain (ADR-0053 rule 6), the
/// encoding `crates/transport/libp2p/tests/mdns_bounds.rs` sends. Note the
/// crate rewrites an announced address's FIRST host to the packet's
/// observed source unless that source is IPv6 link-local
/// (`iface/query.rs`, `_address_translation`), so what is refused at the
/// learn site is decided by the rest of the address.
async fn announce(args: &[String]) -> Result<(), String> {
    let mut peer_id = None;
    let mut address = None;
    let mut from_ip = None;
    let mut ttl_s: u32 = 120;
    let mut count: u32 = 10;
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--from-ip" => {
                from_ip = Some(
                    value
                        .parse::<Ipv4Addr>()
                        .map_err(|_| format!("not an IPv4 address: {value}"))?,
                );
            }
            "--peer" => peer_id = Some(peer(&value)?),
            "--addr" => address = Some(value),
            "--ttl-s" => ttl_s = number(&value)?,
            "--count" => count = number(&value)?,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    let peer_id = peer_id.ok_or("--peer is required")?;
    let address = address.ok_or("--addr is required")?;
    let packet = response(peer_id.as_str(), &address, ttl_s)?;
    let from_ip = from_ip.ok_or("--from-ip is required")?;
    let socket = UdpSocket::bind(SocketAddrV4::new(from_ip, 0))
        .map_err(|e| format!("bind: {e}"))?;
    let group = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
    for n in 0..count {
        match socket.send_to(&packet, group) {
            Ok(_) => println!("SENT {n} {address}/p2p/{}", peer_id.as_str()),
            Err(e) => println!("SENDFAIL {n} {e}"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}

fn append_name(out: &mut Vec<u8>, labels: &[&[u8]]) -> Result<(), String> {
    for label in labels {
        out.push(u8::try_from(label.len()).map_err(|_| "label too long")?);
        out.extend_from_slice(label);
    }
    out.push(0);
    Ok(())
}

fn append_record(out: &mut Vec<u8>, rtype: u16, ttl: u32, rdata: &[u8]) -> Result<(), String> {
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&1_u16.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(
        &u16::try_from(rdata.len())
            .map_err(|_| "rdata too long")?
            .to_be_bytes(),
    );
    out.extend_from_slice(rdata);
    Ok(())
}

/// A response with one PTR answer and one TXT additional carrying
/// `dnsaddr=<address>/p2p/<peer>`.
fn response(peer: &str, address: &str, ttl: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(512);
    for field in [0_u16, 0x8400, 0, 1, 0, 1] {
        out.extend_from_slice(&field.to_be_bytes());
    }
    append_name(&mut out, &[b"_p2p", b"_udp", b"local"])?;
    let mut target = Vec::new();
    append_name(&mut target, &[b"spike010", b"local"])?;
    append_record(&mut out, 12, ttl, &target)?;
    append_name(&mut out, &[b"spike010", b"local"])?;
    let value = format!("dnsaddr={address}/p2p/{peer}");
    let mut rdata = vec![u8::try_from(value.len()).map_err(|_| "dnsaddr too long")?];
    rdata.extend_from_slice(value.as_bytes());
    append_record(&mut out, 16, ttl, &rdata)?;
    Ok(out)
}
