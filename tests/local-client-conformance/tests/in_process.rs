// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The conformance suite against the direct in-process binding (plan §15:
//! "Run `LocalDataSession` conformance first against the direct in-process
//! binding"): two runtimes composed from profiles, connected over real
//! sockets on the host's private address, each check run against their
//! `sessions()` bindings.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, DataSessionBinding, DataSessionPort, SessionEvent,
};
use interweave_local_client_conformance_tests as suite;
use interweave_transport_api::{
    ChannelId, DirectDestination, EndpointId, MessageId, TransportError,
};
use interweave_transport_composition::ComposedRuntime;

mod common;

use common::{Pair, agent, human};

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
async fn a_bounded_take_leaves_the_rest_queued_in_order() {
    let pair = Pair::start().await;
    let (a, b) = pair.bindings();
    suite::a_bounded_take_leaves_the_rest_queued_in_order(
        &a,
        &b,
        &pair.b_peer,
        &human(),
        &ChannelId::parse("general").expect("valid"),
    )
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_7_administration_is_a_separate_authority() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    suite::administration_is_a_separate_authority(&a, &human(), &pair.b_peer).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabling_an_endpoint_revokes_and_never_rebinds() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    suite::disabling_revokes_and_never_rebinds(&a, &human()).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_admin_view_and_the_default_overlay() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    suite::the_admin_view_and_the_default_overlay(&a, &pair.a_peer, &human(), &agent()).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_query_needs_its_capability() {
    let pair = Pair::start().await;
    let (a, b) = pair.bindings();
    suite::a_directory_query_needs_its_capability(&a, &b, &pair.b_peer, &agent()).await;
    pair.stop().await;
}

/// The substrate's join references, read through the runtime's own
/// diagnostics, until they reach `want`.
async fn join_references_reach(runtime: &ComposedRuntime, want: usize) {
    let deadline = tokio::time::Instant::now() + suite::PATIENCE;
    loop {
        let got = runtime
            .diagnostics()
            .await
            .expect("answered")
            .substrate
            .broadcast_join_references;
        if got == want {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "join references stayed {got}, never {want}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A session that ends -- closed, or dropped -- leaves the substrate
/// holding none of its joins (#139 review F5): the substrate's release
/// ends leases, not joins, so the binding must leave for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ended_session_leaves_every_join() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    let channels = [
        ChannelId::parse("general").expect("legal"),
        ChannelId::parse("ops").expect("legal"),
    ];
    let closed = a.open(suite::full(None)).await.expect("opens");
    let dropped = a.open(suite::full(None)).await.expect("opens");
    for channel in &channels {
        closed.join(channel.clone()).await.expect("joins");
        dropped.join(channel.clone()).await.expect("joins");
    }
    join_references_reach(&pair.a, 4).await;
    closed.close().await.expect("closes");
    join_references_reach(&pair.a, 2).await;
    drop(dropped);
    join_references_reach(&pair.a, 0).await;
    pair.stop().await;
}

/// Poll `future` exactly once and drop it, returning whether it was still
/// waiting -- a caller that gave up after its command was sent. On a
/// current-thread runtime the substrate cannot answer inside that poll.
/// (A zero `timeout` does not do this: its deadline fires only after the
/// timer runs, by when the substrate had answered.)
fn cancelled_after_one_poll<F: std::future::Future>(future: F) -> bool {
    let mut future = std::pin::pin!(future);
    polled_once(future.as_mut())
}

/// Poll a pinned future once, keeping it: whether it is still waiting.
fn polled_once<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> bool {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    future.poll(&mut cx).is_pending()
}

/// A `join` whose caller stops waiting after the command left holds no
/// join, even while the session lives: its guard queues the leave behind
/// the join (#139 review N3), and the session records only accepted
/// joins. A second join, answered, is the fence -- the substrate has
/// taken both earlier commands by then -- and the count is exactly its
/// one reference; dropping the session takes it back to none.
#[tokio::test(flavor = "current_thread")]
async fn a_cancelled_join_holds_no_join_while_the_session_lives() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    let session = a.open(suite::full(None)).await.expect("opens");
    assert!(
        cancelled_after_one_poll(session.join(ChannelId::parse("ops").expect("legal"))),
        "the join was cancelled mid-flight"
    );
    session
        .join(ChannelId::parse("general").expect("legal"))
        .await
        .expect("joins");
    join_references_reach(&pair.a, 1).await;
    drop(session);
    join_references_reach(&pair.a, 0).await;
    pair.stop().await;
}

/// A cancelled RE-join of a channel the session already holds ends
/// nothing: that join is the substrate's no-op, and its guard is not armed
/// -- a leave would end the join it repeats. Fenced as above: the count
/// is both joins' references.
#[tokio::test(flavor = "current_thread")]
async fn a_cancelled_rejoin_keeps_the_join_it_repeats() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    let session = a.open(suite::full(None)).await.expect("opens");
    let ops = ChannelId::parse("ops").expect("legal");
    session.join(ops.clone()).await.expect("joins");
    assert!(
        cancelled_after_one_poll(session.join(ops)),
        "the re-join was cancelled mid-flight"
    );
    session
        .join(ChannelId::parse("general").expect("legal"))
        .await
        .expect("joins");
    join_references_reach(&pair.a, 2).await;
    drop(session);
    join_references_reach(&pair.a, 0).await;
    pair.stop().await;
}

/// The substrate's join references, once every command already queued
/// has been taken: the status ask queues behind them.
async fn join_references(runtime: &ComposedRuntime) -> usize {
    runtime
        .diagnostics()
        .await
        .expect("answered")
        .substrate
        .broadcast_join_references
}

/// Cancel joins of `channel` on `session` until the command channel is
/// full, after draining it with an awaited leave and padding it by `pad`
/// cancelled leaves of a channel nobody joined. Returns whether a join
/// took the channel's LAST slot -- the substrate then holds that join
/// while its leave is owed, one reference above `before`. The answer is
/// MEASURED, not derived: a run of cancelled joins reaches that slot when
/// the free slots were odd, but other senders share the channel, so the
/// caller tries several pads and acts only on a pass that saw it.
async fn cancel_joins_until_full<S: DataSessionPort>(
    runtime: &ComposedRuntime,
    session: &S,
    channel: &ChannelId,
    pad: usize,
) -> bool {
    let unjoined = ChannelId::parse("unjoined").expect("legal");
    session.leave(unjoined.clone()).await.expect("leaves");
    let before = join_references(runtime).await;
    for _ in 0..pad {
        assert!(cancelled_after_one_poll(session.leave(unjoined.clone())));
    }
    // Four times the substrate's command depth: past its boundary,
    // whatever else the runtime had queued.
    for _ in 0..256 {
        assert!(cancelled_after_one_poll(session.join(channel.clone())));
    }
    join_references(runtime).await == before + 1
}

/// A join cancelled when its own command took the channel's LAST slot is
/// joined by the substrate while its leave finds the channel full: that
/// leave is owed, and sent under the session's lock by its next join or
/// leave -- before that join, never after it, where it would end the join
/// the session records -- or by its teardown (#144 re-review 2, F1;
/// re-review 3). Which pass reaches the last slot depends on what else the
/// runtime queued, so each phase tries pads until one pass is SEEN to
/// hold the owed join (`cancel_joins_until_full`), acts on that pass, and
/// fails if none does.
#[tokio::test(flavor = "current_thread")]
async fn a_leave_owed_on_a_full_channel_is_sent_before_the_next_join() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    let session = a.open(suite::full(None)).await.expect("opens");
    let unjoined = ChannelId::parse("unjoined").expect("legal");
    let base = join_references(&pair.a).await;

    // The next JOIN sends the owed leave first: the rejoin is held.
    let mut reached = false;
    for pad in 0..8 {
        let channel = ChannelId::parse(format!("rejoined{pad}")).expect("legal");
        if cancel_joins_until_full(&pair.a, &session, &channel, pad).await {
            session.join(channel.clone()).await.expect("joins");
            assert_eq!(join_references(&pair.a).await, base + 1, "the rejoin holds");
            session.leave(channel).await.expect("leaves");
            assert_eq!(join_references(&pair.a).await, base, "and is left");
            reached = true;
            break;
        }
    }
    assert!(reached, "no pass reached the last slot for the rejoin");

    // The next LEAVE sends it, with no join after it to do so instead.
    let mut reached = false;
    for pad in 0..8 {
        let channel = ChannelId::parse(format!("stray{pad}")).expect("legal");
        if cancel_joins_until_full(&pair.a, &session, &channel, pad).await {
            session.leave(unjoined.clone()).await.expect("leaves");
            assert_eq!(join_references(&pair.a).await, base, "the leave sent it");
            reached = true;
            break;
        }
    }
    assert!(reached, "no pass reached the last slot for the leave");

    // TEARDOWN sends it: a session that owes one and is only dropped.
    let mut reached = false;
    for pad in 0..8 {
        let dropped = a.open(suite::full(None)).await.expect("opens");
        let channel = ChannelId::parse(format!("dropped{pad}")).expect("legal");
        let owed = cancel_joins_until_full(&pair.a, &dropped, &channel, pad).await;
        drop(dropped);
        join_references_reach(&pair.a, base).await;
        if owed {
            reached = true;
            break;
        }
    }
    assert!(reached, "no pass reached the last slot for the teardown");

    drop(session);
    join_references_reach(&pair.a, 0).await;
    pair.stop().await;
}

/// An `open` cancelled after its claim left claims nothing: the endpoint
/// comes back without an administrator.
#[tokio::test(flavor = "current_thread")]
async fn a_cancelled_open_holds_no_lease() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    assert!(
        cancelled_after_one_poll(a.open(suite::full(Some(&human())))),
        "the open was cancelled mid-flight"
    );
    let deadline = tokio::time::Instant::now() + suite::PATIENCE;
    let session = loop {
        match a.open(suite::full(Some(&human()))).await {
            Ok(session) => break session,
            Err(TransportError::EndpointInUse) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the cancelled open still holds the lease"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(other) => panic!("unexpected refusal: {other:?}"),
        }
    };
    session.close().await.expect("closes");
    pair.stop().await;
}

/// A session's joins and leaves of one channel settle in the order they
/// were asked (#144 review F3): a `leave` asked first must not erase the
/// record of the `join` asked after it, or the substrate holds a join the
/// session will never leave. Both are polled before either answer is
/// read, on one thread, and the leave is read first; the control is the
/// count reaching one while the session lives.
#[tokio::test(flavor = "current_thread")]
async fn a_leave_asked_before_a_join_leaves_the_join_recorded() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    let session = a.open(suite::full(None)).await.expect("opens");
    let channel = ChannelId::parse("ops").expect("legal");
    {
        let mut leave = std::pin::pin!(session.leave(channel.clone()));
        let mut join = std::pin::pin!(session.join(channel.clone()));
        assert!(polled_once(leave.as_mut()), "the leave waits on its answer");
        let _ = polled_once(join.as_mut());
        leave.as_mut().await.expect("leaves");
        join.as_mut().await.expect("joins");
    }
    join_references_reach(&pair.a, 1).await;
    drop(session);
    join_references_reach(&pair.a, 0).await;
    pair.stop().await;
}

/// A session dropped on a thread with NO runtime, while the command
/// channel is full, still releases its lease: its teardown cannot be
/// queued at once and goes to the runtime the binding was made in, not to
/// whichever runtime is current where it drops -- there is none (the
/// owner's review of c283e375, P2-1). A current-thread runtime, so
/// nothing drains the channel until this test awaits.
#[tokio::test(flavor = "current_thread")]
async fn a_session_dropped_off_runtime_on_a_full_channel_releases_its_lease() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    let holder = a.open(suite::full(Some(&human()))).await.expect("leases");
    let filler = a.open(suite::full(None)).await.expect("opens");
    let channel = ChannelId::parse("filler").expect("legal");
    // Four times the substrate's command depth: the channel is full.
    for _ in 0..256 {
        assert!(cancelled_after_one_poll(filler.join(channel.clone())));
    }
    std::thread::spawn(move || drop(holder))
        .join()
        .expect("dropped off the runtime");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        match a.open(suite::full(Some(&human()))).await {
            Ok(_) => break,
            Err(e) => assert!(
                tokio::time::Instant::now() < deadline,
                "never claimable again: {e:?}"
            ),
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(filler);
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
    let admin = a
        .admin([AdminCapability::Endpoints].into())
        .await
        .expect("a port");
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

    let stolen = stale.events(usize::MAX).await.expect("answers");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_9_ready_resolves_on_what_waits_and_takes_nothing() {
    let pair = Pair::start().await;
    let (a, b) = pair.bindings();
    suite::ready_resolves_on_what_waits_and_takes_nothing(&a, &b, &pair.b_peer, &agent(), &human())
        .await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_10_the_runtimes_state_is_owed_once_at_open() {
    let pair = Pair::start().await;
    let (_, b) = pair.bindings();
    suite::the_runtimes_state_is_owed_once_at_open(&b).await;
    pair.stop().await;
}

/// The in-process `ready` is WOKEN -- by a delivery and by a revocation --
/// not found by its once-a-second recheck: each wait ends well inside
/// that interval. And a stopped runtime ends every wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_process_ready_is_woken_well_inside_its_recheck() {
    const WOKEN_WITHIN: Duration = Duration::from_millis(400);
    let pair = Pair::start().await;
    let (a, b) = pair.bindings();
    let from = a.open(suite::full(Some(&agent()))).await.expect("leases");
    let to = b.open(suite::full(Some(&human()))).await.expect("leases");
    to.events(usize::MAX).await.expect("the open-time state");
    let timed = |label: &'static str| {
        let to = &to;
        async move {
            // Past any recheck already running, so only a wake ends it
            // inside the window.
            assert!(
                tokio::time::timeout(Duration::from_millis(1100), to.ready())
                    .await
                    .is_err(),
                "{label}: nothing owed yet"
            );
        }
    };
    timed("before the message").await;
    {
        let wait = to.ready();
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut wait)
                .await
                .is_err()
        );
        from.send_direct(
            DirectDestination {
                peer: pair.b_peer.clone(),
                endpoint: Some(human()),
            },
            MessageId::from_bytes([3; 16]),
            suite::text("wake"),
        )
        .await
        .expect("accepted");
        tokio::time::timeout(WOKEN_WITHIN, &mut wait)
            .await
            .expect("a delivery wakes the session")
            .expect("ready");
    }
    to.events(usize::MAX).await.expect("the message");
    timed("before the revocation").await;
    {
        let wait = to.ready();
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut wait)
                .await
                .is_err()
        );
        let admin = b
            .admin([AdminCapability::Endpoints].into())
            .await
            .expect("admin");
        admin.revoke_endpoint(human()).await.expect("revoked");
        tokio::time::timeout(WOKEN_WITHIN, &mut wait)
            .await
            .expect("a revocation wakes its holder")
            .expect("ready");
    }
    let notices = to.events(usize::MAX).await.expect("the notice");
    assert!(
        notices.iter().any(|e| matches!(
            e,
            SessionEvent::Local(
                interweave_local_client_api::LocalSessionEvent::EndpointLeaseChanged { .. }
            )
        )),
        "{notices:?}"
    );
    let survivor = b.open(suite::full(None)).await.expect("opens");
    survivor
        .events(usize::MAX)
        .await
        .expect("the open-time state");
    drop((from, to));
    // Waiting ACROSS the stop: its end wakes the wait, not a later look.
    // TWO waits, since `ready` takes `&self`: the end wakes every one.
    {
        let wait = survivor.ready();
        let other = survivor.ready();
        tokio::pin!(wait);
        tokio::pin!(other);
        let both = async { tokio::join!(&mut wait, &mut other) };
        assert!(
            tokio::time::timeout(Duration::from_millis(1100), both)
                .await
                .is_err()
        );
        pair.stop().await;
        // ONE poll each, the moment the stop returns: the driver ended the
        // waits before its task finished, while the next recheck is still
        // hundreds of milliseconds off (the waits started 1.1 s before a
        // stop that takes about half a second, measured 2026-10-04).
        for w in [wait.as_mut(), other.as_mut()] {
            tokio::time::timeout(Duration::ZERO, w)
                .await
                .expect("a stopped runtime has ended every wait")
                .expect("ready");
        }
    }
    assert_eq!(
        survivor.events(1).await,
        Err(TransportError::BackendUnavailable),
        "and events reports it"
    );
}

/// Item 10's path half through the in-process binding's own wiring, a
/// path change posted through the driver's handling (`test-hooks`):
/// a message the session DRAINED is a route, and so is a send it had
/// accepted; a session with no route is owed nothing; and the notice is
/// taken after a message still waiting, under `max`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drained_message_is_a_route_and_a_path_change_follows_it() {
    use interweave_local_client_api::LocalSessionEvent;
    use interweave_transport_api::PeerPath;
    let pair = Pair::start().await;
    let (a, b) = pair.bindings();
    let from = a.open(suite::full(Some(&agent()))).await.expect("leases");
    let to = b.open(suite::full(Some(&human()))).await.expect("leases");
    let stranger = b.open(suite::full(None)).await.expect("opens");
    let send = |n: u8| {
        from.send_direct(
            DirectDestination {
                peer: pair.b_peer.clone(),
                endpoint: Some(human()),
            },
            MessageId::from_bytes([n; 16]),
            suite::text("a route"),
        )
    };
    let is_path = |e: &SessionEvent| {
        matches!(
            e,
            SessionEvent::Local(LocalSessionEvent::PeerPathChanged { .. })
        )
    };
    send(1).await.expect("accepted");
    assert_eq!(
        suite::receive(&to, suite::PATIENCE).await.len(),
        1,
        "the first message, drained: a route"
    );
    send(2).await.expect("accepted");
    // The second waits, untaken, while the path changes: accepted means
    // admitted to the receiver's queue (`AcceptedV2`), and `ready` sees
    // it there without taking it.
    tokio::time::timeout(suite::PATIENCE, to.ready())
        .await
        .expect("the second message is queued")
        .expect("ready");
    pair.b
        .inject_path_change(pair.a_peer.clone(), PeerPath::Relayed, PeerPath::Direct)
        .await
        .expect("posted");
    pair.a
        .inject_path_change(pair.b_peer.clone(), PeerPath::Relayed, PeerPath::Direct)
        .await
        .expect("posted");

    let first: Vec<SessionEvent> = to
        .events(usize::MAX)
        .await
        .expect("events")
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                SessionEvent::Local(LocalSessionEvent::ServerState { .. })
            )
        })
        .collect();
    assert!(
        matches!(first.as_slice(), [SessionEvent::Direct(_), last] if is_path(last)),
        "the waiting message, then the path notice: {first:?}"
    );
    let SessionEvent::Local(LocalSessionEvent::PeerPathChanged {
        peer,
        previous,
        current,
        reason_class,
        ..
    }) = &first[1]
    else {
        unreachable!()
    };
    assert_eq!(
        (peer, *previous, *current, reason_class.as_str()),
        (&pair.a_peer, PeerPath::Relayed, PeerPath::Direct, "dcutr")
    );
    assert!(
        !stranger
            .events(usize::MAX)
            .await
            .expect("events")
            .iter()
            .any(is_path),
        "no route, nothing owed"
    );
    assert!(
        from.events(usize::MAX)
            .await
            .expect("events")
            .iter()
            .any(is_path),
        "an accepted send is a route too"
    );
    drop((from, to, stranger));
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trust_administration_revokes_as_policy() {
    let pair = Pair::start().await;
    let (a, _) = pair.bindings();
    suite::trust_administration_revokes_as_policy(&a, &pair.a_peer, &pair.b_peer).await;
    pair.stop().await;
}
