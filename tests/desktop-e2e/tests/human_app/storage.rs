// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A store that cannot take unread content takes the client off the
//! network rather than accept what it cannot keep (ADR-0044,
//! RETENTION.md): the full store gives up the `human` lease, so the next
//! message is refused at the daemon (`no_route`, retried by its sender)
//! instead of being accepted into nothing.
//!
//! The store is filled for real -- a page ceiling SQLite enforces with
//! the same `SQLITE_FULL` a full disk gives. What this does not close:
//! the message that found the store full was already admitted by the
//! daemon (`AcceptedV2` before the unread commit), the window §17
//! carries by name.

use std::time::Duration;

use interweave_human_transport_client::{OutboundStatus, SendProblem};

use crate::common::human;
use crate::harness::{self as app, until_lease};
use crate::world::{Peer, app_store, envelope, rows, two_daemons};

/// The store's size in pages, read-only.
fn pages(home: &crate::common::Home) -> u32 {
    let conn = rusqlite::Connection::open_with_flags(
        app_store(home),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("the app's store opens read-only");
    conn.query_row("PRAGMA page_count", [], |r| r.get(0))
        .expect("a page count")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_store_gives_up_the_endpoint_rather_than_accept_unread_content() {
    let world = two_daemons().await;
    // A first run makes the store; the quota is then exactly its size, so
    // the next row that needs a page finds it full.
    let mut first = app::start(&world.a);
    until_lease(&world.a.binding(), true, &first).await;
    assert!(first.terminate().success(), "{}", first.log());
    let quota = pages(&world.a).to_string();

    let mut client = app::start_with(&world.a, &["--store-max-pages", &quota]);
    until_lease(&world.a.binding(), true, &client).await;
    let mut peer = Peer::new(&world.b);
    peer.until("B ready", || world.logs(), Peer::is_ready).await;

    // Larger than any page's free space: its row needs new pages.
    let filler = envelope(71, &"a message the full store cannot hold. ".repeat(1_000));
    peer.send(&world.a_peer, Some(human()), &filler).await;
    until_lease(&world.a.binding(), false, &client).await;
    assert!(client.running(), "degraded, not gone: {}", client.log());

    let refused = envelope(72, "after the store filled");
    peer.send(&world.a_peer, Some(human()), &refused).await;
    // Refused at A's daemon as `no_route` -- the endpoint has no lease --
    // and scheduled for a retry: the only status that outcome produces.
    peer.until(
        "the next message refused no_route and scheduled for a retry",
        || format!("{}\n{}", client.log(), world.logs()),
        |p| {
            matches!(
                p.outbound.get(&refused.app_message_id),
                Some(OutboundStatus::Sending {
                    attempts: 1..,
                    next_retry_at: Some(_),
                    last_problem: Some(SendProblem::RouteUnavailable),
                })
            )
        },
    )
    .await;
    // And it stays that way: never accepted while the store is full.
    let settle = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < settle {
        peer.step().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !peer.accepted(&refused.app_message_id),
        "not accepted while the store is full: {:?}",
        peer.outbound.get(&refused.app_message_id)
    );
    assert_eq!(
        rows(&world.a, "unread_inbound"),
        0,
        "nothing was committed unread into the full store"
    );
    assert!(client.terminate().success(), "{}", client.log());
}
