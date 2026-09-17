// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 11 step 3: the AutoNAT v2 client's two routes to an
//! infrastructure-only connection, over real sockets.
//!
//! CLAUDE.md §1 names three routes to a DIALLED or RETAINED
//! infrastructure-only connection and says step 3 reaches routes 2
//! and 3. This is the test that they are reached by their own
//! mechanism and by nothing else:
//!
//! - **Route 2** — a static AutoNAT server this profile is not
//!   connected to is reached by `attempt_dial` under
//!   `DialOrigin::AutonatProbe`, admitted toward an infrastructure-only
//!   peer, and the connection is announced. Nothing else in the tree
//!   passes that origin, so `Connected { peer: server }` on a profile
//!   whose only relation to the server is `static_servers` is that
//!   route and no other.
//! - **Route 3** — an INBOUND from a peer the adapter holds as a server
//!   is retained rather than established-then-closed, and what it is
//!   offered is Identify and the client's dial-back protocol and
//!   nothing else. The CONTROL runs in the same test: an
//!   infrastructure-only peer that is NOT a server -- never dialled,
//!   never offered -- dials the same subject and is established and
//!   then closed, exactly as `advertised_protocol_set.rs` pins for
//!   every such peer before this step.
//!
//! # What loopback cannot show, and where it is owed
//!
//! No probe is issued here and no dial-back arrives. `AUTONAT.md` §6
//! refuses a loopback address as a candidate, `ScopedCandidates`
//! enforces that before the crate sees one, and a test that widened
//! the rule to make the wire move would be proving a lookalike. So the
//! server's dial-back -- the one inbound route 3 exists for -- arrives
//! here as the server's own dial instead, which is the same arm under
//! the same condition ("is a server", not "has a probe outstanding":
//! the crate emits no probe-start event, so the arm cannot be keyed on
//! one). The probe -> outcome -> verdict -> advertised-address seam is
//! proven in `autonat_driver.rs` over a real Swarm with a constructible
//! success; the wire from a real probe to a real dial-back needs a
//! public candidate and is SPIKE-004 phase B's, recorded as such in
//! the plan's step-3 note.
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::time::Duration;

use futures::StreamExt as _;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::autonat_driver::{AutonatClientSettings, StaticServer};
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;
use libp2p::{Multiaddr, PeerId, identify, identity};

/// How long a loopback exchange may take before the test fails.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long a RETAINED connection must survive to count as retained.
///
/// An infrastructure-only inbound the subject refuses is closed at
/// establishment, well inside this; one it keeps has no reason to close
/// before its idle timeout (60 s by default, far beyond it).
const RETENTION_WINDOW: Duration = Duration::from_secs(3);

/// What a retained infrastructure inbound is offered: Identify, and the
/// client's half of AutoNAT. Nothing of the data plane.
const OFFERED_TO_A_SERVER: &[&str] = &[
    "/ipfs/id/1.0.0",
    "/ipfs/id/push/1.0.0",
    "/libp2p/autonat/2/dial-back",
];

#[derive(libp2p::swarm::NetworkBehaviour)]
struct ServerBehaviour {
    identify: identify::Behaviour,
    autonat: libp2p::autonat::v2::server::Behaviour,
}

/// A bare AutoNAT v2 server: the vendored server behaviour beside
/// Identify, which is how a client learns it speaks the protocol.
fn server(keys: identity::Keypair) -> libp2p::Swarm<ServerBehaviour> {
    libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the subject uses")
        .with_behaviour(|k| ServerBehaviour {
            identify: identify::Behaviour::new(identify::Config::new(
                "/interweave-autonat-test-server/1".to_owned(),
                k.public(),
            )),
            autonat: libp2p::autonat::v2::server::Behaviour::default(),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build()
}

/// A bare Identify-only peer: infrastructure-only to the subject, and
/// never a server. The control.
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

async fn bound<B: libp2p::swarm::NetworkBehaviour>(swarm: &mut libp2p::Swarm<B>) -> Multiaddr
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

/// Wait for one event of the subject matching `pred`, driving the
/// given bare swarms meanwhile so their side of every exchange moves.
async fn subject_event<F>(
    subject: &mut SwarmRuntime,
    server: &mut libp2p::Swarm<ServerBehaviour>,
    what: &str,
    mut pred: F,
) -> SwarmEvent
where
    F: FnMut(&SwarmEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for {what}");
        tokio::select! {
            event = subject.next_event() => {
                let event = event.expect("the runtime is alive");
                if pred(&event) {
                    return event;
                }
            }
            _ = server.select_next_some() => {}
            () = tokio::time::sleep(remaining) => panic!("timed out waiting for {what}"),
        }
    }
}

#[tokio::test]
async fn a_static_server_is_dialled_under_autonat_probe_and_its_own_inbound_is_retained_offered_only_control_protocols()
 {
    // THE SERVER, listening first so the subject has an address to
    // configure.
    let server_keys = identity::Keypair::generate_ed25519();
    let server_peer = identity_of(&server_keys);
    let mut server = server(server_keys.clone());
    let server_addr = bound(&mut server).await;

    // THE BYSTANDER: infrastructure-only like the server, but not a
    // server. The control for route 3.
    let bystander_keys = identity::Keypair::generate_ed25519();
    let bystander_peer = identity_of(&bystander_keys);
    let mut bystander = bystander(bystander_keys);

    // THE SUBJECT: the production runtime, client configured with the
    // server as its one static server, both peers infrastructure-only.
    let subject_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let config = SubstrateConfig {
        autonat_client: Some(AutonatClientSettings {
            static_servers: vec![StaticServer {
                peer: server_peer.clone(),
                address: format!("{server_addr}/p2p/{}", server_peer.as_str()),
            }],
            use_authorized_identify_servers: false,
            required_distinct_successes: 2,
            success_evidence_ttl_ms: 15 * 60 * 1000,
            refresh_interval_ms: 5 * 60 * 1000,
            max_candidate_addresses_per_cycle: 4,
        }),
        ..SubstrateConfig::default()
    };
    let mut subject = SwarmRuntime::start(
        &subject_id,
        config,
        infrastructure_only(&[&server_peer, &bystander_peer]),
    )
    .expect("the runtime starts");
    let subject_addr = subject
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a listen address"))
        .await
        .expect("the subject listens");

    // ROUTE 2. Nobody asked the subject to dial; its adapter dials the
    // static server on its tick under `AutonatProbe`, and the gate
    // admits it toward an infrastructure-only peer. `Connected` is the
    // admitted, announced connection.
    let _ = subject_event(
        &mut subject,
        &mut server,
        "the static server to be dialled",
        |e| matches!(e, SwarmEvent::Connected { peer } if *peer == server_peer),
    )
    .await;

    // The subject learns the server's protocols through Identify on
    // that outbound connection and offers it to the manager. Nothing
    // announces that, so route 3 below is also the assay that it
    // happened: an inbound from a peer NOT held as a server is closed.
    // Give Identify a moment to complete both ways.
    let _ = subject_event(
        &mut subject,
        &mut server,
        "identify to settle",
        |e| matches!(e, SwarmEvent::Identified { peer, .. } if *peer == server_peer),
    )
    .await;

    // ROUTE 3. The server dials the subject. On the subject that is an
    // inbound from an infrastructure-only peer -- refused outright
    // before this step -- and it is retained, because the peer is a
    // server. What the server is told the subject offers on it is
    // Identify and the dial-back protocol, and nothing else.
    let subject_pid: PeerId = subject_peer.as_str().parse().expect("a libp2p identity");
    // `PeerCondition::Always`, because the server is ALREADY connected
    // to the subject on the subject's own outbound -- which is exactly
    // the situation a real dial-back is in, since a probe is sent over
    // that connection and answered with a second one.
    server
        .dial(
            libp2p::swarm::dial_opts::DialOpts::peer_id(subject_pid)
                .condition(libp2p::swarm::dial_opts::PeerCondition::Always)
                .addresses(vec![subject_addr.clone()])
                .build(),
        )
        .expect("dial accepted");
    let mut offered: Option<BTreeSet<String>> = None;
    let mut inbound_connection: Option<libp2p::swarm::ConnectionId> = None;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while offered.is_none() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the server never identified the subject on its own dial"
        );
        tokio::select! {
            event = server.select_next_some() => match event {
                Libp2pSwarmEvent::ConnectionEstablished { connection_id, endpoint, .. }
                    if endpoint.is_dialer() =>
                {
                    inbound_connection = Some(connection_id);
                }
                Libp2pSwarmEvent::Behaviour(ServerBehaviourEvent::Identify(
                    identify::Event::Received { connection_id, info, .. },
                )) if Some(connection_id) == inbound_connection => {
                    offered = Some(info.protocols.iter().map(ToString::to_string).collect());
                }
                Libp2pSwarmEvent::ConnectionClosed { connection_id, .. }
                    if Some(connection_id) == inbound_connection =>
                {
                    panic!("the subject closed the server's inbound before identifying: route 3 is not reached");
                }
                _ => {}
            },
            event = subject.next_event() => { let _ = event; }
            () = tokio::time::sleep(remaining) => panic!("the server never identified the subject"),
        }
    }
    let expected: BTreeSet<String> = OFFERED_TO_A_SERVER
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(
        offered.expect("identified"),
        expected,
        "a retained infrastructure inbound is offered exactly Identify and the client's \
         dial-back protocol: more means a data-plane behaviour reached an infrastructure \
         connection, fewer means the client is not there"
    );

    // RETAINED: the server's connection survives the window.
    let inbound_connection = inbound_connection.expect("established");
    let until = tokio::time::Instant::now() + RETENTION_WINDOW;
    loop {
        let remaining = until.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            event = server.select_next_some() => {
                assert!(
                    !matches!(event, Libp2pSwarmEvent::ConnectionClosed { connection_id, .. } if connection_id == inbound_connection),
                    "the subject closed a server's inbound inside the retention window"
                );
            }
            event = subject.next_event() => { let _ = event; }
            () = tokio::time::sleep(remaining) => break,
        }
    }

    // THE CONTROL. The bystander is infrastructure-only too, but the
    // subject never dialled it and holds it as no server: its inbound
    // is established and then closed, the pre-step-3 behaviour every
    // other such peer still gets.
    bystander
        .dial(
            libp2p::swarm::dial_opts::DialOpts::peer_id(subject_pid)
                .addresses(vec![subject_addr])
                .build(),
        )
        .expect("dial accepted");
    let mut established = false;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the bystander's inbound was not closed by the subject"
        );
        tokio::select! {
            event = bystander.select_next_some() => match event {
                Libp2pSwarmEvent::ConnectionEstablished { .. } => established = true,
                Libp2pSwarmEvent::ConnectionClosed { .. } => {
                    assert!(established, "closed before it was established");
                    break;
                }
                _ => {}
            },
            _ = server.select_next_some() => {}
            event = subject.next_event() => { let _ = event; }
            () = tokio::time::sleep(remaining) => panic!("the bystander's inbound was not closed"),
        }
    }

    subject.shutdown().await.expect("stops");
}

/// A static server that refuses at the socket is re-dialled by the
/// ADAPTER alone. The reconnect scheduler sees the failure too -- a
/// failed admitted dial schedules a retry whatever its origin -- but
/// dials under its own origin, which the gate refuses for an
/// infrastructure-only peer; it now walks past that class rather than
/// reporting a refusal nobody can act on, once per failure, beside the
/// adapter's own re-dial. The CONTROL is the adapter's dial itself: the
/// kernel refusal it earns is reported as a plain `DialFailed`, so the
/// window is proven live before the absence is asserted. Its mirror,
/// `stage5_dial_admission::a_revoked_peer_is_not_retried`, pins that a
/// REVOKED peer's scheduled retry is still refused and reported.
#[tokio::test(start_paused = true)]
async fn a_static_server_that_refuses_at_the_socket_is_not_also_retried_by_the_reconnect_scheduler()
{
    let server_keys = identity::Keypair::generate_ed25519();
    let server_peer = identity_of(&server_keys);
    let subject_id = ProfileIdentity::generate();
    let config = SubstrateConfig {
        autonat_client: Some(AutonatClientSettings {
            static_servers: vec![StaticServer {
                peer: server_peer.clone(),
                // TCP port 1: refused by the kernel, so the dial is
                // ADMITTED (a ticket, an OutgoingConnectionError, a
                // scheduled retry) and then fails.
                address: format!("/ip4/127.0.0.1/tcp/1/p2p/{}", server_peer.as_str()),
            }],
            use_authorized_identify_servers: false,
            required_distinct_successes: 2,
            success_evidence_ttl_ms: 15 * 60 * 1000,
            refresh_interval_ms: 5 * 60 * 1000,
            max_candidate_addresses_per_cycle: 4,
        }),
        ..SubstrateConfig::default()
    };
    let mut subject =
        SwarmRuntime::start(&subject_id, config, infrastructure_only(&[&server_peer]))
            .expect("the runtime starts");
    // PAST THE GATE'S BASE RETRY DELAY. A failed admitted dial is
    // rescheduled 30 s out, so a five-second window never sees the
    // scheduler look at it and the assertion below is vacuous -- the
    // first draft of this test passed with the walk-past removed.
    // Tokio's paused clock makes forty virtual seconds cost nothing
    // while the kernel refusal on the real socket stays real.
    let window = tokio::time::Instant::now() + Duration::from_secs(40);
    let mut adapter_dial_failed = false;
    loop {
        let remaining = window.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, subject.next_event()).await {
            Ok(Some(SwarmEvent::DialFailed { peer, detail })) => {
                assert_eq!(peer.as_ref(), Some(&server_peer));
                assert!(
                    !detail.contains("scheduled retry"),
                    "the reconnect scheduler dialled an infrastructure-only server under its own \
                     origin and was refused: {detail}"
                );
                adapter_dial_failed = true;
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("the runtime stopped"),
            Err(_) => break,
        }
    }
    assert!(
        adapter_dial_failed,
        "the control: the adapter's own dial to the refusing port was reported"
    );
    subject.shutdown().await.expect("stops");
}
