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
        assert!(matches!(self.read().await, Some(Frame::Hello(_))));
        self.write(&json!({
            "type": "hello_response",
            "ipc_version": {"major": 2, "minor": 0},
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
