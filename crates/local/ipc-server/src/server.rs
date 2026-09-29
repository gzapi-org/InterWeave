// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The server: accept, admit, and one task per connection until asked to
//! stop (plan §16 (1)-(8)).
//!
//! Stopping here is the server's share of the daemon's shutdown order
//! (plan §16 (6)): stop accepting, then close every connection -- each
//! gets `close{ShuttingDown}` and its session is closed, which releases
//! its lease. Settling the network, closing the Swarm, unlinking the
//! sockets and dropping the lock are the daemon's, after this returns.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use interweave_ipc_protocol::{Close, Frame, ServerState};
use interweave_local_client_api::{AdminBinding, AdminCapability, DataSessionBinding};
use interweave_transport_api::TransportError;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::admission::{Refusal, Slots};
use crate::connection::{self, CLOSE_GRACE, Shared};

/// How long stopping waits for every connection to end before aborting
/// what remains: one close grace, and as long again.
const STOP_GRACE: Duration = CLOSE_GRACE.saturating_mul(2);
use crate::counters::Counters;
use crate::hello::ServerConfig;
use crate::listen::Listeners;
use crate::state;

/// Serve `binding` on the bound sockets until `stop` resolves, then close
/// every connection and return.
///
/// The server holds no lease table and no queue: every lease, join and
/// event lives in `binding`, which the composition root built.
pub async fn serve<B>(
    listeners: Listeners,
    binding: B,
    config: ServerConfig,
    stop: impl Future<Output = ()> + Send,
) -> Arc<Counters>
where
    B: DataSessionBinding + AdminBinding + Clone + Send + Sync + 'static,
    B::Session: Send + Sync + 'static,
    B::Admin: Send + Sync + 'static,
{
    let counters = Arc::new(Counters::default());
    let slots = Slots::new(config.limits, Arc::clone(&counters));
    let (state_tx, state_rx) = watch::channel::<Option<ServerState>>(None);
    let mut background = JoinSet::new();
    // The server's own status holding: `admin.status` only, granting no
    // client anything. Without it data clients get no `server_state`.
    if let Ok(port) = binding.admin([AdminCapability::Status].into()).await {
        if let Some(view) = state::read(&port).await {
            state::publish(&state_tx, view);
        }
        background.spawn(state::refresh(
            port,
            state_tx.clone(),
            config.keepalive.interval,
        ));
    }
    let shared = Arc::new(Shared {
        config,
        counters: Arc::clone(&counters),
        state: state_rx,
    });
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut connections = JoinSet::new();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => break,
            accepted = listeners.accept() => {
                let Ok((stream, domain)) = accepted else {
                    // An accept error (a descriptor limit, say) must not
                    // spin the loop.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                let peer_uid = stream.peer_cred().ok().map(|c| c.uid());
                match slots.admit(peer_uid, listeners.owner_uid(), domain) {
                    // Nothing is written to a process of another uid.
                    Err(Refusal::PeerCredential) => drop(stream),
                    Err(Refusal::Full) => {
                        connections.spawn(async move {
                            let mut stream = stream;
                            if let Ok(bytes) = Frame::Close(Close::new(TransportError::Overloaded)).encode() {
                                let _ = stream.write_all(&bytes).await;
                            }
                            let _ = stream.shutdown().await;
                        });
                    }
                    Ok(slot) => {
                        connections.spawn(connection::run(
                            stream,
                            domain,
                            binding.clone(),
                            Arc::clone(&shared),
                            stop_rx.clone(),
                            slot,
                        ));
                    }
                }
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    let _ = stop_tx.send(true);
    // Every connection sees `stop` at once (its loop never waits on the
    // client) and bounds its own end by CLOSE_GRACE; STOP_GRACE is the
    // backstop, after which what is left is aborted (#151 review, F1).
    let drained = tokio::time::timeout(STOP_GRACE, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        connections.shutdown().await;
    }
    drop(state_tx);
    background.shutdown().await;
    counters
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::admission::Limits;
    use crate::fake::{Fake, PEER, peer};
    use crate::frames::FrameReader;
    use crate::hello::KeepalivePolicy;
    use crate::listen::{SocketPaths, bind};
    use interweave_ipc_protocol::encode_frame;
    use interweave_local_client_api::{LocalSessionEvent, SessionEvent};
    use interweave_transport_api::{EndpointId, Health};
    use tokio::net::UnixStream;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    const PATIENCE: Duration = Duration::from_secs(5);

    struct Harness {
        _root: tempfile::TempDir,
        paths: SocketPaths,
        stop: Option<oneshot::Sender<()>>,
        server: JoinHandle<Arc<Counters>>,
    }

    impl Harness {
        fn start(fake: &Fake, config: ServerConfig) -> Self {
            Self::start_with(fake, config, |_| {})
        }

        fn start_with(
            fake: &Fake,
            config: ServerConfig,
            tweak: impl FnOnce(&mut Listeners),
        ) -> Self {
            let root = tempfile::tempdir().expect("tempdir");
            let run_dir = root.path().join("interweave");
            let paths = SocketPaths {
                data: run_dir.join("data.sock"),
                admin: run_dir.join("admin.sock"),
                run_dir,
            };
            let mut listeners = bind(&paths).expect("binds");
            tweak(&mut listeners);
            let (stop, stopped) = oneshot::channel();
            let server = tokio::spawn(serve(listeners, fake.clone(), config, async {
                let _ = stopped.await;
            }));
            Self {
                _root: root,
                paths,
                stop: Some(stop),
                server,
            }
        }

        async fn stop(mut self) -> Arc<Counters> {
            let _ = self.stop.take().expect("once").send(());
            self.server.await.expect("the server stops")
        }
    }

    fn config() -> ServerConfig {
        ServerConfig {
            peer: peer(),
            limits: Limits::default(),
            keepalive: KeepalivePolicy::default(),
            shutdown_grace: Duration::from_secs(5),
            command_deadline: Duration::from_secs(10),
            write_stall: crate::hello::WRITE_STALL,
        }
    }

    struct Client {
        reader: FrameReader<OwnedReadHalf>,
        write: OwnedWriteHalf,
    }

    impl Client {
        async fn connect(path: &std::path::Path) -> Self {
            let (read, write) = UnixStream::connect(path)
                .await
                .expect("connects")
                .into_split();
            Self {
                reader: FrameReader::new(read),
                write,
            }
        }

        async fn send(&mut self, body: &str) {
            use tokio::io::AsyncWriteExt as _;
            let frame = encode_frame(body).expect("a frame");
            self.write.write_all(&frame).await.expect("sent");
        }

        /// The next frame, or `None` when the server closed the stream.
        async fn next(&mut self) -> Option<Frame> {
            let body = tokio::time::timeout(PATIENCE, self.reader.next())
                .await
                .expect("the server answers in time")
                .ok()??;
            Some(Frame::parse(&body).expect("the server writes frames"))
        }

        /// The next frame that is not a `server_state` push.
        async fn next_reply(&mut self) -> Option<Frame> {
            loop {
                match self.next().await? {
                    Frame::ServerState(_) => {}
                    frame => return Some(frame),
                }
            }
        }

        async fn response(&mut self) -> ResponseFrameView {
            match self.next_reply().await {
                Some(Frame::Response(r)) => ResponseFrameView {
                    id: r.id.as_str().to_owned(),
                    body: Frame::Response(r).to_body(),
                },
                other => panic!("a response, got {other:?}"),
            }
        }

        async fn hello(&mut self, hello: &str) {
            self.send(hello).await;
            match self.next_reply().await {
                Some(Frame::HelloResponse(_)) => {}
                other => panic!("a hello_response, got {other:?}"),
            }
        }
    }

    struct ResponseFrameView {
        id: String,
        body: String,
    }

    const DATA: &str = r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
        "client":{"kind":"human-client"},"endpoint":{"id":"human"},
        "requested_capabilities":["events","commands"],"features":["keepalive"]}"#;
    const ADMIN: &str = r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
        "client":{"kind":"transportctl"},"requested_capabilities":["admin.status"]}"#;

    fn join(id: &str) -> String {
        format!(
            r#"{{"type":"request","id":"{id}","method":"channel.join","params":{{"channel":"ops"}}}}"#
        )
    }

    fn send(id: &str) -> String {
        format!(
            r#"{{"type":"request","id":"{id}","method":"direct.send","params":{{"peer":"{PEER}",
            "message_id":"00000000000000000000000000000001","payload":{{"bytes":""}}}}}}"#
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_request_is_answered_and_an_unknown_method_leaves_the_connection_open() {
        let fake = Fake::default();
        let harness = Harness::start(&fake, config());
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        client
            .send(r#"{"type":"request","id":"x","method":"admin.trust.add"}"#)
            .await;
        let unknown = client.response().await;
        assert!(
            unknown.body.contains("ProtocolUnsupported"),
            "{}",
            unknown.body
        );
        client.send(&join("1")).await;
        let joined = client.response().await;
        assert_eq!(joined.id, "1");
        assert!(joined.body.contains(r#""ok":true"#), "{}", joined.body);
        assert!(fake.script().calls.contains(&"join ops".to_owned()));
        drop(client);
        harness.stop().await;
    }

    /// An admin method on the data socket is refused before dispatch and
    /// COUNTED; the admin socket reports the count.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_other_domains_method_is_refused_and_counted() {
        let fake = Fake::default();
        let harness = Harness::start(&fake, config());
        let mut data = Client::connect(&harness.paths.data).await;
        data.hello(DATA).await;
        data.send(r#"{"type":"request","id":"1","method":"admin.shutdown","params":{}}"#)
            .await;
        let refused = data.response().await;
        assert!(
            refused.body.contains("CapabilityDenied"),
            "{}",
            refused.body
        );
        assert!(
            !fake
                .script()
                .calls
                .iter()
                .any(|c| c.starts_with("shutdown"))
        );
        let mut admin = Client::connect(&harness.paths.admin).await;
        admin.hello(ADMIN).await;
        admin
            .send(r#"{"type":"request","id":"s","method":"admin.status"}"#)
            .await;
        let status = admin.response().await;
        assert!(
            status
                .body
                .contains(r#""cross_domain_capability_denied_total":1"#),
            "{}",
            status.body
        );
        assert!(
            status.body.contains(r#""data_connections":1"#),
            "{}",
            status.body
        );
        assert!(
            status.body.contains(r#""admin_connections":1"#),
            "{}",
            status.body
        );
        drop((data, admin));
        harness.stop().await;
    }

    /// 16 in flight, 48 waiting, the 65th `Overloaded`; a waiting one
    /// cancelled is `CancelledBeforeDispatch`, one in flight
    /// `CancellationRaced` -- and its outcome is not answered again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn requests_are_bounded_and_a_cancel_answers_once() {
        let fake = Fake::default();
        let hold = Arc::new(tokio::sync::Notify::new());
        fake.script().hold_send = Some(Arc::clone(&hold));
        let harness = Harness::start(&fake, config());
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        for i in 0..=(crate::MAX_IN_FLIGHT + crate::MAX_PENDING) {
            client.send(&send(&format!("r{i}"))).await;
        }
        let overloaded = client.response().await;
        assert_eq!(
            overloaded.id,
            format!("r{}", crate::MAX_IN_FLIGHT + crate::MAX_PENDING)
        );
        assert!(
            overloaded.body.contains("Overloaded"),
            "{}",
            overloaded.body
        );
        client.send(r#"{"type":"cancel","id":"r20"}"#).await;
        let waiting = client.response().await;
        assert_eq!(waiting.id, "r20");
        assert!(
            waiting.body.contains("CancelledBeforeDispatch"),
            "{}",
            waiting.body
        );
        client.send(r#"{"type":"cancel","id":"r0"}"#).await;
        let raced = client.response().await;
        assert_eq!(raced.id, "r0");
        assert!(raced.body.contains("CancellationRaced"), "{}", raced.body);
        // Release every send: each remaining request is answered once,
        // r0 and r20 not at all.
        fake.script().hold_send = None;
        let mut answered = std::collections::BTreeSet::new();
        while answered.len() < crate::MAX_IN_FLIGHT + crate::MAX_PENDING - 2 {
            hold.notify_waiters();
            let response = client.response().await;
            assert!(
                answered.insert(response.id.clone()),
                "{} answered twice",
                response.id
            );
        }
        assert!(!answered.contains("r0") && !answered.contains("r20"));
        drop(client);
        harness.stop().await;
    }

    /// An id whose response the client already has -- here a raced
    /// cancel answered it while its task still runs -- is not outstanding
    /// to the client: its reuse is refused with a response, and the
    /// connection stays open.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_answered_id_reused_while_its_task_runs_is_refused_not_closed() {
        let fake = Fake::default();
        let hold = Arc::new(tokio::sync::Notify::new());
        fake.script().hold_send = Some(Arc::clone(&hold));
        let harness = Harness::start(&fake, config());
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        client.send(&send("r0")).await;
        client.send(r#"{"type":"cancel","id":"r0"}"#).await;
        let raced = client.response().await;
        assert!(raced.body.contains("CancellationRaced"), "{}", raced.body);
        client.send(&send("r0")).await;
        let refused = client.response().await;
        assert_eq!(refused.id, "r0");
        assert!(refused.body.contains("InvalidArgument"), "{}", refused.body);
        hold.notify_waiters();
        drop(client);
        harness.stop().await;
    }

    /// A request id reused while the first still awaits its response --
    /// in flight or waiting for a slot -- closes the connection with
    /// `ProtocolViolation`; distinct ids beside it are the control.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_request_id_reused_while_outstanding_closes_the_connection() {
        for (reused, label) in [("r0", "in flight"), ("r16", "waiting")] {
            let fake = Fake::default();
            let hold = Arc::new(tokio::sync::Notify::new());
            fake.script().hold_send = Some(Arc::clone(&hold));
            let harness = Harness::start(&fake, config());
            let mut client = Client::connect(&harness.paths.data).await;
            client.hello(DATA).await;
            // r16 waits: the first MAX_IN_FLIGHT fill every slot.
            for i in 0..=crate::MAX_IN_FLIGHT {
                client.send(&send(&format!("r{i}"))).await;
            }
            // The control: a cancel for a distinct id is answered and the
            // connection stays open.
            client.send(r#"{"type":"cancel","id":"r1"}"#).await;
            assert_eq!(client.response().await.id, "r1", "{label}");
            client.send(&send(reused)).await;
            loop {
                match client.next_reply().await {
                    Some(Frame::Close(close)) => {
                        assert_eq!(close.code, TransportError::ProtocolViolation, "{label}");
                        break;
                    }
                    Some(Frame::Response(response)) => {
                        panic!("{label}: answered rather than closed: {response:?}")
                    }
                    Some(_) => {}
                    None => panic!("{label}: closed without a close frame"),
                }
            }
            hold.notify_waiters();
            drop(client);
            harness.stop().await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_sessions_events_are_written_in_sequence() {
        let fake = Fake::default();
        let harness = Harness::start(&fake, config());
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        for _ in 0..2 {
            fake.script().events.push_back(SessionEvent::Local(
                LocalSessionEvent::PeerDisconnected {
                    peer: peer(),
                    reason_class: "policy".into(),
                },
            ));
        }
        let mut sequences = Vec::new();
        while sequences.len() < 2 {
            if let Some(Frame::Event(event)) = client.next_reply().await {
                assert_eq!(event.event_type, "peer.disconnected");
                sequences.push(event.sequence);
            }
        }
        assert_eq!(sequences, [0, 1]);
        drop(client);
        harness.stop().await;
    }

    /// A client that does not read fills the socket and then the event
    /// lane; the pump then takes nothing, so the rest stays with the
    /// binding -- and once the client reads, every event arrives, in
    /// sequence, none lost (#151 review, F3).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn events_past_the_lane_stay_with_the_binding_until_there_is_room() {
        // Past what the socket buffer and the lane hold together.
        const QUEUED: usize = 1000;
        let fake = Fake::default();
        let harness = Harness::start(&fake, config());
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        for _ in 0..QUEUED {
            fake.script().events.push_back(SessionEvent::Local(
                LocalSessionEvent::PeerDisconnected {
                    peer: peer(),
                    reason_class: "policy".into(),
                },
            ));
        }
        // Wait for the pump to stop taking: the socket and the lane are full.
        let mut held = QUEUED;
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let now = fake.script().events.len();
            if now == held {
                break;
            }
            held = now;
        }
        assert!(held > 0, "the pump took what the lane had no room for");
        let mut next = 0;
        while next < QUEUED {
            match client.next_reply().await {
                Some(Frame::Event(event)) => {
                    assert_eq!(event.sequence, next as u64, "no event lost or reordered");
                    next += 1;
                }
                Some(_) => {}
                None => panic!("the connection closed after {next} of {QUEUED} events"),
            }
        }
        drop(client);
        harness.stop().await;
    }

    /// Unanswered probes close the connection, and its lease is released
    /// as for any disconnect.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unanswered_probes_close_the_connection_and_release_its_lease() {
        let fake = Fake::default();
        let mut config = config();
        config.keepalive.interval = Duration::from_millis(50);
        config.keepalive.response_timeout = Duration::from_millis(50);
        config.keepalive.max_missed = 2;
        let harness = Harness::start(&fake, config);
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        let mut pings = 0;
        let close = loop {
            match client.next_reply().await {
                Some(Frame::Ping(_)) => pings += 1,
                Some(Frame::Close(close)) => break close,
                other => panic!("pings then a close, got {other:?}"),
            }
        };
        assert_eq!(close.code, TransportError::Timeout);
        assert_eq!(pings, 2);
        assert!(client.next().await.is_none(), "closed");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(fake.script().leased.is_empty(), "the lease is released");
        assert_eq!(fake.script().closed, 1);
        harness.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_answered_probe_keeps_the_connection() {
        let fake = Fake::default();
        let mut config = config();
        config.keepalive.interval = Duration::from_millis(50);
        config.keepalive.response_timeout = Duration::from_millis(100);
        config.keepalive.max_missed = 1;
        let harness = Harness::start(&fake, config);
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        for _ in 0..3 {
            let Some(Frame::Ping(ping)) = client.next_reply().await else {
                panic!("a ping")
            };
            client.send(&Frame::Pong(ping.echo()).to_body()).await;
        }
        client.send(&join("after")).await;
        assert_eq!(client.response().await.id, "after", "still open");
        drop(client);
        harness.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_disconnect_closes_the_session_and_frees_the_lease_and_the_slot() {
        let fake = Fake::default();
        let mut config = config();
        config.limits = Limits {
            max_clients: 1,
            max_admin_clients: 1,
        };
        let harness = Harness::start(&fake, config);
        let mut first = Client::connect(&harness.paths.data).await;
        first.hello(DATA).await;
        let mut second = Client::connect(&harness.paths.data).await;
        match second.next_reply().await {
            Some(Frame::Close(close)) => assert_eq!(close.code, TransportError::Overloaded),
            other => panic!("the one slot is held: {other:?}"),
        }
        drop(first);
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while fake.script().closed == 0 {
            assert!(tokio::time::Instant::now() < deadline, "the session closes");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(fake.script().leased.is_empty());
        let mut third = Client::connect(&harness.paths.data).await;
        third.hello(DATA).await;
        drop(third);
        harness.stop().await;
    }

    /// Another uid cannot connect unprivileged, so the run directory's
    /// owner is made someone else: the connection is closed without a
    /// word, and counted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_peer_of_another_uid_is_closed_before_hello_and_counted() {
        let fake = Fake::default();
        let harness = Harness::start_with(&fake, config(), |l| {
            l.owner_uid = l.owner_uid.wrapping_add(1);
        });
        let mut client = Client::connect(&harness.paths.data).await;
        // Nothing is sent: the server may close before a write would land,
        // and a closed socket is what is being asserted.
        assert!(client.next().await.is_none(), "closed, nothing written");
        let counters = harness.stop().await;
        assert_eq!(counters.snapshot().peer_credential_refused_total, 1);
        assert!(
            fake.script().calls.is_empty(),
            "the binding was never asked"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_data_client_gets_the_view_on_connect_and_on_change() {
        let fake = Fake::default();
        let mut config = config();
        config.keepalive.interval = Duration::from_millis(50);
        let harness = Harness::start(&fake, config);
        let mut client = Client::connect(&harness.paths.data).await;
        client.send(DATA).await;
        assert!(matches!(client.next().await, Some(Frame::HelloResponse(_))));
        let Some(Frame::ServerState(first)) = client.next().await else {
            panic!("the view on connect")
        };
        assert_eq!(first.health, Health::Healthy);
        fake.script().health = Some(Health::Degraded);
        loop {
            match client.next().await {
                Some(Frame::ServerState(view)) if view.health == Health::Degraded => break,
                Some(Frame::ServerState(_) | Frame::Ping(_)) => {}
                other => panic!("the changed view, got {other:?}"),
            }
        }
        drop(client);
        harness.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopping_closes_every_connection_with_shutting_down() {
        let fake = Fake::default();
        let harness = Harness::start(&fake, config());
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        let paths = harness.paths.clone();
        let stopping = tokio::spawn(harness.stop());
        match client.next_reply().await {
            Some(Frame::Close(close)) => assert_eq!(close.code, TransportError::ShuttingDown),
            other => panic!("a close, got {other:?}"),
        }
        stopping.await.expect("stopped");
        assert_eq!(fake.script().closed, 1, "the session is closed");
        let _ = (paths, EndpointId::parse("human"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_hello_or_a_server_class_after_hello_is_a_violation() {
        let fake = Fake::default();
        let harness = Harness::start(&fake, config());
        for second in [DATA, r#"{"type":"server_state","health":"healthy"}"#] {
            let mut client = Client::connect(&harness.paths.data).await;
            client.hello(DATA).await;
            client.send(second).await;
            match client.next_reply().await {
                Some(Frame::Close(close)) => {
                    assert_eq!(close.code, TransportError::ProtocolViolation);
                }
                other => panic!("a close, got {other:?}"),
            }
            // Each connection releases its lease before the next claims it.
            let deadline = tokio::time::Instant::now() + PATIENCE;
            while !fake.script().leased.is_empty() {
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        harness.stop().await;
    }

    fn send_by(id: &str, deadline_ms: u64) -> String {
        format!(
            r#"{{"type":"request","id":"{id}","method":"direct.send","deadline_ms":{deadline_ms},
            "params":{{"peer":"{PEER}","message_id":"00000000000000000000000000000001",
            "payload":{{"bytes":""}}}}}}"#
        )
    }

    /// A request handed over and still unanswered at its deadline is
    /// answered `Timeout`; its outcome is discarded when it comes. A zero
    /// deadline is clamped to one second, not refused.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_request_past_its_deadline_is_answered_timeout_once() {
        let fake = Fake::default();
        let hold = Arc::new(tokio::sync::Notify::new());
        fake.script().hold_send = Some(Arc::clone(&hold));
        let harness = Harness::start(&fake, config());
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        let sent = tokio::time::Instant::now();
        client.send(&send_by("late", 0)).await;
        let timed_out = client.response().await;
        let waited = sent.elapsed();
        assert_eq!(timed_out.id, "late");
        assert!(timed_out.body.contains("Timeout"), "{}", timed_out.body);
        assert!(
            waited >= Duration::from_millis(900),
            "clamped to 1 s, answered after {waited:?}"
        );
        fake.script().hold_send = None;
        hold.notify_waiters();
        client.send(&join("next")).await;
        assert_eq!(
            client.response().await.id,
            "next",
            "the late outcome is not answered"
        );
        drop(client);
        harness.stop().await;
    }

    /// A request still waiting at its deadline is answered `Timeout` and
    /// never reaches the port.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_waiting_request_past_its_deadline_is_dropped_unsent() {
        let fake = Fake::default();
        let hold = Arc::new(tokio::sync::Notify::new());
        fake.script().hold_send = Some(Arc::clone(&hold));
        let mut config = config();
        config.command_deadline = Duration::from_secs(60);
        let harness = Harness::start(&fake, config);
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        for i in 0..crate::MAX_IN_FLIGHT {
            client.send(&send(&format!("held{i}"))).await;
        }
        client.send(&send_by("waiting", 1000)).await;
        let timed_out = client.response().await;
        assert_eq!(timed_out.id, "waiting");
        assert!(timed_out.body.contains("Timeout"), "{}", timed_out.body);
        let sends = fake
            .script()
            .calls
            .iter()
            .filter(|c| c.starts_with("send"))
            .count();
        assert_eq!(
            sends,
            crate::MAX_IN_FLIGHT,
            "the waiting one was never handed over"
        );
        drop(client);
        harness.stop().await;
    }

    /// A client that sends and never reads: its answers stall the writer,
    /// which gives up after `write_stall`, the connection closes and the
    /// slot is free again -- the loop never waited on it (#151 review,
    /// F1-F2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_client_that_never_reads_is_closed_and_frees_its_slot() {
        let fake = Fake::default();
        let mut config = config();
        config.limits = Limits {
            max_clients: 1,
            max_admin_clients: 1,
        };
        config.write_stall = Duration::from_millis(300);
        let harness = Harness::start(&fake, config);
        let mut deaf = Client::connect(&harness.paths.data).await;
        deaf.hello(DATA).await;
        let unknown = encode_frame(r#"{"type":"request","id":"x","method":"no.such.method"}"#)
            .expect("frame");
        let flood: Vec<u8> = unknown
            .iter()
            .copied()
            .cycle()
            .take(unknown.len() * 40_000)
            .collect();
        // The server may close before the flood is written; that is the
        // outcome under test, not an error.
        let _ = tokio::time::timeout(
            PATIENCE,
            tokio::io::AsyncWriteExt::write_all(&mut deaf.write, &flood),
        )
        .await;
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let mut next = Client::connect(&harness.paths.data).await;
            next.send(DATA).await;
            match next.next_reply().await {
                Some(Frame::HelloResponse(_)) => break,
                Some(Frame::Close(close)) if close.code == TransportError::Overloaded => {}
                other => panic!("the slot frees, got {other:?}"),
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the stalled connection was never closed"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        drop(deaf);
        harness.stop().await;
    }

    /// A port call that panics is answered `Internal`, once, and the
    /// connection goes on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_port_call_is_answered_internal() {
        let fake = Fake::default();
        fake.script().panic_join = true;
        let harness = Harness::start(&fake, config());
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        client.send(&join("boom")).await;
        let answer = client.response().await;
        assert_eq!(answer.id, "boom");
        assert!(answer.body.contains("Internal"), "{}", answer.body);
        fake.script().panic_join = false;
        client.send(&join("after")).await;
        assert_eq!(client.response().await.id, "after");
        drop(client);
        harness.stop().await;
    }

    /// An event the protocol refuses takes its sequence number, so the
    /// client sees a gap instead of nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_event_leaves_a_sequence_gap() {
        let fake = Fake::default();
        let harness = Harness::start(&fake, config());
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        for reason_class in [String::new(), "policy".to_owned()] {
            fake.script().events.push_back(SessionEvent::Local(
                LocalSessionEvent::PeerDisconnected {
                    peer: peer(),
                    reason_class,
                },
            ));
        }
        let sequence = loop {
            if let Some(Frame::Event(event)) = client.next_reply().await {
                break event.sequence;
            }
        };
        assert_eq!(sequence, 1, "the refused event took 0");
        drop(client);
        harness.stop().await;
    }

    /// A client that pipelines faster than the writer drains, but reads,
    /// is slowed by backpressure and never closed: every request is
    /// answered, once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pipelining_client_that_reads_is_answered_in_full() {
        let fake = Fake::default();
        let harness = Harness::start(&fake, config());
        let mut client = Client::connect(&harness.paths.data).await;
        client.hello(DATA).await;
        let burst: Vec<u8> = (0..500)
            .flat_map(|i| {
                encode_frame(&format!(
                    r#"{{"type":"request","id":"u{i}","method":"no.such.method"}}"#
                ))
                .expect("frame")
            })
            .collect();
        tokio::io::AsyncWriteExt::write_all(&mut client.write, &burst)
            .await
            .expect("sent");
        let mut answered = std::collections::BTreeSet::new();
        while answered.len() < 500 {
            let response = client.response().await;
            assert!(
                response.body.contains("ProtocolUnsupported"),
                "{}",
                response.body
            );
            assert!(
                answered.insert(response.id.clone()),
                "{} twice",
                response.id
            );
        }
        drop(client);
        harness.stop().await;
    }

    /// Stopping does not wait on a client that never reads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopping_is_bounded_by_a_client_that_never_reads() {
        let fake = Fake::default();
        let harness = Harness::start(&fake, config());
        let mut deaf = Client::connect(&harness.paths.data).await;
        deaf.hello(DATA).await;
        let flood: Vec<u8> = (0..20_000)
            .flat_map(|_| {
                encode_frame(r#"{"type":"request","id":"x","method":"no.such.method"}"#)
                    .expect("frame")
            })
            .collect();
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            tokio::io::AsyncWriteExt::write_all(&mut deaf.write, &flood),
        )
        .await;
        let stopped = tokio::time::timeout(PATIENCE, harness.stop()).await;
        assert!(
            stopped.is_ok(),
            "stop returned with a deaf client connected"
        );
        drop(deaf);
    }
}
