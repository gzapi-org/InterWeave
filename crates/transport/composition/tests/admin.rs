// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What the in-process admin port asks of the runtime's OWNER (plan §16
//! (2)): a shutdown request reaches `ComposedRuntime::shutdown_requested`
//! and stops nothing by itself, and `stop` returns the count of events
//! the runtime dropped, which nothing can read after it (#139 review N1).
//! The port's endpoint operations are the conformance suite's, run
//! against every binding.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, DataCapability, DataSessionBinding, DataSessionPort,
    LocalSessionEvent, SessionEvent, SessionRequest,
};
use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    DirectDestination, DisconnectReason, EndpointId, MessageId, Payload, TransportError,
    TransportEvent, TransportIdentity, TransportRuntime,
};
use interweave_transport_composition::{ComposedRuntime, CompositionOptions};

const PATIENCE: Duration = Duration::from_secs(20);

fn profile(trusted: &[&TransportIdentity], statics: &[String]) -> ProfileConfig {
    let allowed: Vec<String> = trusted
        .iter()
        .map(|p| format!("\"{}\"", p.as_str()))
        .collect();
    let peers: Vec<String> = statics.iter().map(|s| format!("\"{s}\"")).collect();
    let doc = format!(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: [{}]
endpoints:
  entries:
    - id: human
      enabled: true
      advertise: false
discovery:
  providers:
    - type: static-bootstrap
      enabled: true
      priority: 10
      config:
        peers: [{}]
",
        allowed.join(", "),
        peers.join(", ")
    );
    serde_norway::from_str(&doc).expect("the document parses")
}

fn id() -> (ProfileIdentity, TransportIdentity) {
    let identity = ProfileIdentity::generate();
    let peer = identity.transport_identity().expect("peer id");
    (identity, peer)
}

/// The request reaches the owner with the asking port's id and grace;
/// the runtime keeps running until the owner stops it; the first request
/// stands; a port without `admin.shutdown` is refused; and once the owner
/// has stopped the runtime, a request is `BackendUnavailable`.
#[tokio::test]
async fn an_admin_shutdown_is_a_request_the_owner_receives() {
    let (identity, _) = id();
    let runtime =
        ComposedRuntime::start(&identity, &profile(&[], &[]), CompositionOptions::default())
            .await
            .expect("composes");
    let binding = runtime.sessions();

    let powerless = binding
        .admin([AdminCapability::Endpoints].into())
        .await
        .expect("a port");
    assert_eq!(
        powerless.shutdown(Duration::from_secs(1)).await,
        Err(TransportError::CapabilityDenied)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), runtime.shutdown_requested())
            .await
            .is_err(),
        "nothing asked yet"
    );

    let first = binding
        .admin([AdminCapability::Shutdown].into())
        .await
        .expect("a port");
    let second = binding
        .admin([AdminCapability::Shutdown].into())
        .await
        .expect("a port");
    first.shutdown(Duration::from_secs(5)).await.expect("asked");
    second
        .shutdown(Duration::from_secs(1))
        .await
        .expect("asked too");
    let asked = tokio::time::timeout(PATIENCE, runtime.shutdown_requested())
        .await
        .expect("the owner hears it")
        .expect("a request");
    assert_eq!(&asked.port, first.port().port_id(), "the first port's");
    assert_eq!(asked.grace, Duration::from_secs(5), "and its grace stands");
    assert!(
        runtime.health().await.is_ok(),
        "a request stops nothing: the runtime still answers"
    );

    runtime.stop().await.expect("the owner stops it");
    assert_eq!(
        first.shutdown(Duration::from_secs(1)).await,
        Err(TransportError::BackendUnavailable),
        "no owner is left to ask"
    );
}

/// A consumer that never reads, behind a one-slot queue, loses events as
/// a peer connects; `stop` returns at least what was counted before it --
/// the total, not a fresh zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_returns_the_events_the_runtime_dropped() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let listen = CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        ..CompositionOptions::default()
    };
    let (b_id, b) = id();
    let (a_id, a) = id();
    let target = ComposedRuntime::start(&b_id, &profile(&[&a], &[]), listen.clone())
        .await
        .expect("b composes");
    let b_addr = format!("{}/p2p/{}", target.listening()[0], b.as_str());
    let unread = CompositionOptions {
        event_capacity: 1,
        ..listen
    };
    let subject = ComposedRuntime::start(&a_id, &profile(&[&b], &[b_addr]), unread)
        .await
        .expect("a composes");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    while subject.events_dropped() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no event dropped within {PATIENCE:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let before = subject.events_dropped();
    let total = subject.stop().await.expect("stops");
    assert!(
        total >= before,
        "stop returned {total}, below the {before} counted"
    );
    target.shutdown().await.expect("clean shutdown");
}

/// Dropping the runtime ends it, however many bindings and admin ports its
/// owner handed out are still held (#144 review F1): the driver stops, and
/// with it the substrate. The control is the same port answering while
/// the runtime lives.
#[tokio::test]
async fn dropping_the_runtime_ends_it_while_a_binding_is_held() {
    let (identity, _) = id();
    let runtime =
        ComposedRuntime::start(&identity, &profile(&[], &[]), CompositionOptions::default())
            .await
            .expect("composes");
    let binding = runtime.sessions();
    let admin = binding
        .admin([AdminCapability::Status].into())
        .await
        .expect("a port");
    admin
        .status()
        .await
        .expect("the control: a live runtime answers");

    drop(runtime);
    let claim = || {
        SessionRequest::new(
            "conformance",
            Some(EndpointId::parse("human").expect("valid")),
            [DataCapability::Commands],
        )
        .expect("in bounds")
    };
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let status = admin.status().await;
        let opened = binding.open(claim()).await;
        if matches!(status, Err(TransportError::BackendUnavailable))
            && matches!(opened, Err(TransportError::BackendUnavailable))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a dropped runtime still answers: status {:?}, open {:?}",
            status.map(|_| ()),
            opened.map(|_| ())
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `stop_within`'s grace is the one the substrate settles for: with a
/// direct exchange in flight to a peer that never answers, a 200 ms grace
/// stops well inside the default's five seconds. Nothing in flight would
/// stop fast under any grace, so the exchange is dispatched first, over a
/// connection that exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_within_settles_for_the_grace_it_is_given() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let silent = interweave_test_support::silent::silent_direct_peer(ip).await;
    let silent_peer = TransportIdentity::parse(&silent.peer).expect("a peer id");
    let (identity, _) = id();
    let mut runtime = ComposedRuntime::start(
        &identity,
        &profile(&[&silent_peer], std::slice::from_ref(&silent.address)),
        CompositionOptions {
            listen: vec![format!("/ip4/{ip}/tcp/0")],
            ..CompositionOptions::default()
        },
    )
    .await
    .expect("composes");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(TransportEvent::PeerConnected { peer, .. })) if peer == silent_peer => break,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the runtime stopped"),
            Err(elapsed) => panic!("no connection within {PATIENCE:?} ({elapsed})"),
        }
    }
    let session = runtime
        .sessions()
        .open(
            SessionRequest::new(
                "human-client",
                Some(EndpointId::parse("human").expect("valid")),
                [DataCapability::Commands],
            )
            .expect("in bounds"),
        )
        .await
        .expect("leases");
    let dispatched = tokio::time::timeout(
        Duration::from_millis(500),
        session.send_direct(
            DirectDestination {
                peer: silent_peer,
                endpoint: Some(EndpointId::parse("human").expect("valid")),
            },
            MessageId::from_bytes([9; 16]),
            Payload::at_ceiling(None, b"held".to_vec()).expect("within the ceiling"),
        ),
    )
    .await;
    assert!(
        dispatched.is_err(),
        "unanswered, so in flight: {dispatched:?}"
    );

    let started = tokio::time::Instant::now();
    runtime
        .stop_within(Duration::from_millis(200))
        .await
        .expect("stops");
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_secs(2),
        "the caller's grace, not the default's: took {waited:?}"
    );
}

/// The admin port's status carries the substrate's pre-authentication
/// counts: none before anyone connects, then a handshake held open by a
/// silent TCP connection is pending with its source tracked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_admin_status_carries_the_pre_authentication_counts() {
    let (identity, _) = id();
    let runtime = ComposedRuntime::start(
        &identity,
        &profile(&[], &[]),
        CompositionOptions {
            listen: vec!["/ip4/127.0.0.1/tcp/0".to_owned()],
            ..CompositionOptions::default()
        },
    )
    .await
    .expect("composes");
    let port: u16 = runtime.listening()[0]
        .rsplit('/')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("a tcp port");
    let admin = runtime
        .sessions()
        .admin([AdminCapability::Status].into())
        .await
        .expect("a port");
    let counts = || async {
        admin
            .status()
            .await
            .expect("answered")
            .pre_auth
            .expect("the in-process binding has the funnel")
    };
    let before = counts().await;
    assert_eq!(
        (before.pending, before.tracked_sources),
        (0, 0),
        "{before:?}"
    );

    let held = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connects");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut seen = before;
    while seen.pending == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "never pending: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        seen = counts().await;
    }
    assert_eq!((seen.pending, seen.tracked_sources), (1, 1), "{seen:?}");

    // The two apart: the handshake ends, its source stays accounted for
    // its attempt window -- so a swap of the fields is seen here.
    drop(held);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while seen.pending != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "never released: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        seen = counts().await;
    }
    assert_eq!((seen.pending, seen.tracked_sources), (0, 1), "{seen:?}");
    drop(admin);
    runtime.stop().await.expect("stops");
}

/// The admin port's status carries the substrate's ingress limiters:
/// nothing tracked before a peer sends, then the direct sender on the
/// direct lane and not on the broadcast one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_admin_status_carries_the_ingress_limiters_tracked_peers() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let listen = CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        ..CompositionOptions::default()
    };
    let (b_id, b) = id();
    let (a_id, a) = id();
    let target = ComposedRuntime::start(&b_id, &profile(&[&a], &[]), listen.clone())
        .await
        .expect("b composes");
    let b_addr = format!("{}/p2p/{}", target.listening()[0], b.as_str());
    let mut subject = ComposedRuntime::start(&a_id, &profile(&[&b], &[b_addr]), listen)
        .await
        .expect("a composes");
    let admin = target
        .sessions()
        .admin([AdminCapability::Status].into())
        .await
        .expect("a port");
    let tracked = || async {
        let seen = admin
            .status()
            .await
            .expect("answered")
            .ingress
            .expect("the in-process binding has the limiters");
        (seen.direct_tracked_peers, seen.broadcast_tracked_peers)
    };
    assert_eq!(tracked().await, (0, 0), "nobody has sent");

    let human = || {
        SessionRequest::new(
            "human-client",
            Some(EndpointId::parse("human").expect("valid")),
            [DataCapability::Commands],
        )
        .expect("in bounds")
    };
    let receiving = target.sessions().open(human()).await.expect("b leases");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, subject.next_event()).await {
            Ok(Some(TransportEvent::PeerConnected { peer, .. })) if peer == b => break,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the runtime stopped"),
            Err(elapsed) => panic!("no connection within {PATIENCE:?} ({elapsed})"),
        }
    }
    let sending = subject.sessions().open(human()).await.expect("a leases");
    sending
        .send_direct(
            DirectDestination {
                peer: b.clone(),
                endpoint: Some(EndpointId::parse("human").expect("valid")),
            },
            MessageId::from_bytes([7; 16]),
            Payload::at_ceiling(None, b"counted".to_vec()).expect("within the ceiling"),
        )
        .await
        .expect("accepted");
    assert_eq!(tracked().await, (1, 0), "the sender, on the direct lane");

    drop((admin, sending, receiving));
    subject.stop().await.expect("a stops");
    target.stop().await.expect("b stops");
}

/// The runtime's next event, within `PATIENCE`.
async fn next_within(runtime: &mut ComposedRuntime) -> TransportEvent {
    match tokio::time::timeout(PATIENCE, runtime.next_event()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!("the runtime stopped"),
        Err(elapsed) => panic!("nothing within {PATIENCE:?} ({elapsed})"),
    }
}

/// The runtime's peer notice reaches the sessions (`LOCAL-IPC.md`:
/// `peer.disconnected` to every connection holding `events`): B stops,
/// and A's session reads B's disconnect, class `closed`, as the runtime's
/// own stream reports it; a session opened after it is owed nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peers_disconnect_reaches_each_session_holding_events() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let listen = CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        ..CompositionOptions::default()
    };
    let (b_id, b) = id();
    let (a_id, a) = id();
    let target = ComposedRuntime::start(&b_id, &profile(&[&a], &[]), listen.clone())
        .await
        .expect("b composes");
    let b_addr = format!("{}/p2p/{}", target.listening()[0], b.as_str());
    let mut subject = ComposedRuntime::start(&a_id, &profile(&[&b], &[b_addr]), listen)
        .await
        .expect("a composes");
    let watching =
        || SessionRequest::new("human-client", None, [DataCapability::Events]).expect("in bounds");
    let early = subject.sessions().open(watching()).await.expect("opens");
    while !matches!(next_within(&mut subject).await, TransportEvent::PeerConnected { peer, .. } if peer == b)
    {
    }

    target.stop().await.expect("b stops");
    let reason = loop {
        if let TransportEvent::PeerDisconnected {
            peer, reason_class, ..
        } = next_within(&mut subject).await
            && peer == b
        {
            break reason_class;
        }
    };
    assert_eq!(reason, DisconnectReason::Closed, "the runtime's own stream");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    let owed = loop {
        let got = early.events(16).await.expect("reads");
        if !got.is_empty() {
            break got;
        }
        assert!(tokio::time::Instant::now() < deadline, "no notice");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        owed,
        [SessionEvent::Local(LocalSessionEvent::PeerDisconnected {
            peer: b,
            reason_class: "closed".into(),
        })]
    );
    let late = subject.sessions().open(watching()).await.expect("opens");
    assert!(
        late.events(16).await.expect("reads").is_empty(),
        "a session is owed what happened while it was open, nothing before"
    );
    drop((early, late));
    subject.stop().await.expect("a stops");
}
