// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `transport/libp2p/CONNECTIVITY.md` §14 item 5, on the wire: a
//! network change CLOSES every connection running from an IP it took
//! off this host, and keeps the rest (the rule since 2026-09-26,
//! SPIKE-004 phase B's `ifchange` row).
//!
//! On one host an interface going away is a listener going away -- the
//! same listener events the OS raises -- so the subject holds two
//! listeners on this host's private address and drops them in turn:
//!
//! - the first, while the second still carries the IP: a change is
//!   reported and NOTHING closes -- the control, and the case a match on
//!   the removed address alone would get wrong;
//! - the second: the IP departs, and both connections running from it
//!   close -- one that arrived on the first listener, and one the
//!   subject DIALLED, whose local IP libp2p never reports and the
//!   runtime reads from the kernel's route;
//! - a connection over loopback, which no removal touched, stays.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
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
        .any(|e| matches!(e, SwarmEvent::Disconnected { peer: p } if p == peer))
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
    // IP. Reported, and nothing closes.
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
        "the first removal reported",
        PATIENCE,
        Some(|events: &[SwarmEvent]| {
            events.iter().any(
                |e| matches!(e, SwarmEvent::NetworkChanged { removed, .. } if !removed.is_empty()),
            )
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
            if *removed == vec![first.to_string()])),
        "{events:?}"
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
            if *removed == vec![second.to_string()])),
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
