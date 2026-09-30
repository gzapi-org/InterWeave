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
    binding: IpcBinding,
}

impl Script {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let data: PathBuf = root.path().join("data.sock");
        let listener = UnixListener::bind(&data).expect("binds");
        let binding = IpcBinding::new(
            SocketPaths {
                data,
                admin: root.path().join("admin.sock"),
            },
            "scripted-admin",
        );
        Self {
            _root: root,
            listener,
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
        assert!(matches!(self.read().await, Some(Frame::Hello(_))));
        self.write(&json!({
            "type": "hello_response",
            "ipc_version": {"major": 2, "minor": 0},
            "transport_contract_version": "2.0",
            "peer": PEER,
            "endpoint": "human",
            "endpoint_lease_epoch": "AAAAAAAAAAAAAAAAAAAAAQ",
            "event_queue": bound,
            "granted_capabilities": ["events", "commands"]
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

/// The receive buffer holds the granted `event_queue` and no more: past
/// it the client stops reading, so a response the server wrote BEHIND the
/// undrained events waits for them, and arrives once they are taken
/// (LOCAL-IPC.md, A 2026-09-30).
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
