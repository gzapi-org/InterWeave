// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SPIKE-004 phase B's node: one `SwarmRuntime`, configured from flags,
//! printing what it sees.
//!
//! It composes the connectivity behaviours the way Stage 12's
//! composition root will -- a `Some` per configured block -- because
//! nothing else in the tree does yet: every production switch is `None`
//! by default (the owner's 2026-09-07 ruling). What runs is the
//! substrate at the pinned revision; this file only chooses its switches
//! and reports.
//!
//! # Output, and why it is text
//!
//! `SwarmEvent` implements `Debug` and nothing else, so each event is one
//! line, `EV <ms since start> <Debug form>`, and the row scripts match
//! variant names in it. A serialised form would be a second definition
//! of the event that could drift from the real one; the `Debug` form
//! cannot. Other lines: `ID <peer>` once at start, `LISTEN <addr>` per
//! bound address, `DIAL <ms> <peer> <addr> <outcome>`, and `RES <ms>
//! rss_kb=<n> fds=<n> peers=<n>` on the resource interval -- the last
//! read from `/proc/self`, so it is this process as the kernel accounts
//! for it.
//!
//! # Usage
//!
//! ```text
//! node keygen <path>                      # write an identity, print its peer id
//! node run --identity <path> [options]
//!   --listen <multiaddr>                  # repeatable
//!   --relay-server [--max-reservations N] [--max-circuits N]
//!   --autonat-server
//!   --relay <peer>@<multiaddr>            # a static relay to reserve on, repeatable
//!   --relay-transport                     # the circuit transport with no relay to reserve on
//!   --autonat <peer>@<multiaddr>          # a static AutoNAT server, repeatable
//!   --distinct N                          # AutoNAT successes from distinct servers (default 2)
//!   --dcutr [--stability-ms N]
//!   --data <peer> / --infra <peer>        # trust, repeatable
//!   --dial <peer>@<multiaddr>             # after --dial-after-ms, repeatable
//!   --dial-after-ms N  --run-for-s N  --resources-every-ms N
//! ```

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::autonat_driver::{AutonatClientSettings, StaticServer};
use interweave_transport_libp2p::runtime::autonat_server_driver::AutonatServerSettings;
use interweave_transport_libp2p::runtime::dcutr_driver::DcutrSettings;
use interweave_transport_libp2p::runtime::relay_driver::{RelayClientSettings, StaticRelay};
use interweave_transport_libp2p::runtime::relay_server_driver::RelayServerSettings;
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match args.first().map(String::as_str) {
        Some("keygen") => keygen(&args[1..]),
        Some("run") => run(&args[1..]).await,
        _ => Err("usage: node keygen <path> | node run --identity <path> [options]".to_owned()),
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

/// Every flag `run` takes, parsed before anything starts, so a typo is
/// refused rather than ignored.
#[derive(Default)]
struct Flags {
    identity: Option<PathBuf>,
    listen: Vec<Multiaddr>,
    relay_server: bool,
    max_reservations: Option<usize>,
    max_circuits: Option<usize>,
    autonat_server: bool,
    relays: Vec<(TransportIdentity, Multiaddr)>,
    relay_transport: bool,
    autonat: Vec<(TransportIdentity, Multiaddr)>,
    distinct: u32,
    dcutr: bool,
    stability_ms: Option<u64>,
    data: Vec<TransportIdentity>,
    infra: Vec<TransportIdentity>,
    dial: Vec<(TransportIdentity, Multiaddr)>,
    dial_after_ms: u64,
    run_for_s: Option<u64>,
    resources_every_ms: u64,
}

fn parse(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags {
        distinct: 2,
        resources_every_ms: 5_000,
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
            "--relay-server" => flags.relay_server = true,
            "--max-reservations" => flags.max_reservations = Some(number(&value()?)?),
            "--max-circuits" => flags.max_circuits = Some(number(&value()?)?),
            "--autonat-server" => flags.autonat_server = true,
            "--relay" => flags.relays.push(peer_at(&value()?)?),
            "--relay-transport" => flags.relay_transport = true,
            "--autonat" => flags.autonat.push(peer_at(&value()?)?),
            "--distinct" => flags.distinct = number(&value()?)?,
            "--dcutr" => flags.dcutr = true,
            "--stability-ms" => flags.stability_ms = Some(number(&value()?)?),
            "--data" => flags.data.push(peer(&value()?)?),
            "--infra" => flags.infra.push(peer(&value()?)?),
            "--dial" => flags.dial.push(peer_at(&value()?)?),
            "--dial-after-ms" => flags.dial_after_ms = number(&value()?)?,
            "--run-for-s" => flags.run_for_s = Some(number(&value()?)?),
            "--resources-every-ms" => flags.resources_every_ms = number(&value()?)?,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if flags.resources_every_ms == 0 {
        return Err("--resources-every-ms must be positive".to_owned());
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

/// `<peer>@<multiaddr>`: the peer and the address it is reached at.
fn peer_at(text: &str) -> Result<(TransportIdentity, Multiaddr), String> {
    let (p, a) = text
        .split_once('@')
        .ok_or_else(|| format!("expected <peer>@<multiaddr>: {text}"))?;
    Ok((peer(p)?, addr(a)?))
}

fn config(flags: &Flags) -> SubstrateConfig {
    let mut config = SubstrateConfig::default();
    if flags.relay_server {
        let mut server = RelayServerSettings::default();
        if let Some(n) = flags.max_reservations {
            server.max_reservations = n;
        }
        if let Some(n) = flags.max_circuits {
            server.max_circuits = n;
        }
        config.relay_server = Some(server);
    }
    if flags.autonat_server {
        config.autonat_server = Some(AutonatServerSettings::default());
    }
    if !flags.relays.is_empty() || flags.relay_transport {
        config.relay_client = Some(RelayClientSettings {
            static_relays: flags
                .relays
                .iter()
                .map(|(p, a)| StaticRelay {
                    peer: p.clone(),
                    address: format!("{a}/p2p/{}", p.as_str()),
                })
                .collect(),
            ..RelayClientSettings::default()
        });
    }
    if !flags.autonat.is_empty() {
        // `CONNECTIVITY.md` §7's defaults, restated because the settings
        // type has no `Default`: its profile block is where they live.
        config.autonat_client = Some(AutonatClientSettings {
            static_servers: flags
                .autonat
                .iter()
                .map(|(p, a)| StaticServer {
                    peer: p.clone(),
                    address: format!("{a}/p2p/{}", p.as_str()),
                })
                .collect(),
            use_authorized_identify_servers: false,
            required_distinct_successes: flags.distinct,
            success_evidence_ttl_ms: 15 * 60 * 1000,
            refresh_interval_ms: 5 * 60 * 1000,
            max_candidate_addresses_per_cycle: 4,
        });
    }
    if flags.dcutr {
        let mut dcutr = DcutrSettings::default();
        if let Some(ms) = flags.stability_ms {
            dcutr.direct_stability_period_ms = ms;
        }
        config.dcutr = Some(dcutr);
    }
    config
}

async fn run(args: &[String]) -> Result<(), String> {
    let flags = parse(args)?;
    let path = flags
        .identity
        .clone()
        .ok_or_else(|| "--identity is required".to_owned())?;
    let identity = ProfileIdentity::load(&path).map_err(|e| format!("loading identity: {e:?}"))?;
    println!("ID {}", peer_of(&identity)?.as_str());

    let trust = TrustSources::new(
        PeerTrustPolicy::new(flags.data.iter().cloned()).map_err(|e| format!("data trust: {e:?}"))?,
        InfrastructureSet::new(flags.infra.iter().cloned())
            .map_err(|e| format!("infrastructure set: {e:?}"))?,
    );
    let mut runtime = SwarmRuntime::start(&identity, config(&flags), trust)
        .map_err(|e| format!("start: {e:?}"))?;
    for address in &flags.listen {
        let bound = runtime
            .listen(address.clone())
            .await
            .map_err(|e| format!("listen {address}: {e:?}"))?;
        println!("LISTEN {bound}");
    }

    let started = Instant::now();
    let ms = || started.elapsed().as_millis();
    let mut resources = tokio::time::interval(Duration::from_millis(flags.resources_every_ms));
    let dial_at = tokio::time::sleep(Duration::from_millis(flags.dial_after_ms));
    tokio::pin!(dial_at);
    let mut dialled = flags.dial.is_empty();
    let stop_at = tokio::time::sleep(flags.run_for_s.map_or(Duration::MAX, Duration::from_secs));
    tokio::pin!(stop_at);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("SIGTERM handler: {e}"))?;
    // Peers with at least one connection, from the runtime's own
    // once-per-logical-peer events (`dialing::path_events`).
    let mut peers: BTreeSet<TransportIdentity> = BTreeSet::new();

    loop {
        tokio::select! {
            event = runtime.next_event() => {
                let Some(event) = event else {
                    println!("END {} runtime closed", ms());
                    return Ok(());
                };
                match &event {
                    SwarmEvent::Connected { peer, .. } => { peers.insert(peer.clone()); }
                    SwarmEvent::Disconnected { peer, .. } => { peers.remove(peer); }
                    _ => {}
                }
                println!("EV {} {event:?}", ms());
            }
            _ = resources.tick() => {
                println!("RES {} {} peers={}", ms(), proc_self(), peers.len());
            }
            () = &mut dial_at, if !dialled => {
                dialled = true;
                for (peer, address) in &flags.dial {
                    let outcome = runtime.dial(peer.clone(), address.clone()).await;
                    println!("DIAL {} {} {address} {outcome:?}", ms(), peer.as_str());
                }
            }
            () = &mut stop_at => {
                println!("RES {} {} peers={}", ms(), proc_self(), peers.len());
                println!("END {} run-for elapsed", ms());
                return Ok(());
            }
            _ = terminate.recv() => {
                println!("RES {} {} peers={}", ms(), proc_self(), peers.len());
                println!("END {} SIGTERM", ms());
                return Ok(());
            }
        }
    }
}

/// This process as the kernel accounts for it: resident set and open
/// descriptors. `?` where `/proc` could not be read, never a guess.
fn proc_self() -> String {
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.split_whitespace().next().map(str::to_owned))
        })
        .unwrap_or_else(|| "?".to_owned());
    let fds = std::fs::read_dir("/proc/self/fd")
        .map(|d| d.count().to_string())
        .unwrap_or_else(|_| "?".to_owned());
    format!("rss_kb={rss} fds={fds}")
}
