// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The AutoNAT v2 SERVER role through the production runtime, over
//! real sockets (`AUTONAT.md` §7; CLAUDE.md §1 route 1).
//!
//! # What runs
//!
//! The SUBJECT is a `SwarmRuntime` with `autonat_server` configured --
//! the first profile in this repository that serves probes -- and a
//! trust policy naming one CLIENT as infrastructure-only. The client is
//! a raw Swarm carrying the vendored AutoNAT client and Identify: it
//! dials the subject, learns from Identify that the subject speaks
//! `/libp2p/autonat/2/dial-request`, and asks it to dial back to the
//! candidate Identify handed it -- its own loopback listener, which the
//! subject's target rule refuses. A BYSTANDER in no trust set is the
//! control for the service policy.
//!
//! # What is measured
//!
//! - The client's inbound is RETAINED with the server on: an
//!   infrastructure-only inbound is otherwise established-then-closed,
//!   and the server role is what widens the inbound arm (`dialing.rs`).
//!   The control is the same client against a subject with the server
//!   OFF, whose inbound is closed at establishment.
//! - On that retained inbound the subject offers exactly Identify and
//!   the dial-request protocol: the infrastructure service of the class
//!   gate, and nothing of the data plane.
//! - The probe is refused BY NAME with the address the crate named, the
//!   client hears a probe failure, NO connection reaches the client and
//!   NO dial reaches the subject's Swarm.
//! - The outbound gate took the refused dial-back's ticket back: the
//!   refusal is counted as a release, so no pending-dial slot leaks
//!   (PR #91; the reason the server field may sit after the gate).
//! - The bystander is established and then closed, under either
//!   origin.
//!
//! # What loopback cannot show
//!
//! A dial-back that is MADE: §7 refuses every loopback target, so the
//! subject never dials here. The crate-level harness beside the crate
//! (`crates/transport/libp2p/tests/autonat_outcome_wire.rs`) makes one
//! with the bare vendored server; a dial-back through the substrate
//! needs a public candidate and is SPIKE-004 phase B's.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::time::Duration;

use futures::StreamExt as _;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::autonat_driver::DIAL_BACK_FAILURE_TEXTS;
use interweave_transport_libp2p::runtime::autonat_server_driver::AutonatServerSettings;
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::autonat::v2::client::{self as autonat_client, Config as ClientConfig};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent as Libp2pSwarmEvent};
use libp2p::{Multiaddr, PeerId, identify, identity};

const PATIENCE: Duration = Duration::from_secs(20);

/// How long a RETAINED connection must survive to count as retained,
/// and how long a dial-back that must not come is waited for.
const WINDOW: Duration = Duration::from_secs(3);

/// What a retained infrastructure inbound is offered with the server
/// on: Identify, and the server's half of AutoNAT. Nothing of the data
/// plane, and not the client's half either, since this subject runs no
/// client.
const OFFERED_TO_A_CLIENT: &[&str] = &[
    "/ipfs/id/1.0.0",
    "/ipfs/id/push/1.0.0",
    "/libp2p/autonat/2/dial-request",
];

#[derive(NetworkBehaviour)]
struct ClientBehaviour {
    identify: identify::Behaviour,
    autonat: autonat_client::Behaviour,
}

/// A raw AutoNAT client beside Identify, sweeping every 200 ms so the
/// test does not wait for the crate's five-second tick.
fn client(keys: identity::Keypair) -> libp2p::Swarm<ClientBehaviour> {
    libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the subject uses")
        .with_behaviour(|k| ClientBehaviour {
            identify: identify::Behaviour::new(identify::Config::new(
                "/interweave-autonat-test-client/1".to_owned(),
                k.public(),
            )),
            autonat: autonat_client::Behaviour::new(
                rand::rngs::OsRng,
                ClientConfig::default().with_probe_interval(Duration::from_millis(200)),
            ),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build()
}

/// A bare Identify-only peer in no trust set. The control.
fn bystander(keys: identity::Keypair) -> libp2p::Swarm<identify::Behaviour> {
    libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the subject uses")
        .with_behaviour(|k| {
            identify::Behaviour::new(identify::Config::new(
                "/interweave-autonat-test-bystander/1".to_owned(),
                k.public(),
            ))
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build()
}

async fn bound<B: NetworkBehaviour>(swarm: &mut libp2p::Swarm<B>) -> Multiaddr
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
            Ok(Libp2pSwarmEvent::NewListenAddr { address, .. }) => return address,
            Ok(_) => {}
            Err(_) => panic!("the listener never bound"),
        }
    }
}

fn identity_of(keys: &identity::Keypair) -> TransportIdentity {
    TransportIdentity::parse(keys.public().to_peer_id().to_base58()).expect("canonical")
}

/// Trust: nobody on the data plane; `infra` as infrastructure only.
fn infrastructure_only(infra: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(std::iter::empty()).expect("an empty allowlist"),
        InfrastructureSet::new(infra.iter().map(|p| (*p).clone())).expect("a small set"),
    )
}

/// A subject serving probes, with a per-client budget wide enough that
/// this file's probes are judged on their target and not on the rate.
fn serving() -> SubstrateConfig {
    SubstrateConfig {
        autonat_server: Some(AutonatServerSettings {
            max_probes_per_peer_per_minute: 10,
            ..AutonatServerSettings::default()
        }),
        ..SubstrateConfig::default()
    }
}

/// What the client observed of one exchange with the subject.
#[derive(Debug, Default)]
struct ClientView {
    /// The protocols the subject advertised on the client's outbound.
    offered: Option<BTreeSet<String>>,
    /// The client's outbound connection to the subject, and whether it
    /// was closed.
    outbound: Option<libp2p::swarm::ConnectionId>,
    outbound_closed: bool,
    /// Whether any connection ARRIVED at the client (a dial-back would).
    inbound_arrived: bool,
    /// The first probe outcome the client reported.
    outcome: Option<Result<(), String>>,
}

impl ClientView {
    fn absorb(&mut self, event: Libp2pSwarmEvent<ClientBehaviourEvent>) {
        match event {
            Libp2pSwarmEvent::ConnectionEstablished {
                connection_id,
                endpoint,
                ..
            } => {
                if endpoint.is_dialer() {
                    self.outbound = Some(connection_id);
                } else {
                    self.inbound_arrived = true;
                }
            }
            Libp2pSwarmEvent::ConnectionClosed { connection_id, .. }
                if Some(connection_id) == self.outbound =>
            {
                self.outbound_closed = true;
            }
            Libp2pSwarmEvent::Behaviour(ClientBehaviourEvent::Identify(
                identify::Event::Received {
                    connection_id,
                    info,
                    ..
                },
            )) if Some(connection_id) == self.outbound => {
                self.offered = Some(info.protocols.iter().map(ToString::to_string).collect());
            }
            Libp2pSwarmEvent::Behaviour(ClientBehaviourEvent::Autonat(autonat_client::Event {
                result,
                ..
            })) if self.outcome.is_none() => {
                self.outcome = Some(result.map_err(|e| e.to_string()));
            }
            _ => {}
        }
    }
}

/// Drive the subject and the client until `done` says so or `limit`
/// elapses, collecting the subject's events and the client's view.
async fn drive<F>(
    subject: &mut SwarmRuntime,
    client_swarm: &mut libp2p::Swarm<ClientBehaviour>,
    view: &mut ClientView,
    subject_events: &mut Vec<SwarmEvent>,
    limit: Duration,
    mut done: F,
) -> bool
where
    F: FnMut(&ClientView, &[SwarmEvent]) -> bool,
{
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if done(view, subject_events) {
            return true;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        tokio::select! {
            event = subject.next_event() => {
                subject_events.push(event.expect("the runtime is alive"));
            }
            event = client_swarm.select_next_some() => view.absorb(event),
            () = tokio::time::sleep(remaining) => return false,
        }
    }
}

#[tokio::test]
async fn an_infrastructure_only_client_is_served_and_its_loopback_target_is_refused_before_any_socket()
 {
    let client_keys = identity::Keypair::generate_ed25519();
    let client_peer = identity_of(&client_keys);
    let mut client_swarm = client(client_keys);
    let client_addr = bound(&mut client_swarm).await;

    let subject_id = ProfileIdentity::generate();
    let mut subject =
        SwarmRuntime::start(&subject_id, serving(), infrastructure_only(&[&client_peer]))
            .expect("the runtime starts");
    let subject_addr = subject
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a listen address"))
        .await
        .expect("the subject listens");
    let subject_pid: PeerId = subject_id
        .transport_identity()
        .expect("peer id")
        .as_str()
        .parse()
        .expect("a libp2p identity");

    // The client dials the subject. On the subject that is an inbound
    // from an infrastructure-only peer, and with the server on it is
    // RETAINED under `AutonatProbe`.
    client_swarm
        .dial(
            subject_addr
                .clone()
                .with_p2p(subject_pid)
                .expect("a peer address"),
        )
        .expect("dial accepted");
    let mut view = ClientView::default();
    let mut events = Vec::new();

    // Identify on the retained inbound: exactly the infrastructure
    // service, nothing of the data plane.
    assert!(
        drive(
            &mut subject,
            &mut client_swarm,
            &mut view,
            &mut events,
            PATIENCE,
            |v, _| v.offered.is_some() || v.outbound_closed
        )
        .await,
        "the subject never identified itself to the client"
    );
    assert!(
        !view.outbound_closed,
        "the client's inbound was closed: the server role did not retain it"
    );
    let expected: BTreeSet<String> = OFFERED_TO_A_CLIENT
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(
        view.offered.clone().expect("identified"),
        expected,
        "a retained infrastructure inbound is offered exactly Identify and the server's \
         dial-request protocol"
    );

    // THE PROBE. Identify handed the client its own loopback listener as
    // a candidate (it dialled from its listen port); the client asks
    // the subject to dial it back; the subject refuses on the target.
    assert!(
        drive(
            &mut subject,
            &mut client_swarm,
            &mut view,
            &mut events,
            PATIENCE,
            |v, _| v.outcome.is_some()
        )
        .await,
        "the client never heard a probe outcome"
    );
    let failure = view
        .outcome
        .clone()
        .expect("heard")
        .expect_err("a loopback target is refused under section 7");
    assert!(
        DIAL_BACK_FAILURE_TEXTS.contains(&failure.as_str()),
        "the client hears a probe failure the adapter classifies: {failure}"
    );
    let refusal = events.iter().find(|e| {
        matches!(
            e,
            SwarmEvent::AutonatProbeRefused { client, .. } if *client == client_peer
        )
    });
    assert_eq!(
        refusal,
        Some(&SwarmEvent::AutonatProbeRefused {
            client: client_peer.clone(),
            address: Some(client_addr.to_string()),
            reason: "refused_not_global",
        }),
        "refused by name, with the address the crate named: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SwarmEvent::AutonatProbeServed { .. })),
        "a refused request is not also reported as served"
    );

    // NOTHING WAS DIALLED. No connection arrived at the client in the
    // window, the client's own connection survived it, and the gate
    // counted the refused dial-back as a RELEASE -- the ticket it had
    // deposited was taken back, so no pending-dial slot leaked.
    let _ = drive(
        &mut subject,
        &mut client_swarm,
        &mut view,
        &mut events,
        WINDOW,
        |_, _| false,
    )
    .await;
    assert!(!view.inbound_arrived, "no dial-back reached the client");
    assert!(
        !view.outbound_closed,
        "and the client's connection was retained through the window"
    );
    let refusals = subject.dial_refusals();
    assert_eq!(
        refusals.released_after_admission(),
        1,
        "the gate admitted the dial-back and took its ticket back on the wrapper's denial"
    );
    assert_eq!(
        refusals.total(),
        1,
        "and refused nothing else: the dial-back was admitted, not denied, by the policy"
    );
}

#[tokio::test]
async fn a_peer_in_no_trust_set_is_established_and_then_closed_with_the_server_on() {
    // THE CONTROL for the service policy: an `Unauthorized` inbound is
    // refused under `AutonatProbe` as under `Manual`.
    let bystander_keys = identity::Keypair::generate_ed25519();
    let mut bystander = bystander(bystander_keys);
    let subject_id = ProfileIdentity::generate();
    let mut subject = SwarmRuntime::start(&subject_id, serving(), infrastructure_only(&[]))
        .expect("the runtime starts");
    let subject_addr = subject
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a listen address"))
        .await
        .expect("the subject listens");
    let subject_pid: PeerId = subject_id
        .transport_identity()
        .expect("peer id")
        .as_str()
        .parse()
        .expect("a libp2p identity");
    bystander
        .dial(subject_addr.with_p2p(subject_pid).expect("a peer address"))
        .expect("dial accepted");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut established = false;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the bystander's connection was never closed"
        );
        tokio::select! {
            event = bystander.select_next_some() => match event {
                Libp2pSwarmEvent::ConnectionEstablished { .. } => established = true,
                Libp2pSwarmEvent::ConnectionClosed { .. } => {
                    assert!(established, "closed after establishing, not refused before");
                    break;
                }
                _ => {}
            },
            event = subject.next_event() => { let _ = event; }
            () = tokio::time::sleep(remaining) => panic!("the bystander's connection was never closed"),
        }
    }
}

#[tokio::test]
async fn with_the_server_off_an_infrastructure_only_inbound_is_still_closed() {
    // THE CONTROL for the widened inbound arm: the same client, the
    // same trust, no server -- established and then closed, as before
    // this step.
    let client_keys = identity::Keypair::generate_ed25519();
    let client_peer = identity_of(&client_keys);
    let mut client_swarm = client(client_keys);
    let _ = bound(&mut client_swarm).await;
    let subject_id = ProfileIdentity::generate();
    let mut subject = SwarmRuntime::start(
        &subject_id,
        SubstrateConfig::default(),
        infrastructure_only(&[&client_peer]),
    )
    .expect("the runtime starts");
    let subject_addr = subject
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a listen address"))
        .await
        .expect("the subject listens");
    let subject_pid: PeerId = subject_id
        .transport_identity()
        .expect("peer id")
        .as_str()
        .parse()
        .expect("a libp2p identity");
    client_swarm
        .dial(subject_addr.with_p2p(subject_pid).expect("a peer address"))
        .expect("dial accepted");
    let mut view = ClientView::default();
    let mut events = Vec::new();
    assert!(
        drive(
            &mut subject,
            &mut client_swarm,
            &mut view,
            &mut events,
            PATIENCE,
            |v, _| v.outbound_closed
        )
        .await,
        "with the server off, an infrastructure-only inbound is closed at establishment"
    );
    assert!(
        view.outbound.is_some(),
        "closed after establishing, not refused before"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            SwarmEvent::AutonatProbeRefused { .. } | SwarmEvent::AutonatProbeServed { .. }
        )),
        "and no server event exists to emit"
    );
}
