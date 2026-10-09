// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `transport/libp2p/CONNECTIVITY.md` §14 on the wire, from both of the
//! detector's sources: the listeners' bound set and the platform's view.
//!
//! Item 5: a network change CLOSES every connection running from an IP
//! it took off this host, and keeps the rest (the rule since
//! 2026-09-26, SPIKE-004 phase B's `ifchange` row). On one host an
//! interface going away is a listener going away -- the same listener
//! events the OS raises -- so the subject holds two listeners on this
//! host's private address and drops them in turn:
//!
//! - the first, while the second still carries the IP: the host is on
//!   the same network, so NO change is reported and NOTHING closes --
//!   the control, and the case a match on the removed address alone
//!   would get wrong;
//! - the second: the IP departs, and both connections running from it
//!   close -- one that arrived on the first listener, and one the
//!   subject DIALLED, whose local IP libp2p never reports and the
//!   runtime reads from the kernel's route;
//! - a connection over loopback, which no removal touched, stays.
//!
//! The platform's view (§20 step 5) does the same with every listener
//! still bound -- the hand-over seen before the listener poll -- and an
//! ADDITION it reports makes a peer held off by its dial backoff
//! dialable at once (architect-cto's ruling of 2026-10-09, relay seq
//! 33736), where without a view the peer waits its backoff.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{NetworkView, SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

const PATIENCE: Duration = Duration::from_secs(20);
const WINDOW: Duration = Duration::from_secs(2);

fn trusting(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone())).expect("a small allowlist"),
        InfrastructureSet::default(),
    )
}

struct Node {
    runtime: SwarmRuntime,
    peer: TransportIdentity,
}

fn node(trusted: &[&TransportIdentity], id: &ProfileIdentity) -> Node {
    Node {
        runtime: SwarmRuntime::start(id, SubstrateConfig::default(), trusting(trusted))
            .expect("starts"),
        peer: id.transport_identity().expect("peer id"),
    }
}

/// Drive every runtime for `window`, or until `pred` matches one of the
/// SUBJECT's events; returns the subject's events.
async fn drive(
    subject: &mut SwarmRuntime,
    others: &mut [&mut SwarmRuntime],
    what: &str,
    window: Duration,
    mut pred: Option<impl FnMut(&[SwarmEvent]) -> bool>,
) -> Vec<SwarmEvent> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        if let Some(p) = pred.as_mut()
            && p(&events)
        {
            return events;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            assert!(pred.is_none(), "timed out waiting for {what}: {events:?}");
            return events;
        }
        let next_other = async {
            let futures: Vec<_> = others
                .iter_mut()
                .map(|o| Box::pin(o.next_event()))
                .collect();
            futures::future::select_all(futures).await
        };
        tokio::select! {
            event = subject.next_event() => events.push(event.expect("the subject is alive")),
            _ = next_other => {}
            () = tokio::time::sleep(remaining) => {}
        }
    }
}

fn disconnected(events: &[SwarmEvent], peer: &TransportIdentity) -> bool {
    events
        .iter()
        .any(|e| matches!(e, SwarmEvent::Disconnected { peer: p, .. } if p == peer))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removal_closes_what_ran_from_the_departed_ip_and_keeps_the_rest() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let (subject_id, arriving_id, dialled_id, loopback_id) = (
        ProfileIdentity::generate(),
        ProfileIdentity::generate(),
        ProfileIdentity::generate(),
        ProfileIdentity::generate(),
    );
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let (arriving_peer, dialled_peer, loopback_peer) = (
        arriving_id.transport_identity().expect("peer id"),
        dialled_id.transport_identity().expect("peer id"),
        loopback_id.transport_identity().expect("peer id"),
    );
    let mut subject = node(
        &[&arriving_peer, &dialled_peer, &loopback_peer],
        &subject_id,
    );
    let mut arriving = node(&[&subject_peer], &arriving_id);
    let mut dialled = node(&[&subject_peer], &dialled_id);
    let mut on_loopback = node(&[&subject_peer], &loopback_id);

    let private: Multiaddr = format!("/ip4/{ip}/tcp/0").parse().expect("valid");
    let first = subject
        .runtime
        .listen(private.clone())
        .await
        .expect("the subject's first private listener");
    let second = subject
        .runtime
        .listen(private.clone())
        .await
        .expect("the subject's second private listener");
    let dialled_addr = dialled
        .runtime
        .listen(private)
        .await
        .expect("the dialled peer listens on the private address");
    let loopback_addr = on_loopback
        .runtime
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("the loopback peer listens");

    // THREE CONNECTIONS: one arriving on the FIRST listener, one the
    // subject dials from the private address, one over loopback.
    arriving
        .runtime
        .dial(subject_peer.clone(), first.clone())
        .await
        .expect("reaches the task")
        .expect("admitted");
    subject
        .runtime
        .dial(dialled.peer.clone(), dialled_addr)
        .await
        .expect("reaches the task")
        .expect("admitted");
    subject
        .runtime
        .dial(on_loopback.peer.clone(), loopback_addr)
        .await
        .expect("reaches the task")
        .expect("admitted");
    let peers = [
        arriving.peer.clone(),
        dialled.peer.clone(),
        on_loopback.peer.clone(),
    ];
    let mut others = [
        &mut arriving.runtime,
        &mut dialled.runtime,
        &mut on_loopback.runtime,
    ];
    drive(
        &mut subject.runtime,
        &mut others,
        "all three connected",
        PATIENCE,
        Some(|events: &[SwarmEvent]| {
            peers.iter().all(|p| {
                events
                    .iter()
                    .any(|e| matches!(e, SwarmEvent::Connected { peer, .. } if peer == p))
            })
        }),
    )
    .await;

    // THE CONTROL: the first listener goes, the second still carries the
    // IP. The host is on the same network: no change, and nothing closes.
    // The listener's own close is what shows the window was live.
    assert!(
        subject
            .runtime
            .stop_listening(first.clone())
            .await
            .expect("reaches the task")
    );
    let mut events = drive(
        &mut subject.runtime,
        &mut others,
        "the first listener's close",
        PATIENCE,
        Some(|events: &[SwarmEvent]| {
            events
                .iter()
                .any(|e| matches!(e, SwarmEvent::ListeningStopped { addresses, .. } if addresses.contains(&first)))
        }),
    )
    .await;
    events.extend(
        drive(
            &mut subject.runtime,
            &mut others,
            "settling",
            WINDOW,
            None::<fn(&[SwarmEvent]) -> bool>,
        )
        .await,
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SwarmEvent::NetworkChanged { .. })),
        "the IP is still bound, so the network did not move: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SwarmEvent::Disconnected { .. })),
        "the IP is still bound, so nothing closed -- not even the connection that arrived on \
         the listener that went: {events:?}"
    );

    // THE RULE: the second goes, the IP departs, and what ran from it
    // closes -- arrived and dialled alike -- while loopback stays.
    assert!(
        subject
            .runtime
            .stop_listening(second.clone())
            .await
            .expect("reaches the task")
    );
    let mut events = drive(
        &mut subject.runtime,
        &mut others,
        "both private connections closed",
        PATIENCE,
        Some(|events: &[SwarmEvent]| {
            disconnected(events, &peers[0]) && disconnected(events, &peers[1])
        }),
    )
    .await;
    events.extend(
        drive(
            &mut subject.runtime,
            &mut others,
            "settling",
            WINDOW,
            None::<fn(&[SwarmEvent]) -> bool>,
        )
        .await,
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SwarmEvent::NetworkChanged { removed, .. }
            if *removed == vec![std::net::IpAddr::from(ip)])),
        "{events:?}"
    );
    assert!(
        !disconnected(&events, &peers[2]),
        "the loopback connection is over no departed IP and stays: {events:?}"
    );

    for n in [subject, arriving, dialled, on_loopback] {
        n.runtime.shutdown().await.expect("shutdown");
    }
}

/// The platform's view, with every listener still bound: the first view
/// agreeing with the listeners moves nothing, and an empty one -- offline
/// -- takes the private IP off the host, so what ran from it closes and
/// the loopback connection stays. The hand-over seen before the listener
/// poll, which on a device is 10 s behind or never comes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_view_without_the_ip_closes_what_ran_from_it_while_its_listener_is_still_bound() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let (subject_id, arriving_id, loopback_id) = (
        ProfileIdentity::generate(),
        ProfileIdentity::generate(),
        ProfileIdentity::generate(),
    );
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let (arriving_peer, loopback_peer) = (
        arriving_id.transport_identity().expect("peer id"),
        loopback_id.transport_identity().expect("peer id"),
    );
    let mut subject = node(&[&arriving_peer, &loopback_peer], &subject_id);
    let mut arriving = node(&[&subject_peer], &arriving_id);
    let mut on_loopback = node(&[&subject_peer], &loopback_id);
    let private = subject
        .runtime
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("the subject's private listener");
    let loopback_addr = on_loopback
        .runtime
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("the loopback peer listens");
    arriving
        .runtime
        .dial(subject_peer.clone(), private)
        .await
        .expect("reaches the task")
        .expect("admitted");
    subject
        .runtime
        .dial(loopback_peer.clone(), loopback_addr)
        .await
        .expect("reaches the task")
        .expect("admitted");
    let peers = [arriving_peer.clone(), loopback_peer.clone()];
    let mut others = [&mut arriving.runtime, &mut on_loopback.runtime];
    drive(
        &mut subject.runtime,
        &mut others,
        "both connected",
        PATIENCE,
        Some(|events: &[SwarmEvent]| {
            peers.iter().all(|p| {
                events
                    .iter()
                    .any(|e| matches!(e, SwarmEvent::Connected { peer, .. } if peer == p))
            })
        }),
    )
    .await;

    // The first view names what the listeners bound: nothing moved.
    subject.runtime.network_changed(NetworkView {
        addresses: vec![ip.into(), "127.0.0.1".parse().expect("an ip")],
    });
    let events = drive(
        &mut subject.runtime,
        &mut others,
        "settling",
        WINDOW,
        None::<fn(&[SwarmEvent]) -> bool>,
    )
    .await;
    assert!(
        !events.iter().any(|e| matches!(
            e,
            SwarmEvent::NetworkChanged { .. } | SwarmEvent::Disconnected { .. }
        )),
        "a view that agrees is no change: {events:?}"
    );

    // OFFLINE: the private IP departs though its listener is bound.
    subject.runtime.network_changed(NetworkView::default());
    let mut events = drive(
        &mut subject.runtime,
        &mut others,
        "the arrived connection closed",
        PATIENCE,
        Some(|events: &[SwarmEvent]| disconnected(events, &arriving_peer)),
    )
    .await;
    events.extend(
        drive(
            &mut subject.runtime,
            &mut others,
            "settling",
            WINDOW,
            None::<fn(&[SwarmEvent]) -> bool>,
        )
        .await,
    );
    assert!(
        events.iter().any(
            |e| matches!(e, SwarmEvent::NetworkChanged { removed, added }
            if *removed == vec![std::net::IpAddr::from(ip)] && added.is_empty())
        ),
        "{events:?}"
    );
    assert!(
        !disconnected(&events, &loopback_peer),
        "the loopback connection is over no departed IP and stays: {events:?}"
    );

    for n in [subject, arriving, on_loopback] {
        n.runtime.shutdown().await.expect("shutdown");
    }
}

/// A view that ADDS an address makes a peer held off by its dial backoff
/// dialable at once: the scheduler redials it within a tick, through the
/// gate, long before its 30 s retry -- and without the view, the control,
/// it waits. The control's window is longer than conntrack's 10 s CLOSE
/// state for the failed dial's port pair, so the redial is not dropped
/// before TCP sees it (a same-port redial inside that window hangs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_addition_redials_a_held_off_peer_at_once_and_without_one_it_waits() {
    const CONTROL: Duration = Duration::from_secs(12);
    let ip = interweave_test_support::net::require_private_interface_v4();
    let (subject_id, far_id) = (ProfileIdentity::generate(), ProfileIdentity::generate());
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let far_peer = far_id.transport_identity().expect("peer id");
    let mut subject = node(&[&far_peer], &subject_id);
    let mut far = node(&[&subject_peer], &far_id);
    // The subject's private listener fills its set; the view later adds
    // to it.
    let _private = subject
        .runtime
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("the subject's private listener");
    let far_addr = far
        .runtime
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("the far peer listens");
    assert!(
        far.runtime
            .stop_listening(far_addr.clone())
            .await
            .expect("reaches the task")
    );

    // THE FAILURE: the far peer is not there, so the dial is refused and
    // the peer held off for the first retry's 30 s.
    subject
        .runtime
        .dial(far_peer.clone(), far_addr.clone())
        .await
        .expect("reaches the task")
        .expect("admitted");
    let mut others = [&mut far.runtime];
    drive(
        &mut subject.runtime,
        &mut others,
        "the dial to fail",
        PATIENCE,
        Some(|events: &[SwarmEvent]| {
            events.iter().any(
                |e| matches!(e, SwarmEvent::DialFailed { peer: Some(p), .. } if *p == far_peer),
            )
        }),
    )
    .await;
    let failed_at = tokio::time::Instant::now();
    // Back where it was.
    let _ = others[0]
        .listen(far_addr.clone())
        .await
        .expect("the far peer listens again on the same address");

    // THE CONTROL: no view, and the peer waits its backoff.
    let events = drive(
        &mut subject.runtime,
        &mut others,
        "the control window",
        CONTROL,
        None::<fn(&[SwarmEvent]) -> bool>,
    )
    .await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SwarmEvent::Connected { peer, .. } if *peer == far_peer)),
        "without a change the peer waits its backoff: {events:?}"
    );

    // THE ADDITION: redialled at once.
    let added = std::net::IpAddr::from(std::net::Ipv4Addr::new(10, 255, 0, 1));
    subject.runtime.network_changed(NetworkView {
        addresses: vec![ip.into(), added],
    });
    let events = drive(
        &mut subject.runtime,
        &mut others,
        "the held-off peer redialled",
        PATIENCE,
        Some(|events: &[SwarmEvent]| {
            events
                .iter()
                .any(|e| matches!(e, SwarmEvent::Connected { peer, .. } if *peer == far_peer))
        }),
    )
    .await;
    assert!(
        failed_at.elapsed() < Duration::from_secs(30),
        "connected inside the backoff, so the change made it due: {:?}",
        failed_at.elapsed()
    );
    assert!(
        events.iter().any(
            |e| matches!(e, SwarmEvent::NetworkChanged { removed, added: a }
            if removed.is_empty() && *a == vec![added])
        ),
        "{events:?}"
    );

    for n in [subject, far] {
        n.runtime.shutdown().await.expect("shutdown");
    }
}

/// The view is a snapshot slot, not a queue: two views handed in before
/// the runtime reads either count as the LATEST one, so the change is
/// computed against it alone -- the first view's address is never
/// reported -- and nothing blocks the caller. On the current-thread
/// runtime the task cannot run between the two reports.
#[tokio::test]
async fn the_latest_view_replaces_one_not_yet_read() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let id = ProfileIdentity::generate();
    let mut subject = node(&[], &id);
    let _private = subject
        .runtime
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("the subject's private listener");
    let (superseded, latest) = (
        std::net::IpAddr::from(std::net::Ipv4Addr::new(10, 255, 0, 1)),
        std::net::IpAddr::from(std::net::Ipv4Addr::new(10, 255, 0, 2)),
    );
    subject.runtime.network_changed(NetworkView {
        addresses: vec![ip.into(), superseded],
    });
    subject.runtime.network_changed(NetworkView {
        addresses: vec![ip.into(), latest],
    });
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + WINDOW;
    while let Ok(Some(event)) =
        tokio::time::timeout_at(deadline, subject.runtime.next_event()).await
    {
        events.push(event);
    }
    let changes: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, SwarmEvent::NetworkChanged { .. }))
        .collect();
    assert!(
        matches!(changes.as_slice(), [SwarmEvent::NetworkChanged { removed, added }]
            if removed.is_empty() && *added == vec![latest]),
        "one change, against the latest view only: {changes:?}"
    );
    subject.runtime.shutdown().await.expect("shutdown");
}

/// ADR-0052 rule 3 (A 2026-10-09) at a runtime learn site: a peer's
/// private candidate is admitted beside a private listener on an IP the
/// host holds -- the control -- and refused once the platform's view
/// says that IP departed, though its listener is still bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_departed_ips_listener_admits_no_private_candidate() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let (subject_id, far_id) = (ProfileIdentity::generate(), ProfileIdentity::generate());
    let far_peer = far_id.transport_identity().expect("peer id");
    let mut subject = node(&[&far_peer], &subject_id);
    let _private = subject
        .runtime
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("the subject's private listener");
    subject.runtime.network_changed(NetworkView {
        addresses: vec![ip.into()],
    });
    let admitted = subject
        .runtime
        .learn(far_peer.clone(), ["/ip4/10.1.2.3/tcp/4001".to_owned()])
        .await
        .expect("reaches the task");
    assert_eq!(admitted, 1, "the control: beside a held private listener");

    subject.runtime.network_changed(NetworkView::default());
    // The view and a command reach the task on two channels, so wait for
    // the removal the view makes -- the proof it was read -- before
    // asking; a timed settle lost that race under a loaded suite.
    let removal = std::net::IpAddr::from(ip);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let event = tokio::time::timeout_at(deadline, subject.runtime.next_event())
            .await
            .expect("the view's removal within the patience")
            .expect("the subject is alive");
        if matches!(&event, SwarmEvent::NetworkChanged { removed, .. } if removed.contains(&removal))
        {
            break;
        }
    }
    let admitted = subject
        .runtime
        .learn(far_peer.clone(), ["/ip4/10.1.2.4/tcp/4001".to_owned()])
        .await
        .expect("reaches the task");
    assert_eq!(
        admitted, 0,
        "the listener's IP departed: no private listener of the family is held"
    );
    subject.runtime.shutdown().await.expect("shutdown");
}
