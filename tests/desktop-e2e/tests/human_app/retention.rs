// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What the shipped client keeps across its own end (ADR-0044,
//! RETENTION.md): unread inbound and pending outbound survive a restart
//! and a real `SIGKILL`; a pending send leaves the store once the
//! transport is done with it, and does not come back.
//!
//! Observed in the client's store file and at the other end, never by a
//! person: the read-and-not-kept half of each case needs a read, which is
//! the window's (batch 7).

use std::time::Duration;

use crate::common::human;
use crate::harness::{self as app, until_lease};
use crate::world::{self, Peer, envelope, ids, two_daemons, until_rows};

/// A `claude` endpoint nobody leases: a send to it is answered
/// `no_route` and retried, so it stays pending for as long as the test
/// wants it to.
fn unheld() -> interweave_transport_api::EndpointId {
    interweave_transport_api::EndpointId::parse("claude").expect("an endpoint")
}

fn one(id: &str) -> std::collections::BTreeSet<String> {
    [id.to_owned()].into()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unread_inbound_persists_across_a_restart_of_the_client() {
    let world = two_daemons().await;
    let mut peer = Peer::new(&world.b);
    let mut client = app::start(&world.a);
    until_lease(&world.a.binding(), true, &client).await;
    peer.until("B ready", || world.logs(), Peer::is_ready).await;

    let message = envelope(21, "unread until someone reads it");
    peer.send(&world.a_peer, Some(human()), &message).await;
    until_rows(&world.a, "unread_inbound", 1, || client.log()).await;
    assert!(client.terminate().success(), "{}", client.log());
    assert_eq!(
        ids(&world.a, "unread_inbound"),
        one(&message.app_message_id)
    );

    let mut again = app::start(&world.a);
    until_lease(&world.a.binding(), true, &again).await;
    // Long enough for the start-up listing and a few turns of the loop.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        ids(&world.a, "unread_inbound"),
        one(&message.app_message_id),
        "still unread, and only once, after the client came back"
    );
    assert!(again.terminate().success(), "{}", again.log());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pending_send_survives_a_restart_and_leaves_the_store_once_accepted() {
    let world = two_daemons().await;
    // Nobody holds B's `human` lease yet: the send is answered `no_route`
    // and retried.
    let message = envelope(31, "sent while nobody was there");
    world::seed_pending(&world.a, &world.b_peer, &human(), &message);
    let mut client = app::start(&world.a);
    until_lease(&world.a.binding(), true, &client).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        ids(&world.a, "pending_outbound"),
        one(&message.app_message_id),
        "control: tried, refused, and still pending"
    );
    assert!(client.terminate().success(), "{}", client.log());
    assert_eq!(
        ids(&world.a, "pending_outbound"),
        one(&message.app_message_id)
    );

    let mut again = app::start(&world.a);
    let mut peer = Peer::new(&world.b);
    peer.until(
        "the pending send reaching B",
        || world.logs(),
        |p| p.got(&message.app_message_id),
    )
    .await;
    until_rows(&world.a, "pending_outbound", 0, || again.log()).await;
    assert!(again.terminate().success(), "{}", again.log());
    assert!(
        ids(&world.a, "pending_outbound").is_empty(),
        "accepted is terminal: the pending copy is gone, and stays gone"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_kill_keeps_pending_outbound_and_unread_inbound_and_not_a_terminal_send() {
    let world = two_daemons().await;
    let done = envelope(41, "accepted before the kill");
    let held = envelope(42, "pending at the kill");
    world::seed_pending(&world.a, &world.b_peer, &human(), &done);
    world::seed_pending(&world.a, &world.b_peer, &unheld(), &held);
    let mut peer = Peer::new(&world.b);
    let mut client = app::start(&world.a);
    until_lease(&world.a.binding(), true, &client).await;
    peer.until("B ready", || world.logs(), Peer::is_ready).await;

    let unread = envelope(43, "unread at the kill");
    peer.send(&world.a_peer, Some(human()), &unread).await;
    peer.until(
        "the accepted send reaching B",
        || world.logs(),
        |p| p.got(&done.app_message_id),
    )
    .await;
    until_rows(&world.a, "pending_outbound", 1, || client.log()).await;
    until_rows(&world.a, "unread_inbound", 1, || client.log()).await;

    let status = client.kill();
    assert!(!status.success(), "killed, not exited: {status:?}");
    assert_eq!(
        ids(&world.a, "pending_outbound"),
        one(&held.app_message_id),
        "the pending send survived the kill, and the accepted one did not come back"
    );
    assert_eq!(
        ids(&world.a, "unread_inbound"),
        one(&unread.app_message_id),
        "the unread message survived the kill"
    );

    // And the client that comes back still holds them, and still sends.
    let mut again = app::start(&world.a);
    until_lease(&world.a.binding(), true, &again).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(ids(&world.a, "pending_outbound"), one(&held.app_message_id));
    assert_eq!(ids(&world.a, "unread_inbound"), one(&unread.app_message_id));
    assert!(again.terminate().success(), "{}", again.log());
}
