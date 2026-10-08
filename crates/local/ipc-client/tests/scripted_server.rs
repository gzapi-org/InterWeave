// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The client against a server that is a script: frames written by hand,
//! so each case reaches exactly the state it names. The conformance suite
//! (`tests/local-client-conformance`, the IPC runner) is where the client
//! meets the real server; these are the behaviours of the client itself.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Duration;

use interweave_ipc_client::{IpcBinding, SocketPaths};
use interweave_ipc_protocol::{DecodedFrame, Frame, FrameError, decode_frame, encode_frame};
use interweave_local_client_api::{
    DataCapability, DataSessionBinding as _, DataSessionPort as _, SessionRequest,
};
use interweave_transport_api::{ChannelId, EndpointId};
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};

const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
const PATIENCE: Duration = Duration::from_secs(5);

struct Script {
    _root: tempfile::TempDir,
    listener: UnixListener,
    admin: UnixListener,
    binding: IpcBinding,
}

impl Script {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let data: PathBuf = root.path().join("data.sock");
        let listener = UnixListener::bind(&data).expect("binds");
        let admin_path = root.path().join("admin.sock");
        let admin = UnixListener::bind(&admin_path).expect("binds");
        let binding = IpcBinding::new(
            SocketPaths {
                data,
                admin: admin_path,
            },
            "scripted-admin",
        );
        Self {
            _root: root,
            listener,
            admin,
            binding,
        }
    }
}

/// The server's end of one connection.
struct Server {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Server {
    async fn accept(listener: &UnixListener) -> Self {
        let (stream, _) = listener.accept().await.expect("accepts");
        Self {
            stream,
            buf: Vec::new(),
        }
    }

    async fn write(&mut self, body: &serde_json::Value) {
        let frame = encode_frame(&body.to_string()).expect("a frame");
        self.stream.write_all(&frame).await.expect("written");
    }

    /// The client's next frame, or `None` at its end of stream.
    async fn read(&mut self) -> Option<Frame> {
        tokio::time::timeout(PATIENCE, async {
            loop {
                match decode_frame(&self.buf) {
                    Ok(DecodedFrame { body, consumed }) => {
                        self.buf.drain(..consumed);
                        return Some(Frame::parse(&body).expect("the client writes frames"));
                    }
                    Err(FrameError::Incomplete { .. }) => {}
                    Err(e) => panic!("a bad frame: {e:?}"),
                }
                let mut chunk = [0_u8; 4096];
                let n = self.stream.read(&mut chunk).await.expect("reads");
                if n == 0 {
                    return None;
                }
                self.buf.extend_from_slice(&chunk[..n]);
            }
        })
        .await
        .expect("the client writes in time")
    }

    /// Read the hello and grant `human` with an event queue of `bound`.
    async fn grant(&mut self, bound: u32) {
        self.grant_with(bound, &["events", "commands"]).await;
    }

    async fn grant_with(&mut self, bound: u32, capabilities: &[&str]) {
        self.grant_at(0, bound, capabilities).await;
    }

    /// Grant as [`Server::grant_with`], selecting IPC 2.`minor`.
    async fn grant_at(&mut self, minor: u64, bound: u32, capabilities: &[&str]) {
        assert!(matches!(self.read().await, Some(Frame::Hello(_))));
        self.write(&json!({
            "type": "hello_response",
            "ipc_version": {"major": 2, "minor": minor},
            "transport_contract_version": "2.0",
            "peer": PEER,
            "endpoint": "human",
            "endpoint_lease_epoch": "AAAAAAAAAAAAAAAAAAAAAQ",
            "event_queue": bound,
            "granted_capabilities": capabilities
        }))
        .await;
    }

    async fn event(&mut self, sequence: u64) {
        self.write(&json!({
            "type": "event", "sequence": sequence, "event_type": "peer.disconnected",
            "data": {"peer": PEER, "reason_class": "policy"}
        }))
        .await;
    }

    /// The next request's id.
    async fn request_id(&mut self) -> String {
        match self.read().await {
            Some(Frame::Request(request)) => request.id.as_str().to_owned(),
            other => panic!("a request, got {other:?}"),
        }
    }
}

fn request() -> SessionRequest {
    SessionRequest::new(
        "k",
        Some(EndpointId::parse("human").expect("endpoint")),
        [DataCapability::Events, DataCapability::Commands],
    )
    .expect("a request")
}

fn general() -> ChannelId {
    ChannelId::parse("general").expect("channel")
}

/// The receive buffer holds the granted `event_queue`, and the reader one
/// more in hand when it pauses: past that the client stops reading, so a
/// response the server wrote BEHIND the undrained events waits for them,
/// and arrives once they are taken (LOCAL-IPC.md, A 2026-09-30).
#[tokio::test]
async fn a_full_receive_buffer_holds_the_response_behind_it_until_drained() {
    let script = Script::new();
    let (session, mut server) = tokio::join!(script.binding.open(request()), async {
        let mut server = Server::accept(&script.listener).await;
        server.grant(2).await;
        server
    });
    let session = session.expect("opens");
    assert_eq!(session.session().event_queue(), 2, "the granted bound");
    // Three events -- one past the bound -- then the join's answer.
    for sequence in 0..3 {
        server.event(sequence).await;
    }
    let join = session.join(general());
    tokio::pin!(join);
    let id = tokio::select! {
        id = server.request_id() => id,
        _ = &mut join => panic!("answered before the server wrote an answer"),
    };
    server
        .write(&json!({"type": "response", "id": id, "ok": true, "result": {}}))
        .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut join)
            .await
            .is_err(),
        "the answer waits behind the third event while the buffer is full"
    );
    let taken = session.events(usize::MAX).await.expect("events");
    assert_eq!(taken.len(), 2, "exactly the bound was held: {taken:?}");
    tokio::time::timeout(PATIENCE, &mut join)
        .await
        .expect("drained, the reader goes on to the answer")
        .expect("joined");
    let rest = session.events(usize::MAX).await.expect("events");
    assert_eq!(rest.len(), 1, "the third event, read after the drain");
}

/// A call dropped before its answer tells the server, with its own id.
#[tokio::test]
async fn a_dropped_call_is_cancelled_by_id() {
    let script = Script::new();
    let (session, mut server) = tokio::join!(script.binding.open(request()), async {
        let mut server = Server::accept(&script.listener).await;
        server.grant(8).await;
        server
    });
    let session = session.expect("opens");
    let id = {
        let join = session.join(general());
        tokio::pin!(join);
        tokio::select! {
            id = server.request_id() => id,
            _ = &mut join => panic!("answered with no answer written"),
        }
        // `join` is dropped here, unanswered.
    };
    match server.read().await {
        Some(Frame::Cancel(cancel)) => assert_eq!(cancel.id.as_str(), id),
        other => panic!("a cancel for {id}, got {other:?}"),
    }
}

/// `close` returns only once the server has closed its side -- which it
/// does after releasing the session's lease -- so a client reopening on
/// its return finds the lease free.
#[tokio::test]
async fn close_waits_for_the_server_to_close() {
    const SERVER_TAKES: Duration = Duration::from_millis(400);
    let script = Script::new();
    let (session, mut server) = tokio::join!(script.binding.open(request()), async {
        let mut server = Server::accept(&script.listener).await;
        server.grant(8).await;
        server
    });
    let session = session.expect("opens");
    let started = tokio::time::Instant::now();
    // Timed inside its own branch: `join!` waits for the server's branch
    // too, so the time after it would be the server's, not close's.
    let (took, ()) = tokio::join!(
        async {
            session.close().await.expect("closes");
            started.elapsed()
        },
        async {
            assert!(server.read().await.is_none(), "the client shut its side");
            tokio::time::sleep(SERVER_TAKES).await;
            drop(server);
        }
    );
    assert!(
        took >= SERVER_TAKES,
        "close returned before the server closed: {took:?}"
    );
}

/// Open a session against a script that grants with `bound` and
/// `capabilities`, returning both ends.
async fn opened(
    script: &Script,
    bound: u32,
    capabilities: &[&str],
) -> (interweave_ipc_client::IpcSession, Server) {
    let (session, server) = tokio::join!(script.binding.open(request()), async {
        let mut server = Server::accept(&script.listener).await;
        server.grant_with(bound, capabilities).await;
        server
    });
    (session.expect("opens"), server)
}

/// A call made after the connection ended is answered at once with the
/// end, not registered where no answer can come.
#[tokio::test]
async fn a_call_on_an_ended_connection_is_refused_not_left_waiting() {
    let script = Script::new();
    let (session, server) = opened(&script, 8, &["events", "commands"]).await;
    drop(server);
    // Until the reader has seen the end, a call may still go out and be
    // answered by it; either way it must come back.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let answer = tokio::time::timeout(PATIENCE, session.join(general()))
            .await
            .expect("a call on an ended connection comes back");
        assert!(answer.is_err(), "no server, no join: {answer:?}");
        if session.events(1).await.is_err() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the end was never seen"
        );
    }
    assert_eq!(
        session.close().await,
        Err(interweave_transport_api::TransportError::BackendUnavailable),
        "closing an ended connection reports it"
    );
}

/// A write that fails ends the connection then and there, before the
/// reader sees anything: the server here stops reading and keeps its
/// write half open, so the client's next write fails and its reader goes
/// on waiting. The call's error and the session's end are one fact
/// (#199's carried risk: in between, `events(0)` read live, so a caller
/// asking whether the session ended took the failure for a refusal).
/// The control is the session reading live before the write.
#[tokio::test]
async fn a_failed_write_ends_the_session_before_the_reader_sees_it() {
    let script = Script::new();
    let (session, server) = opened(&script, 8, &["events", "commands"]).await;
    assert!(session.events(0).await.is_ok(), "the control: live");
    let held = server.stream.into_std().expect("a std stream");
    held.shutdown(std::net::Shutdown::Read)
        .expect("the server stops reading");
    // A session already waiting in `ready` is woken by the end too.
    let (ready, answer) = tokio::join!(
        tokio::time::timeout(PATIENCE, session.ready()),
        tokio::time::timeout(PATIENCE, session.join(general())),
    );
    assert_eq!(
        answer.expect("the call comes back"),
        Err(interweave_transport_api::TransportError::BackendUnavailable)
    );
    ready.expect("ready is woken by the end").ok();
    assert_eq!(
        session.events(0).await.map(|e| e.len()),
        Err(interweave_transport_api::TransportError::BackendUnavailable),
        "ended as the call failed"
    );
    drop(held);
}

/// The server's own `close` decides the code the session ends with, even
/// when a write of the client's fails against the socket the server is
/// closing first (#224 review B F1): the writer's end is provisional and
/// the `close` read after it replaces it. The call that met the failed
/// write answers that same code, not the writer's (architect-cto,
/// 01a11be2): it is answered when the reader has read what arrived.
/// Current-thread, so the writer runs before the reader is polled -- the
/// order that loses the code. The control is the same script with no
/// `close` written: the call and the end are `BackendUnavailable`.
#[tokio::test(flavor = "current_thread")]
async fn a_failed_write_keeps_the_servers_close_code() {
    use interweave_transport_api::TransportError;
    for (close, expected) in [
        (true, TransportError::ShuttingDown),
        (false, TransportError::BackendUnavailable),
    ] {
        let script = Script::new();
        let (session, mut server) = opened(&script, 8, &["events", "commands"]).await;
        if close {
            server
                .write(&json!({"type": "close", "code": "ShuttingDown"}))
                .await;
        }
        drop(server);
        let answer = tokio::time::timeout(PATIENCE, session.join(general()))
            .await
            .expect("the call comes back");
        assert_eq!(answer, Err(expected), "the call answers the session's end");
        assert_eq!(
            session.events(0).await.map(|e| e.len()),
            Err(expected),
            "the session ends with the same code"
        );
    }
}

/// After a failed write the reader does not wait for room: with the
/// buffer full and an event in hand, the reading ends there and the call
/// is answered, though the server's `close` sits unread behind that event
/// -- a session that never drains would otherwise hold the call for good.
/// What fitted before the end is still delivered, then the end.
#[tokio::test]
async fn a_failed_write_with_a_full_buffer_answers_without_waiting_for_room() {
    use interweave_transport_api::TransportError;
    let script = Script::new();
    let (session, mut server) = opened(&script, 1, &["events", "commands"]).await;
    server.event(0).await;
    server.event(1).await;
    server
        .write(&json!({"type": "close", "code": "ShuttingDown"}))
        .await;
    drop(server);
    let answer = tokio::time::timeout(PATIENCE, session.join(general()))
        .await
        .expect("the call comes back without the buffer drained");
    assert_eq!(answer, Err(TransportError::BackendUnavailable));
    assert_eq!(
        session.events(usize::MAX).await.map(|e| e.len()),
        Ok(1),
        "what fitted is delivered"
    );
    assert_eq!(
        session.events(usize::MAX).await.map(|e| e.len()),
        Err(TransportError::BackendUnavailable),
        "then the end"
    );
}

/// A server granting more than a session may hold is capped, not trusted.
#[tokio::test]
async fn a_grant_past_the_session_ceiling_is_capped() {
    let script = Script::new();
    let (session, _server) = opened(&script, 5_000, &["events", "commands"]).await;
    assert_eq!(
        session.session().event_queue(),
        interweave_local_client_api::MAX_EVENT_QUEUE
    );
}

/// An event pushed to a session not granted `events` could never be
/// drained, so it ends the connection as the server's protocol violation.
#[tokio::test]
async fn an_event_to_a_session_without_events_is_a_protocol_violation() {
    let script = Script::new();
    let (session, mut server) = opened(&script, 8, &["commands"]).await;
    server.event(0).await;
    assert_eq!(
        server.read().await.map(|_| ()),
        None,
        "the client ended the connection"
    );
    assert_eq!(
        session.join(general()).await,
        Err(interweave_transport_api::TransportError::ProtocolViolation)
    );
}

/// A dropped admin port ends its connection.
#[tokio::test]
async fn a_dropped_admin_port_ends_its_connection() {
    use interweave_local_client_api::{AdminBinding as _, AdminCapability};
    let script = Script::new();
    let (admin, mut server) = tokio::join!(
        script.binding.admin([AdminCapability::Status].into()),
        async {
            let (stream, _) = script.admin.accept().await.expect("accepts");
            let mut server = Server {
                stream,
                buf: Vec::new(),
            };
            assert!(matches!(server.read().await, Some(Frame::Hello(_))));
            server
                .write(&json!({
                    "type": "hello_response",
                    "ipc_version": {"major": 2, "minor": 0},
                    "transport_contract_version": "2.0",
                    "peer": PEER,
                    "granted_capabilities": ["admin.status"]
                }))
                .await;
            server
        }
    );
    drop(admin.expect("an admin port"));
    assert!(server.read().await.is_none(), "the port's connection ended");
}

/// A ping is echoed with its own nonce.
#[tokio::test]
async fn a_ping_is_echoed_with_its_nonce() {
    let script = Script::new();
    let (_session, mut server) = opened(&script, 8, &["events", "commands"]).await;
    for nonce in ["AAAAAAAAAAAAAAAAAAAAAA", "BBBBBBBBBBBBBBBBBBBBBB"] {
        server.write(&json!({"type": "ping", "nonce": nonce})).await;
        match server.read().await {
            Some(Frame::Pong(pong)) => {
                assert_eq!(
                    serde_json::to_value(&pong).expect("ser")["nonce"],
                    nonce,
                    "the echo carries the ping's own nonce"
                );
            }
            other => panic!("a pong, got {other:?}"),
        }
    }
}

/// A server that ends the connection itself with a `close` frame -- here
/// answering the client's Finish, after `close` asked -- is reported by
/// `close`, however the timing falls.
#[tokio::test]
async fn close_reports_a_server_end_even_after_it_asked() {
    let script = Script::new();
    let (session, mut server) = opened(&script, 8, &["events", "commands"]).await;
    let (closed, ()) = tokio::join!(session.close(), async {
        assert!(server.read().await.is_none(), "the client sent its Finish");
        server
            .write(&json!({"type": "close", "code": "ShuttingDown"}))
            .await;
        drop(server);
    });
    assert_eq!(
        closed,
        Err(interweave_transport_api::TransportError::ShuttingDown),
        "the server's own end, not the answer to the Finish"
    );
}

/// `status_result` hands back the result object whole: the IPC server's
/// counters, which the neutral `AdminStatus` has no field for, arrive as
/// the daemon sent them.
#[tokio::test]
async fn the_status_result_carries_the_servers_counters() {
    use interweave_local_client_api::{AdminBinding as _, AdminCapability};
    let script = Script::new();
    let (admin, mut server) = tokio::join!(
        script.binding.admin([AdminCapability::Status].into()),
        async {
            let (stream, _) = script.admin.accept().await.expect("accepts");
            let mut server = Server {
                stream,
                buf: Vec::new(),
            };
            assert!(matches!(server.read().await, Some(Frame::Hello(_))));
            server
                .write(&json!({
                    "type": "hello_response",
                    "ipc_version": {"major": 2, "minor": 0},
                    "transport_contract_version": "2.0",
                    "peer": PEER,
                    "granted_capabilities": ["admin.status"]
                }))
                .await;
            server
        }
    );
    let admin = admin.expect("an admin port");
    let (status, ()) = tokio::join!(admin.status_result(), async {
        let id = server.request_id().await;
        server
            .write(
                &json!({"type": "response", "id": id, "ok": true, "result": {
                    "health": "healthy",
                    "peer": PEER,
                    "connectivity": {
                        "direct_inbound": "unknown",
                        "relay_inbound": "unavailable",
                        "active_relay_reservations": 0,
                        "target_relay_reservations": 0,
                        "active_relayed_peer_paths": 0,
                        "hole_punch_inflight": 0,
                        "preferred_path_policy": "direct_first",
                        "updated_at": 0
                    },
                    "ipc": {
                        "data_connections": 3,
                        "admin_connections": 1,
                        "active_leases": 2,
                        "cross_domain_capability_denied_total": 5,
                        "peer_credential_refused_total": 7
                    }
                }}),
            )
            .await;
    });
    let status = status.expect("answered");
    assert_eq!(
        (
            status.ipc.data_connections,
            status.ipc.cross_domain_capability_denied_total,
            status.ipc.peer_credential_refused_total,
        ),
        (3, 5, 7),
        "the server's counters, whole"
    );
}

/// A shutdown with no grace sends none -- the daemon's default -- and one
/// with a grace sends it, capped at the wire's ceiling.
#[tokio::test]
async fn a_shutdown_without_a_grace_leaves_it_to_the_daemon() {
    use interweave_local_client_api::{AdminBinding as _, AdminCapability, AdminPort as _};
    let script = Script::new();
    let (admin, mut server) = tokio::join!(
        script.binding.admin([AdminCapability::Shutdown].into()),
        async {
            let (stream, _) = script.admin.accept().await.expect("accepts");
            let mut server = Server {
                stream,
                buf: Vec::new(),
            };
            assert!(matches!(server.read().await, Some(Frame::Hello(_))));
            server
                .write(&json!({
                    "type": "hello_response",
                    "ipc_version": {"major": 2, "minor": 0},
                    "transport_contract_version": "2.0",
                    "peer": PEER,
                    "granted_capabilities": ["admin.shutdown"]
                }))
                .await;
            server
        }
    );
    let admin = admin.expect("an admin port");
    let mut sent = Vec::new();
    for grace in [None, Some(Duration::from_secs(3600))] {
        let (asked, ()) = tokio::join!(admin.request_shutdown(grace), async {
            let Some(Frame::Request(request)) = server.read().await else {
                panic!("a request");
            };
            sent.push(
                request
                    .params
                    .as_ref()
                    .map(|p| p.get().to_owned())
                    .unwrap_or_default(),
            );
            server
                .write(&json!({"type": "response", "id": request.id.as_str(), "ok": true, "result": {}}))
                .await;
        });
        asked.expect("asked");
    }
    let (asked, ()) = tokio::join!(admin.shutdown(Duration::from_millis(250)), async {
        let Some(Frame::Request(request)) = server.read().await else {
            panic!("a request");
        };
        sent.push(
            request
                .params
                .as_ref()
                .map(|p| p.get().to_owned())
                .unwrap_or_default(),
        );
        server
            .write(
                &json!({"type": "response", "id": request.id.as_str(), "ok": true, "result": {}}),
            )
            .await;
    });
    asked.expect("asked");
    assert_eq!(
        sent,
        ["{}", r#"{"grace_ms":600000}"#, r#"{"grace_ms":250}"#],
        "absent, capped, as given"
    );
}

/// Every frame written before a ping has been read once its pong comes
/// back: the reader handles frames in order.
async fn settled(server: &mut Server) {
    server
        .write(&json!({"type": "ping", "nonce": "SSSSSSSSSSSSSSSSSSSSSS"}))
        .await;
    assert!(matches!(server.read().await, Some(Frame::Pong(_))));
}

/// The server's `server_state` is held as the session's newest
/// `ServerState`, replaced and never queued: three pushed unread are one,
/// the last, taken ahead of the events buffered beside it, and taken once.
#[tokio::test]
async fn the_newest_server_state_is_held_once() {
    use interweave_local_client_api::{LocalSessionEvent, SessionEvent};
    use interweave_transport_api::Health;
    let script = Script::new();
    let (session, mut server) = opened(&script, 8, &["events", "commands"]).await;
    for health in ["healthy", "degraded", "unavailable"] {
        server
            .write(&json!({"type": "server_state", "health": health}))
            .await;
    }
    server.event(0).await;
    settled(&mut server).await;
    let taken = session.events(usize::MAX).await.expect("events");
    let states: Vec<Health> = taken
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Local(LocalSessionEvent::ServerState {
                health,
                connectivity,
            }) => {
                assert!(connectivity.is_none(), "none was sent");
                Some(*health)
            }
            _ => None,
        })
        .collect();
    assert_eq!(states, [Health::Unavailable], "{taken:?}");
    assert!(
        matches!(
            taken.as_slice(),
            [
                SessionEvent::Local(LocalSessionEvent::ServerState { .. }),
                SessionEvent::Local(LocalSessionEvent::PeerDisconnected { .. })
            ]
        ),
        "the state first, then the event: {taken:?}"
    );
    assert!(session.events(usize::MAX).await.expect("events").is_empty());
}

/// `ready` waits while nothing is owed, resolves on an event buffered
/// without taking it -- `events` after it takes exactly that event -- and
/// resolves at the connection's end, where `events` reports it.
#[tokio::test]
async fn ready_waits_wakes_without_taking_and_resolves_at_the_end() {
    let script = Script::new();
    let (session, mut server) = opened(&script, 8, &["events", "commands"]).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), session.ready())
            .await
            .is_err(),
        "nothing owed: it waits"
    );
    let waiting = session.ready();
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut waiting)
            .await
            .is_err()
    );
    server.event(0).await;
    tokio::time::timeout(PATIENCE, &mut waiting)
        .await
        .expect("woken by the event")
        .expect("ready");
    // Taken nothing: it is still there, once.
    tokio::time::timeout(PATIENCE, session.ready())
        .await
        .expect("still owed")
        .expect("ready");
    assert_eq!(session.events(usize::MAX).await.expect("events").len(), 1);

    drop(server);
    tokio::time::timeout(PATIENCE, session.ready())
        .await
        .expect("the end ends the wait")
        .expect("ready");
    assert!(session.events(1).await.is_err(), "and events reports it");
}

/// A session not granted `events` has nothing to wait for.
#[tokio::test]
async fn ready_needs_events() {
    let script = Script::new();
    let (session, _server) = opened(&script, 8, &["commands"]).await;
    assert_eq!(
        session.ready().await,
        Err(interweave_transport_api::TransportError::CapabilityDenied)
    );
}

/// `ready` takes `&self`, so two tasks may wait on one session: the
/// connection's end ends BOTH waits, not the first alone.
#[tokio::test]
async fn every_concurrent_ready_ends_with_the_connection() {
    let script = Script::new();
    let (session, server) = opened(&script, 8, &["events", "commands"]).await;
    let (a, b) = (session.ready(), session.ready());
    tokio::pin!(a);
    tokio::pin!(b);
    for wait in [a.as_mut(), b.as_mut()] {
        assert!(
            tokio::time::timeout(Duration::from_millis(50), wait)
                .await
                .is_err(),
            "both wait while nothing is owed"
        );
    }
    drop(server);
    let both = async { tokio::join!(a, b) };
    let (a, b) = tokio::time::timeout(PATIENCE, both)
        .await
        .expect("the end ends every wait");
    assert!(a.is_ok() && b.is_ok());
}

/// A `peer.path_changed` event (2.1) reads back as the session's
/// `PeerPathChanged`, its fields as the server sent them.
#[tokio::test]
async fn a_path_change_reads_back_as_the_sessions_notice() {
    use interweave_local_client_api::{LocalSessionEvent, SessionEvent};
    use interweave_transport_api::PeerPath;
    let script = Script::new();
    let (session, mut server) = opened_at(&script, 1).await;
    server.write(&path_changed()).await;
    settled(&mut server).await;
    let taken = session.events(usize::MAX).await.expect("events");
    assert!(
        matches!(
            taken.as_slice(),
            [SessionEvent::Local(LocalSessionEvent::PeerPathChanged {
                previous: PeerPath::Relayed,
                current: PeerPath::Direct,
                observed_at: 7,
                reason_class,
                ..
            })] if reason_class == "dcutr"
        ),
        "{taken:?}"
    );
}

/// The same event on a connection that selected 2.0 ends it as the
/// server's violation: a type is accepted only at or above the minor
/// that introduced it (LOCAL-IPC.md §Version negotiation).
#[tokio::test]
async fn a_path_change_on_a_2_0_connection_is_the_servers_violation() {
    let script = Script::new();
    let (session, mut server) = opened_at(&script, 0).await;
    server.write(&path_changed()).await;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let ended = loop {
        match session.events(usize::MAX).await {
            Err(code) => break code,
            Ok(taken) => assert!(taken.is_empty(), "nothing read: {taken:?}"),
        }
        assert!(tokio::time::Instant::now() < deadline, "never ended");
        tokio::task::yield_now().await;
    };
    assert_eq!(
        ended,
        interweave_transport_api::TransportError::ProtocolViolation
    );
}

/// A session opened with events, the server selecting IPC 2.`minor`.
async fn opened_at(script: &Script, minor: u64) -> (interweave_ipc_client::IpcSession, Server) {
    let (session, server) = tokio::join!(script.binding.open(request()), async {
        let mut server = Server::accept(&script.listener).await;
        server.grant_at(minor, 8, &["events", "commands"]).await;
        server
    });
    (session.expect("opens"), server)
}

fn path_changed() -> serde_json::Value {
    json!({
        "type": "event", "sequence": 0, "event_type": "peer.path_changed",
        "data": {"peer": PEER, "previous": "relayed", "current": "direct",
                 "reason_class": "dcutr", "observed_at": 7}
    })
}

/// The server's end of the next admin connection, its hello read: the
/// capabilities it asked, in wire order.
async fn admin_hello(script: &Script) -> (Server, Vec<String>) {
    // Bounded: a client that never connects is the result, not a hang.
    let (stream, _) = tokio::time::timeout(PATIENCE, script.admin.accept())
        .await
        .expect("the client connects in time")
        .expect("accepts");
    let mut server = Server {
        stream,
        buf: Vec::new(),
    };
    let Some(Frame::Hello(hello)) = server.read().await else {
        panic!("a hello");
    };
    let asked = hello
        .requested_capabilities
        .iter()
        .map(|c| {
            serde_json::to_value(c)
                .expect("ser")
                .as_str()
                .expect("a name")
                .to_owned()
        })
        .collect();
    (server, asked)
}

async fn admin_response(server: &mut Server, minor: u64, granted: &[&str]) {
    server
        .write(&json!({
            "type": "hello_response",
            "ipc_version": {"major": 2, "minor": minor},
            "transport_contract_version": "2.0",
            "peer": PEER,
            "granted_capabilities": granted
        }))
        .await;
}

/// `admin.trust` is 2.1's (LOCAL-IPC.md §Version negotiation): the first
/// hello names only 2.0 capabilities, and the real one follows once the
/// daemon has said it selects 2.1. A second port on the same binding asks
/// directly; when that hello is closed `ProtocolViolation` -- the daemon
/// restarted at 2.0 -- the minor is learnt again and the port opened
/// without the capability, which it then does not hold.
#[tokio::test]
async fn a_trust_port_probes_once_and_relearns_after_a_refusal() {
    use interweave_local_client_api::{AdminBinding as _, AdminCapability, AdminPort as _};
    let script = Script::new();
    let wanted = || [AdminCapability::Status, AdminCapability::Trust].into();

    let (admin, _held) = tokio::join!(script.binding.admin(wanted()), async {
        let (mut probe, asked) = admin_hello(&script).await;
        assert_eq!(asked, ["admin.status"], "the probe names only 2.0's");
        admin_response(&mut probe, 1, &["admin.status"]).await;
        assert!(probe.read().await.is_none(), "the probe is closed");
        drop(probe);
        let (mut real, asked) = admin_hello(&script).await;
        assert_eq!(asked, ["admin.status", "admin.trust"]);
        admin_response(&mut real, 1, &["admin.status", "admin.trust"]).await;
        real
    });
    assert!(admin.expect("a port").port().holds(AdminCapability::Trust));

    let (admin, _held) = tokio::join!(script.binding.admin(wanted()), async {
        let (mut stale, asked) = admin_hello(&script).await;
        assert_eq!(asked, ["admin.status", "admin.trust"], "learnt: no probe");
        stale
            .write(&json!({"type": "close", "code": "ProtocolViolation"}))
            .await;
        drop(stale);
        let (mut probe, asked) = admin_hello(&script).await;
        assert_eq!(asked, ["admin.status"], "learnt again");
        admin_response(&mut probe, 0, &["admin.status"]).await;
        assert!(probe.read().await.is_none());
        drop(probe);
        let (mut real, asked) = admin_hello(&script).await;
        assert_eq!(asked, ["admin.status"], "a 2.0 daemon is not asked for it");
        admin_response(&mut real, 0, &["admin.status"]).await;
        real
    });
    let admin = admin.expect("a port");
    assert!(!admin.port().holds(AdminCapability::Trust));
    assert!(admin.port().holds(AdminCapability::Status), "the control");
}

/// `trust` reads every page through its cursor and joins them, the local
/// peer from the first; a daemon whose cursor does not advance is not
/// followed.
#[tokio::test]
async fn trust_reads_every_page_and_refuses_a_cursor_that_does_not_move() {
    use interweave_local_client_api::{AdminBinding as _, AdminCapability, AdminPort as _};
    use interweave_transport_api::{TransportError, TransportIdentity};
    const OTHER: &str = "QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N";
    const THIRD: &str = "QmZyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N";
    let script = Script::new();
    let (admin, mut server) = tokio::join!(
        script.binding.admin([AdminCapability::Trust].into()),
        async {
            let (mut probe, _) = admin_hello(&script).await;
            admin_response(&mut probe, 3, &[]).await;
            assert!(probe.read().await.is_none());
            drop(probe);
            let (mut real, _) = admin_hello(&script).await;
            admin_response(&mut real, 3, &["admin.trust"]).await;
            real
        }
    );
    let admin = admin.expect("a port");
    let answer = async |server: &mut Server, result: serde_json::Value| -> Option<String> {
        let Some(Frame::Request(request)) = server.read().await else {
            panic!("a request");
        };
        let after = request.params.as_ref().map(|p| p.get().to_owned());
        server
            .write(&json!({"type": "response", "id": request.id.as_str(), "ok": true, "result": result}))
            .await;
        after
    };
    let (view, afters) = tokio::join!(admin.trust(), async {
        let first = answer(
            &mut server,
            json!({"local_peer": PEER, "allowed": [{"peer": OTHER, "persisted": true, "source": "configured"}], "next": OTHER}),
        )
        .await;
        let second = answer(
            &mut server,
            json!({"allowed": [{"peer": THIRD, "persisted": true, "source": "administered"}]}),
        )
        .await;
        (first, second)
    });
    let view = view.expect("the policy");
    let id = |s: &str| TransportIdentity::parse(s).expect("peer");
    assert_eq!(view.local_peer, Some(id(PEER)));
    assert_eq!(
        view.allowed,
        [
            interweave_local_client_api::TrustedPeer {
                peer: id(OTHER),
                persisted: true,
                source: interweave_local_client_api::TrustSource::Configured,
            },
            interweave_local_client_api::TrustedPeer {
                peer: id(THIRD),
                persisted: true,
                source: interweave_local_client_api::TrustSource::Administered,
            },
        ]
    );
    assert_eq!(afters.0.as_deref(), Some("{}"), "the first page names none");
    assert_eq!(afters.1, Some(format!(r#"{{"after":"{OTHER}"}}"#)));

    let (stuck, ()) = tokio::join!(tokio::time::timeout(PATIENCE, admin.trust()), async {
        for _ in 0..2 {
            answer(
                &mut server,
                json!({"allowed": [{"peer": OTHER, "persisted": true, "source": "configured"}], "next": OTHER}),
            )
            .await;
        }
    });
    assert_eq!(
        stuck.expect("refused, not followed"),
        Err(TransportError::Internal)
    );
}

/// A binding that learnt 2.0 is not stuck with it: the next port that
/// wants `admin.trust` probes again, and once the daemon -- upgraded --
/// selects 2.1, asks for it.
#[tokio::test]
async fn a_daemon_upgraded_since_the_probe_is_asked_again() {
    use interweave_local_client_api::{AdminBinding as _, AdminCapability, AdminPort as _};
    let script = Script::new();
    let wanted = || [AdminCapability::Status, AdminCapability::Trust].into();
    let (admin, _held) = tokio::join!(script.binding.admin(wanted()), async {
        let (mut probe, _) = admin_hello(&script).await;
        admin_response(&mut probe, 0, &["admin.status"]).await;
        assert!(probe.read().await.is_none());
        drop(probe);
        let (mut real, asked) = admin_hello(&script).await;
        assert_eq!(asked, ["admin.status"], "a 2.0 daemon is not asked");
        admin_response(&mut real, 0, &["admin.status"]).await;
        real
    });
    assert!(!admin.expect("a port").port().holds(AdminCapability::Trust));

    let (admin, _held) = tokio::join!(script.binding.admin(wanted()), async {
        let (mut probe, asked) = admin_hello(&script).await;
        assert_eq!(asked, ["admin.status"], "probed again, not taken as 2.0");
        admin_response(&mut probe, 1, &["admin.status"]).await;
        assert!(probe.read().await.is_none());
        drop(probe);
        let (mut real, asked) = admin_hello(&script).await;
        assert_eq!(asked, ["admin.status", "admin.trust"]);
        admin_response(&mut real, 1, &["admin.status", "admin.trust"]).await;
        real
    });
    assert!(admin.expect("a port").port().holds(AdminCapability::Trust));
}

/// The selected version is checked, not adopted (LOCAL-IPC.md §Version
/// negotiation): the client offers 2.`IPC_MAX_MINOR`, so a server may
/// select that major and a minor no higher. A minor above the offer
/// (refused by the client) or another major (refused by the frame's
/// parser) opens nothing and is the server's violation; the controls,
/// 2.0 and 2.1, open.
#[tokio::test]
async fn a_selected_version_the_client_did_not_offer_is_a_protocol_violation() {
    use interweave_ipc_protocol::IPC_MAX_MINOR;
    use interweave_transport_api::TransportError;
    let script = Script::new();
    for (major, minor) in [(2, IPC_MAX_MINOR + 1), (3, 0), (1, 0), (2, u64::MAX)] {
        let (session, ()) = tokio::join!(script.binding.open(request()), async {
            let mut server = Server::accept(&script.listener).await;
            assert!(matches!(server.read().await, Some(Frame::Hello(_))));
            server
                .write(&json!({
                    "type": "hello_response",
                    "ipc_version": {"major": major, "minor": minor},
                    "transport_contract_version": "2.0",
                    "peer": PEER,
                    "endpoint": "human",
                    "endpoint_lease_epoch": "AAAAAAAAAAAAAAAAAAAAAQ",
                    "event_queue": 8,
                    "granted_capabilities": ["events", "commands"]
                }))
                .await;
            assert!(server.read().await.is_none(), "{major}.{minor}: ended");
        });
        assert_eq!(
            session.err(),
            Some(TransportError::ProtocolViolation),
            "{major}.{minor}"
        );
    }
    for minor in 0..=IPC_MAX_MINOR {
        // The control: `opened_at` expects it to open.
        let _opened = opened_at(&script, minor).await;
    }
}

/// An admin probe answered with a minor above the offer is refused, and
/// nothing is learnt from it: the next port probes again.
#[tokio::test]
async fn an_admin_answer_above_the_offer_is_refused_and_not_learnt() {
    use interweave_ipc_protocol::IPC_MAX_MINOR;
    use interweave_local_client_api::{AdminBinding as _, AdminCapability, AdminPort as _};
    use interweave_transport_api::TransportError;
    let script = Script::new();
    let wanted = || [AdminCapability::Status, AdminCapability::Trust].into();
    let (admin, ()) = tokio::join!(script.binding.admin(wanted()), async {
        let (mut probe, _) = admin_hello(&script).await;
        admin_response(&mut probe, IPC_MAX_MINOR + 1, &["admin.status"]).await;
        assert!(probe.read().await.is_none(), "ended");
    });
    assert_eq!(admin.err(), Some(TransportError::ProtocolViolation));

    let (admin, _held) = tokio::join!(script.binding.admin(wanted()), async {
        let (mut probe, asked) = admin_hello(&script).await;
        assert_eq!(asked, ["admin.status"], "probed again: nothing was learnt");
        admin_response(&mut probe, 1, &["admin.status"]).await;
        assert!(probe.read().await.is_none());
        drop(probe);
        let (mut real, _) = admin_hello(&script).await;
        admin_response(&mut real, 1, &["admin.status", "admin.trust"]).await;
        real
    });
    assert!(
        admin
            .expect("the control: a port")
            .port()
            .holds(AdminCapability::Trust)
    );
}

/// `peers` reads every page through its cursor on a 2.2 connection and
/// joins them; on a 2.1 connection it is answered `ProtocolUnsupported`
/// and nothing is sent -- the daemon would answer the name as unknown.
#[tokio::test]
async fn peers_reads_every_page_at_two_two_and_sends_nothing_below_it() {
    use interweave_local_client_api::{
        AdminBinding as _, AdminCapability, AdminPort as _, PeerOutcome,
    };
    use interweave_transport_api::{TransportError, TransportIdentity};
    const OTHER: &str = "QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N";
    const THIRD: &str = "QmZyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N";
    for minor in [2, 1] {
        let script = Script::new();
        let (admin, mut server) = tokio::join!(
            script.binding.admin([AdminCapability::Status].into()),
            async {
                let (mut server, _) = admin_hello(&script).await;
                admin_response(&mut server, minor, &["admin.status"]).await;
                server
            }
        );
        let admin = admin.expect("a port");
        if minor < 2 {
            // Bounded: a port that sent the request would wait for an
            // answer this server never gives.
            let (refused, nothing) =
                tokio::join!(tokio::time::timeout(PATIENCE, admin.peers()), async {
                    tokio::time::timeout(Duration::from_millis(200), server.read()).await
                });
            assert_eq!(
                refused.expect("answered without a round trip"),
                Err(TransportError::ProtocolUnsupported)
            );
            assert!(nothing.is_err(), "no request reached the daemon");
            continue;
        }
        let answer = async |server: &mut Server, result: serde_json::Value| -> Option<String> {
            let Some(Frame::Request(request)) = server.read().await else {
                panic!("a request");
            };
            let after = request.params.as_ref().map(|p| p.get().to_owned());
            server
                .write(&json!({"type": "response", "id": request.id.as_str(), "ok": true, "result": result}))
                .await;
            after
        };
        let (rows, afters) = tokio::join!(admin.peers(), async {
            let first = answer(
                &mut server,
                json!({"peers": [{"peer": OTHER, "connected": true, "last_outcome": "connected"}], "next": OTHER}),
            )
            .await;
            let second = answer(
                &mut server,
                json!({"peers": [{"peer": THIRD, "connected": false, "backoff_until": 7, "last_outcome": "denied"}]}),
            )
            .await;
            (first, second)
        });
        let rows = rows.expect("the rows");
        let id = |s: &str| TransportIdentity::parse(s).expect("peer");
        assert_eq!(
            rows.iter().map(|r| r.peer.clone()).collect::<Vec<_>>(),
            [id(OTHER), id(THIRD)]
        );
        assert_eq!(rows[1].backoff_until, Some(7));
        assert_eq!(rows[1].last_outcome, Some(PeerOutcome::Denied));
        assert_eq!(afters.0.as_deref(), Some("{}"), "the first page names none");
        assert_eq!(afters.1, Some(format!(r#"{{"after":"{OTHER}"}}"#)));
    }
}

/// Below 2.3 the trust read is refused `ProtocolUnsupported` and nothing
/// is sent -- the 2.1 row carries no source, and an unknown is never
/// shown as a value -- while `set_trust`, which carries no row, is sent
/// at 2.1 as before. At 2.3 a row without its source is a daemon this
/// client cannot read truthfully: `Internal`, not a guessed source.
#[tokio::test]
async fn trust_is_read_only_at_two_three_and_set_is_sent_below_it() {
    use interweave_local_client_api::{AdminBinding as _, AdminCapability, AdminPort as _};
    use interweave_transport_api::{TransportError, TransportIdentity};
    const OTHER: &str = "QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N";
    for minor in [1, 2, 3] {
        let script = Script::new();
        let (admin, mut server) = tokio::join!(
            script.binding.admin([AdminCapability::Trust].into()),
            async {
                // The binding probes which capabilities the daemon grants,
                // then opens the port it asked for.
                let (mut probe, _) = admin_hello(&script).await;
                admin_response(&mut probe, minor, &[]).await;
                assert!(probe.read().await.is_none());
                drop(probe);
                let (mut server, _) = admin_hello(&script).await;
                admin_response(&mut server, minor, &["admin.trust"]).await;
                server
            }
        );
        let admin = admin.expect("a port");
        if minor < 3 {
            let (refused, nothing) =
                tokio::join!(tokio::time::timeout(PATIENCE, admin.trust()), async {
                    tokio::time::timeout(Duration::from_millis(200), server.read()).await
                });
            assert_eq!(
                refused.expect("answered without the daemon"),
                Err(TransportError::ProtocolUnsupported),
                "2.{minor}"
            );
            assert!(nothing.is_err(), "2.{minor}: no request reached the daemon");
            // The control: the set is still sent, and answered.
            let (set, ()) = tokio::join!(
                tokio::time::timeout(
                    PATIENCE,
                    admin.set_trust(TransportIdentity::parse(OTHER).expect("peer"), true)
                ),
                async {
                    let Some(Frame::Request(request)) = server.read().await else {
                        panic!("the set reaches the daemon");
                    };
                    assert_eq!(request.method.as_str(), "admin.trust.set");
                    server
                        .write(&json!({"type": "response", "id": request.id.as_str(), "ok": true, "result": {}}))
                        .await;
                }
            );
            assert_eq!(set.expect("answered"), Ok(()), "2.{minor}");
            continue;
        }
        let (read, ()) = tokio::join!(tokio::time::timeout(PATIENCE, admin.trust()), async {
            let Some(Frame::Request(request)) = server.read().await else {
                panic!("a request");
            };
            server
                .write(
                    &json!({"type": "response", "id": request.id.as_str(), "ok": true,
                    "result": {"allowed": [{"peer": OTHER, "persisted": false}]}}),
                )
                .await;
        });
        assert_eq!(
            read.expect("answered"),
            Err(TransportError::Internal),
            "a 2.1 row on a 2.3 connection"
        );
    }
}
