// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The conformance suite against the direct in-process binding (plan §15:
//! "Run LocalDataSession conformance first against the direct in-process
//! binding"): two runtimes composed from profiles, connected over real
//! sockets on the host's private address, each check run against their
//! `sessions()` bindings.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_local_client_api::{
    AdminCapability, DataSessionBinding, DataSessionPort, LocalSessionEvent, SessionEvent,
};
use interweave_local_client_conformance_tests as suite;
use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    ChannelId, DirectDestination, EndpointId, MessageId, TransportError, TransportEvent,
    TransportIdentity, TransportRuntime,
};
use interweave_transport_composition::{ComposedRuntime, CompositionOptions, InProcessBinding};

/// The endpoint queue bound both nodes run with: small, so the bound is
/// reachable without the ingress rate limits deciding first.
const QUEUE_BOUND: usize = 2;

fn profile(trusted: &TransportIdentity, statics: &[String]) -> ProfileConfig {
    let peers: Vec<String> = statics.iter().map(|s| format!("\"{s}\"")).collect();
    let doc = format!(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: [\"{}\"]
endpoints:
  default_direct_endpoint: human
  entries:
    - id: human
      enabled: true
      advertise: false
    - id: agent
      enabled: true
      advertise: false
channels:
  desired: [general]
discovery:
  providers:
    - type: static-bootstrap
      enabled: true
      priority: 10
      config:
        peers: [{}]
",
        trusted.as_str(),
        peers.join(", ")
    );
    serde_norway::from_str(&doc).expect("the document parses")
}

fn id() -> (ProfileIdentity, TransportIdentity) {
    let identity = ProfileIdentity::generate();
    let peer = identity.transport_identity().expect("peer id");
    (identity, peer)
}

async fn wait_connected(runtime: &mut ComposedRuntime, peer: &TransportIdentity) {
    let deadline = tokio::time::Instant::now() + suite::PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(TransportEvent::PeerConnected { peer: got, .. })) if &got == peer => return,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the runtime stopped"),
            Err(_) => panic!("no PeerConnected within {:?}", suite::PATIENCE),
        }
    }
}

/// Two composed runtimes, A dialling B through its static entry, both
/// seeing the connection.
struct Pair {
    a: ComposedRuntime,
    b: ComposedRuntime,
    a_peer: TransportIdentity,
    b_peer: TransportIdentity,
}

impl Pair {
    async fn start() -> Self {
        let ip = interweave_test_support::net::require_private_interface_v4();
        let options = CompositionOptions {
            listen: vec![format!("/ip4/{ip}/tcp/0")],
            queue_bound: QUEUE_BOUND,
            ..CompositionOptions::default()
        };
        let (a_id, a_peer) = id();
        let (b_id, b_peer) = id();
        let mut b = ComposedRuntime::start(&b_id, &profile(&a_peer, &[]), options.clone())
            .await
            .expect("b composes");
        let b_addr = format!("{}/p2p/{}", b.listening()[0], b_peer.as_str());
        let mut a = ComposedRuntime::start(&a_id, &profile(&b_peer, &[b_addr]), options)
            .await
            .expect("a composes");
        wait_connected(&mut a, &b_peer).await;
        wait_connected(&mut b, &a_peer).await;
        Self {
            a,
            b,
            a_peer,
            b_peer,
        }
    }

    fn bindings(&self) -> (InProcessBinding, InProcessBinding) {
        (self.a.sessions(), self.b.sessions())
    }

    async fn stop(self) {
        self.a.shutdown().await.expect("a stops");
        self.b.shutdown().await.expect("b stops");
    }
}

fn human() -> EndpointId {
    EndpointId::parse("human").expect("valid")
}

fn agent() -> EndpointId {
    EndpointId::parse("agent").expect("valid")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_1_the_source_endpoint_is_the_senders_lease() {
    let pair = Pair::start().await;
    let (a, b) = pair.bindings();
    suite::the_source_endpoint_is_the_senders_lease(
        &a,
        &b,
        &pair.a_peer,
        &pair.b_peer,
        &agent(),
        &human(),
    )
    .await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn items_2_and_5_a_lease_is_exclusive_and_released_on_close() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    suite::a_lease_is_exclusive_and_released_on_close(&a, &human()).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn items_3_and_6_the_queue_is_bounded_and_acceptance_follows_admission() {
    let pair = Pair::start().await;
    let (a, b) = pair.bindings();
    suite::the_queue_is_bounded_and_acceptance_follows_admission(&a, &b, &pair.b_peer, &human())
        .await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_4_local_refusals_map_exactly() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    suite::local_refusals_map_exactly(
        &a,
        &pair.b_peer,
        &EndpointId::parse("nowhere").expect("valid"),
    )
    .await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_8_nothing_is_kept_for_an_unleased_endpoint() {
    let pair = Pair::start().await;
    let (a, b) = pair.bindings();
    suite::nothing_is_kept_for_an_unleased_endpoint(&a, &b, &pair.b_peer, &human()).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcast_reaches_joined_sessions_only() {
    let pair = Pair::start().await;
    let (a, b) = pair.bindings();
    suite::broadcast_reaches_joined_sessions_only(
        &a,
        &b,
        &pair.a_peer,
        &ChannelId::parse("general").expect("valid"),
    )
    .await;
    pair.stop().await;
}

/// Item 7's runtime half, for this binding: the admin facade is its own
/// authority object -- built from the binding, holding no lease, refused
/// without `admin.endpoints` -- and a revocation it makes reaches the
/// holder as `EndpointLeaseChanged` naming the epoch that ended, after
/// which the holder cannot send on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_7_administration_is_a_separate_authority() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    let holder = a.open(suite::full(Some(&human()))).await.expect("leases");
    let epoch = holder
        .session()
        .endpoint_lease()
        .expect("leased")
        .epoch
        .clone();

    let powerless = a.admin([AdminCapability::Shutdown]).expect("a port");
    assert_eq!(
        powerless.revoke_endpoint(human()).await,
        Err(TransportError::CapabilityDenied),
        "no admin.endpoints, no revocation"
    );
    let admin = a.admin([AdminCapability::Endpoints]).expect("a port");
    assert!(
        admin.port().endpoint_lease().is_none(),
        "an admin port holds no lease"
    );
    assert_ne!(
        admin.port().port_id(),
        holder.session().session_id(),
        "its own identity, not a session's"
    );
    admin.revoke_endpoint(human()).await.expect("revoked");

    let got = suite::receive(&holder, Duration::from_secs(2)).await;
    assert!(
        got.contains(&SessionEvent::Local(
            LocalSessionEvent::EndpointLeaseChanged {
                endpoint: human(),
                revoked_epoch: epoch,
            }
        )),
        "the holder is told which epoch ended: {got:?}"
    );
    assert!(
        holder
            .send_direct(
                DirectDestination {
                    peer: pair.b_peer.clone(),
                    endpoint: None,
                },
                MessageId::from_bytes([9; 16]),
                suite::text("after revocation"),
            )
            .await
            .is_err(),
        "a revoked lease sends nothing"
    );
    holder.close().await.expect("closes");
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_5_a_dropped_session_releases_its_lease() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    suite::a_dropped_session_releases_its_lease(&a, &human()).await;
    pair.stop().await;
}

/// A session whose lease an administrator revoked drains NOTHING of the
/// endpoint's next holder: the queue is the live lease's, checked by
/// epoch at every drain, so the stale session cannot consume messages
/// the remote was told were accepted for the new owner (#139 review F1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_session_drains_nothing_of_the_next_holder() {
    let pair = Pair::start().await;
    let (a, b) = pair.bindings();
    let stale = a.open(suite::full(Some(&human()))).await.expect("leases");
    let admin = a.admin([AdminCapability::Endpoints]).expect("a port");
    admin.revoke_endpoint(human()).await.expect("revoked");
    let next = a
        .open(suite::full(Some(&human())))
        .await
        .expect("the endpoint is free again");

    let sender = b.open(suite::full(Some(&human()))).await.expect("leases");
    sender
        .send_direct(
            DirectDestination {
                peer: pair.a_peer.clone(),
                endpoint: Some(human()),
            },
            MessageId::from_bytes([7; 16]),
            suite::text("for the next holder"),
        )
        .await
        .expect("accepted for the live holder");

    let stolen = stale.events().await.expect("answers");
    assert!(
        !stolen.iter().any(|e| matches!(e, SessionEvent::Direct(_))),
        "the revoked session took the next holder's message: {stolen:?}"
    );
    let got = suite::receive(&next, suite::PATIENCE).await;
    assert!(
        got.iter().any(|e| matches!(e, SessionEvent::Direct(_))),
        "the live holder receives it: {got:?}"
    );
    stale.close().await.expect("closes");
    next.close().await.expect("closes");
    sender.close().await.expect("closes");
    pair.stop().await;
}
