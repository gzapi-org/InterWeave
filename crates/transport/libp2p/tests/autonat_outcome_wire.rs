// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A probe outcome the vendored crate actually produced, fed to the
//! adapter's classifier.
//!
//! Two raw Swarms on loopback -- the vendored client on one, the
//! vendored server on the other, Identify beside each, no substrate
//! around either -- negotiate a genuine dial request and a genuine
//! dial-back. This is the crate-level two-Swarm test ADR-0051 Decision
//! 3 owed to "the step that constructs these behaviours", and it can
//! run on loopback only because nothing here applies `AUTONAT.md` §6:
//! the raw client is fed loopback candidates -- one injected, one that
//! Identify hands it on its own -- and the bare server dials back to
//! whatever it is asked.
//!
//! What it is FOR: `classify_outcome` matched, until this test, an
//! error text that never reaches the public event, so every real
//! failure was "no outcome" and no server's failure vote was ever
//! recorded -- and every test of it passed, because each fed it a
//! `String` of the shape the author expected. The failure here is one
//! the server made: a dial-back to a port nothing listens on.
//!
//! What it also pins, for ADR-0051: with the candidate `Received`,
//! `retest` returns it to the sweep and a SECOND dial-back arrives --
//! the patch driven by the mechanism it was written for rather than
//! by `validate_addr`.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use futures::StreamExt as _;
use interweave_transport_libp2p::runtime::autonat_driver::{
    DIAL_BACK_FAILURE_TEXTS, classify_outcome,
};
use interweave_transport_runtime::reachability::ProbeOutcome;
use libp2p::autonat::v2::client::{self, Behaviour as Client, Config as ClientConfig};
use libp2p::autonat::v2::server::Behaviour as Server;
use libp2p::swarm::{FromSwarm, NetworkBehaviour, NewExternalAddrCandidate, SwarmEvent};
use libp2p::{Multiaddr, PeerId, Swarm, identify, identity};

const PATIENCE: Duration = Duration::from_secs(20);

/// Identify rides beside both behaviours because that is how the
/// client learns a peer speaks the protocol: its handler reacts to
/// `RemoteProtocolsChange`, which only Identify produces.
fn identify(k: &identity::Keypair) -> identify::Behaviour {
    identify::Behaviour::new(identify::Config::new(
        "/interweave-autonat-outcome-test/1".to_owned(),
        k.public(),
    ))
}

#[derive(NetworkBehaviour)]
struct RawClient {
    identify: identify::Behaviour,
    client: Client,
}

#[derive(NetworkBehaviour)]
struct RawServer {
    identify: identify::Behaviour,
    server: Server,
}

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

/// A raw client that sweeps every 200 ms, so the test does not wait for
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

fn server() -> Swarm<RawServer> {
    swarm(|k| RawServer {
        identify: identify(k),
        server: Server::default(),
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

async fn connect(client: &mut Swarm<RawClient>, server: &mut Swarm<RawServer>, at: &Multiaddr) {
    let id: PeerId = *server.local_peer_id();
    client
        .dial(at.clone().with_p2p(id).expect("a peer address"))
        .expect("dials");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut established = false;
    while !established {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "not connected within {PATIENCE:?}");
        tokio::select! {
            event = client.select_next_some() => {
                established = matches!(event, SwarmEvent::ConnectionEstablished { .. });
            }
            _ = server.select_next_some() => {}
            () = tokio::time::sleep(remaining) => {}
        }
    }
}

/// Drive both sides until the client reports a probe outcome FOR
/// `addr`, and return that event whole; an outcome for another address
/// is kept in `others` for the call that asks for it.
///
/// Keyed on the address, because two candidates are untested at the
/// first sweep -- the injected one and the one Identify handed the
/// client on its own -- and the crate probes both in one tick; which
/// outcome arrives first is a race between a refused connect and a
/// completed dial-back, not something a test may rely on. And KEPT,
/// not dropped: a `Received` candidate is never re-swept, so an
/// outcome discarded while waiting for the other one would never
/// come again. Review findings on PR #90, rounds 1 and 2.
async fn outcome(
    client: &mut Swarm<RawClient>,
    server: &mut Swarm<RawServer>,
    addr: &Multiaddr,
    others: &mut Vec<client::Event>,
) -> client::Event {
    if let Some(i) = others.iter().position(|e| e.tested_addr == *addr) {
        return others.remove(i);
    }
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "no probe outcome for {addr} within {PATIENCE:?}"
        );
        tokio::select! {
            event = client.select_next_some() => {
                if let SwarmEvent::Behaviour(RawClientEvent::Client(e)) = event {
                    if e.tested_addr == *addr {
                        return e;
                    }
                    others.push(e);
                }
            }
            _ = server.select_next_some() => {}
            () = tokio::time::sleep(remaining) => {}
        }
    }
}

#[tokio::test]
async fn a_real_dial_back_failure_is_an_unreachable_outcome_and_a_real_success_a_reachable_one() {
    let mut server = server();
    let server_addr = bound(&mut server).await;
    let mut client = client();
    let client_addr = bound(&mut client).await;
    connect(&mut client, &mut server, &server_addr).await;

    // THE FAILURE the classifier exists for: a candidate nothing
    // listens on, injected. The server's dial-back is refused by the
    // kernel, the server answers `E_DIAL_ERROR`, and the client's event
    // carries the crate's public `Error` -- whatever text that displays.
    let dead: Multiaddr = "/ip4/127.0.0.1/tcp/1".parse().expect("a literal");
    candidate(&mut client, &dead);
    let mut others = Vec::new();
    let failed = outcome(&mut client, &mut server, &dead, &mut others).await;
    let text = failed
        .result
        .as_ref()
        .expect_err("nothing listens there")
        .to_string();
    assert!(
        DIAL_BACK_FAILURE_TEXTS.contains(&text.as_str()),
        "the public error displays a text the classifier knows: {text:?}"
    );
    assert_eq!(
        classify_outcome(&failed.result),
        Some(ProbeOutcome::Unreachable),
        "a server's failure vote is an outcome"
    );

    // THE CONTROL: the client's own listener, dialled back to and
    // confirmed. Not injected -- the client dials the server from its
    // listen port (`PortUse::Reuse`), so the server observes it AT that
    // address and Identify hands the client `client_addr` as a
    // candidate before the first sweep. Reachable, and a genuine
    // `Pending` was crossed to get there, which is ADR-0051 Decision
    // 3's owed test.
    let confirmed = outcome(&mut client, &mut server, &client_addr, &mut others).await;
    assert_eq!(
        classify_outcome(&confirmed.result),
        Some(ProbeOutcome::Reachable)
    );

    // ADR-0051's mechanism, on the wire: `retest` returns the received
    // candidate to the sweep and the server serves it again.
    assert!(
        client.behaviour_mut().client.retest(&client_addr),
        "received -> untested is the patch"
    );
    let again = outcome(&mut client, &mut server, &client_addr, &mut others).await;
    assert!(
        others.is_empty(),
        "every outcome the wire produced was asked for"
    );
    assert_eq!(
        classify_outcome(&again.result),
        Some(ProbeOutcome::Reachable)
    );
}
