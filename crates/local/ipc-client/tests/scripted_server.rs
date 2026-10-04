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
