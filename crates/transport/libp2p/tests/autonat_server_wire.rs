// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `ProbeServer` over real sockets, with the bare crate as the control.
//!
//! Two raw Swarms on loopback -- the vendored client on one, the
//! vendored server behind `ProbeServer` on the other, Identify beside
//! each, no substrate around either. THE CONTROL is
//! `autonat_outcome_wire.rs` beside this file: the same client against
//! the bare vendored server dials back to a loopback candidate and
//! confirms it, so a harness of this shape sees a dial-back when one is
//! made. Here the wrapper refuses the same request under `AUTONAT.md`
//! §7 -- a loopback target, then a client over its budget -- and the
//! measurement is what does NOT happen: no dial reaches the Swarm, no
//! connection reaches the client, and the two refusals reach the client
//! as two different classes.
//!
//! What this file does NOT show: the substrate's composition -- the
//! class gate, the outbound gate taking back the ticket of a refused
//! dial-back, the inbound arm retaining a client -- which
//! `tests/connectivity/tests/autonat_server.rs` measures through a
//! `SwarmRuntime`.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use futures::StreamExt as _;
use interweave_transport_libp2p::probe_server::{
    ProbeBudgets, ProbeRefusal, ProbeServer, ProbeServerEvent,
};
use interweave_transport_libp2p::runtime::autonat_driver::DIAL_BACK_FAILURE_TEXTS;
use libp2p::autonat::v2::client::{self, Behaviour as Client, Config as ClientConfig};
use libp2p::autonat::v2::server::Behaviour as Server;
use libp2p::swarm::{FromSwarm, NetworkBehaviour, NewExternalAddrCandidate, SwarmEvent};
use libp2p::{Multiaddr, PeerId, Swarm, identify, identity};

/// Identify rides beside every behaviour here, because that is how the
/// client learns a peer speaks the protocol: its handler reacts to
/// `RemoteProtocolsChange`, which only Identify produces.
fn identify(k: &identity::Keypair) -> identify::Behaviour {
    identify::Behaviour::new(identify::Config::new(
        "/interweave-autonat-wire-test/1".to_owned(),
        k.public(),
    ))
}

#[derive(NetworkBehaviour)]
struct RawClient {
    identify: identify::Behaviour,
    client: Client,
}

#[derive(NetworkBehaviour)]
struct Subject {
    identify: identify::Behaviour,
    server: ProbeServer,
}

const PATIENCE: Duration = Duration::from_secs(20);

/// How long the subject's client waits for a dial-back that must not
/// come. The control's arrives well inside a second on loopback.
const SILENCE: Duration = Duration::from_secs(3);

fn swarm<B: NetworkBehaviour>(build: impl FnOnce(&identity::Keypair) -> B) -> Swarm<B> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the substrate uses")
        .with_behaviour(|k| build(k))
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build()
}

/// A raw client that sweeps every 200 ms, so a test does not wait for
/// the crate's five-second tick.
fn client() -> Swarm<RawClient> {
    swarm(|k| RawClient {
        identify: identify(k),
        client: Client::new(
            rand::rngs::OsRng,
            ClientConfig::default().with_probe_interval(Duration::from_millis(200)),
        ),
    })
}

async fn bound<B: NetworkBehaviour>(swarm: &mut Swarm<B>) -> Multiaddr
where
    B::ToSwarm: std::fmt::Debug,
{
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("a listen address"))
        .expect("listens");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, swarm.select_next_some()).await {
            Ok(SwarmEvent::NewListenAddr { address, .. }) => return address,
            Ok(_) => {}
            Err(_) => panic!("no listen address within {PATIENCE:?}"),
        }
    }
}

fn candidate(swarm: &mut Swarm<RawClient>, addr: &Multiaddr) {
    swarm
        .behaviour_mut()
        .client
        .on_swarm_event(FromSwarm::NewExternalAddrCandidate(
            NewExternalAddrCandidate { addr },
        ));
}

/// What one probe came to, as the CLIENT saw it, with whether a
/// dial-back connection arrived at the client meanwhile.
#[derive(Debug)]
struct Seen {
    outcome: Result<(), String>,
    dial_back_arrived: bool,
}

/// Drive `client` and `server` until the client reports a probe
/// outcome, collecting the server's events into `server_events`.
async fn probe<S: NetworkBehaviour>(
    client_swarm: &mut Swarm<RawClient>,
    server_swarm: &mut Swarm<S>,
    server_events: &mut Vec<S::ToSwarm>,
    server_dialled: &mut bool,
) -> Seen
where
    S::ToSwarm: std::fmt::Debug,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut dial_back_arrived = false;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "no probe outcome within {PATIENCE:?}");
        tokio::select! {
            event = client_swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(RawClientEvent::Client(client::Event { result, .. })) => {
                    return Seen {
                        outcome: result.map_err(|e| e.to_string()),
                        dial_back_arrived,
                    };
                }
                SwarmEvent::ConnectionEstablished { endpoint, .. } if endpoint.is_listener() => {
                    dial_back_arrived = true;
                }
                _ => {}
            },
            event = server_swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(e) => server_events.push(e),
                SwarmEvent::Dialing { .. } => *server_dialled = true,
                _ => {}
            },
            () = tokio::time::sleep(remaining) => {}
        }
    }
}

/// Keep both sides running for `window` and report whether a dial-back
/// reached the client in that time.
async fn silence<S: NetworkBehaviour>(
    client_swarm: &mut Swarm<RawClient>,
    server_swarm: &mut Swarm<S>,
    server_dialled: &mut bool,
    window: Duration,
) -> bool
where
    S::ToSwarm: std::fmt::Debug,
{
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        tokio::select! {
            event = client_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionEstablished { endpoint, .. } = event
                    && endpoint.is_listener()
                {
                    return true;
                }
            }
            event = server_swarm.select_next_some() => {
                if matches!(event, SwarmEvent::Dialing { .. }) {
                    *server_dialled = true;
                }
            }
            () = tokio::time::sleep(remaining) => return false,
        }
    }
}

/// The wrapper's own events out of the subject's, Identify's aside.
fn probe_events(events: &[SubjectEvent]) -> Vec<ProbeServerEvent> {
    events
        .iter()
        .filter_map(|e| match e {
            SubjectEvent::Server(p) => Some(p.clone()),
            SubjectEvent::Identify(_) => None,
        })
        .collect()
}

async fn connect<S: NetworkBehaviour>(
    client_swarm: &mut Swarm<RawClient>,
    server_swarm: &mut Swarm<S>,
    server_addr: &Multiaddr,
    server_id: PeerId,
) where
    S::ToSwarm: std::fmt::Debug,
{
    client_swarm
        .dial(
            server_addr
                .clone()
                .with_p2p(server_id)
                .expect("a peer address"),
        )
        .expect("dials");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut established = false;
    while !established {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "not connected within {PATIENCE:?}");
        tokio::select! {
            event = client_swarm.select_next_some() => {
                if matches!(event, SwarmEvent::ConnectionEstablished { .. }) {
                    established = true;
                }
            }
            _ = server_swarm.select_next_some() => {}
            () = tokio::time::sleep(remaining) => {}
        }
    }
}

#[tokio::test]
async fn the_wrapper_refuses_a_loopback_dial_back_and_no_socket_is_opened() {
    // THE SUBJECT: the same server behind `ProbeServer`.
    let mut subject = swarm(|k| Subject {
        identify: identify(k),
        server: ProbeServer::new(Server::default(), ProbeBudgets::default()),
    });
    let subject_addr = bound(&mut subject).await;
    let subject_id = *subject.local_peer_id();
    let mut c1 = client();
    let c1_addr = bound(&mut c1).await;
    connect(&mut c1, &mut subject, &subject_addr, subject_id).await;
    candidate(&mut c1, &c1_addr);
    let mut subject_events = Vec::new();
    let mut subject_dialled = false;
    let seen = probe(
        &mut c1,
        &mut subject,
        &mut subject_events,
        &mut subject_dialled,
    )
    .await;
    let failure = seen
        .outcome
        .expect_err("a loopback candidate is refused under §7");
    assert!(
        DIAL_BACK_FAILURE_TEXTS.contains(&failure.as_str()),
        "a target refusal reaches the client as a probe failure the adapter classifies: {failure}"
    );
    assert!(!seen.dial_back_arrived, "no dial-back reached the client");
    assert!(
        !subject_dialled,
        "and no dial reached the subject's Swarm at all"
    );
    assert_eq!(
        probe_events(&subject_events),
        vec![ProbeServerEvent::Refused {
            client: *c1.local_peer_id(),
            address: Some(c1_addr.clone()),
            reason: ProbeRefusal::NotGlobal,
        }],
        "refused by name, with the address the crate named"
    );
    assert!(
        !silence(&mut c1, &mut subject, &mut subject_dialled, SILENCE).await,
        "and nothing arrives later either"
    );
    assert!(!subject_dialled);
    let counters = subject.behaviour().server.counters();
    assert_eq!(counters.refused(ProbeRefusal::NotGlobal), 1);
    assert_eq!(
        counters.served_ok + counters.served_failed + counters.served_unrecorded,
        0,
        "the crate's own report on the refused request is not a second event"
    );
    assert_eq!(
        subject.behaviour().server.in_flight(),
        0,
        "and the refusal left no dial in flight"
    );
}

#[tokio::test]
async fn a_probe_over_the_client_budget_is_refused_before_the_crate_and_the_client_hears_no_outcome()
 {
    let mut subject = swarm(|k| Subject {
        identify: identify(k),
        server: ProbeServer::new(
            Server::default(),
            ProbeBudgets {
                max_concurrent: 8,
                per_client_per_minute: 1,
                global_per_minute: 60,
            },
        ),
    });
    let subject_addr = bound(&mut subject).await;
    let subject_id = *subject.local_peer_id();
    let mut c = client();
    let c_addr = bound(&mut c).await;
    connect(&mut c, &mut subject, &subject_addr, subject_id).await;

    // The first probe spends the client's one start; it is then refused
    // on its target, which is the control that the budget was charged
    // by a probe the crate did examine.
    candidate(&mut c, &c_addr);
    let mut events = Vec::new();
    let mut dialled = false;
    let first = probe(&mut c, &mut subject, &mut events, &mut dialled).await;
    assert!(
        DIAL_BACK_FAILURE_TEXTS.contains(&first.outcome.expect_err("refused on target").as_str())
    );
    let refusals = probe_events(&events);
    assert_eq!(refusals.len(), 1);
    assert!(matches!(
        refusals[0],
        ProbeServerEvent::Refused {
            reason: ProbeRefusal::NotGlobal,
            ..
        }
    ));

    // A second candidate, a second request, no budget left: the
    // request never reaches the crate, the crate answers an internal
    // error, and the client's crate reads that as I/O -- which it does
    // NOT report as an outcome: the candidate goes back to `Untested`
    // and the sweep asks again every tick (ADR-0051: `Io` resets and
    // returns without emitting). So what the wire shows is the
    // server's refusal, repeated at the client's sweep, and NO client
    // outcome and NO dial in the same window.
    let second_addr: Multiaddr = "/ip4/127.0.0.1/tcp/1".parse().expect("a literal");
    candidate(&mut c, &second_addr);
    let arrived = refusals_within(&mut c, &mut subject, &mut events, &mut dialled, SILENCE).await;
    assert!(
        !arrived,
        "the client heard no outcome for the budget-refused probe"
    );
    let refusals = probe_events(&events);
    let client_rate = refusals
        .iter()
        .filter(|r| {
            matches!(
                r,
                ProbeServerEvent::Refused {
                    address: None,
                    reason: ProbeRefusal::ClientRate,
                    ..
                }
            )
        })
        .count();
    assert!(
        client_rate >= 2,
        "refused before the crate named the address, and again at the next sweep: {client_rate}"
    );
    assert!(!dialled, "neither refusal reached the Swarm as a dial");
    let counters = subject.behaviour().server.counters();
    // The count leads the events: an event waits for a poll, the count
    // does not, so the two agree only up to the last poll.
    assert!(counters.refused(ProbeRefusal::ClientRate) as usize >= client_rate);
    assert_eq!(
        counters.served_ok + counters.served_failed + counters.served_unrecorded,
        0,
        "a refused request is reported once, never also as served"
    );
    assert_eq!(counters.refused(ProbeRefusal::NotGlobal), 1);
}

/// Drive both sides for `window`, collecting the server's events;
/// `true` if the client reported any probe outcome meanwhile.
async fn refusals_within<S: NetworkBehaviour>(
    client_swarm: &mut Swarm<RawClient>,
    server_swarm: &mut Swarm<S>,
    server_events: &mut Vec<S::ToSwarm>,
    server_dialled: &mut bool,
    window: Duration,
) -> bool
where
    S::ToSwarm: std::fmt::Debug,
{
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        tokio::select! {
            event = client_swarm.select_next_some() => {
                if matches!(event, SwarmEvent::Behaviour(RawClientEvent::Client(_))) {
                    return true;
                }
            }
            event = server_swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(e) => server_events.push(e),
                SwarmEvent::Dialing { .. } => *server_dialled = true,
                _ => {}
            },
            () = tokio::time::sleep(remaining) => return false,
        }
    }
}
