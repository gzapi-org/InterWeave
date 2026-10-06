// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The wedge #199's review found, over the real IPC binding: a session
//! whose events are not taken stops reading its socket once its event
//! buffer is full, so a call answered behind a burst of events would
//! never return if the bridge stopped draining while it waited. The
//! daemon is a script that grants a two-event queue and answers the
//! bridge's send only after pushing ten events ahead of the answer.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_claude_channel::serve::{Config, Env, serve};
use interweave_ipc_client::{IpcBinding, SocketPaths};
use interweave_ipc_protocol::{DecodedFrame, Frame, FrameError, decode_frame, encode_frame};
use interweave_transport_api::EndpointId;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
const PATIENCE: Duration = Duration::from_secs(10);
/// The granted event queue, and the events pushed ahead of the answer:
/// five times the buffer, so a bridge that does not drain cannot see it.
const QUEUE: u32 = 2;
const BURST: u64 = 10;

struct Daemon {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Daemon {
    async fn write(&mut self, body: &Value) {
        let frame = encode_frame(&body.to_string()).expect("a frame");
        self.stream.write_all(&frame).await.expect("written");
    }

    async fn read(&mut self) -> Frame {
        tokio::time::timeout(PATIENCE, async {
            loop {
                match decode_frame(&self.buf) {
                    Ok(DecodedFrame { body, consumed }) => {
                        self.buf.drain(..consumed);
                        return Frame::parse(&body).expect("the client writes frames");
                    }
                    Err(FrameError::Incomplete { .. }) => {}
                    Err(e) => panic!("a bad frame: {e:?}"),
                }
                let mut chunk = [0_u8; 4096];
                let n = self.stream.read(&mut chunk).await.expect("reads");
                assert!(n > 0, "the client closed");
                self.buf.extend_from_slice(&chunk[..n]);
            }
        })
        .await
        .expect("the client writes in time")
    }
}

#[tokio::test]
async fn a_call_answered_behind_a_burst_of_events_completes() {
    let root = tempfile::tempdir().expect("tempdir");
    let data = root.path().join("data.sock");
    let listener = UnixListener::bind(&data).expect("binds");
    let binding = IpcBinding::new(
        SocketPaths {
            data,
            admin: root.path().join("admin.sock"),
        },
        "claude-channel",
    );
    let (mut host_in, bridge_in) = tokio::io::duplex(1 << 16);
    let (bridge_out, host_out) = tokio::io::duplex(1 << 16);
    let env = Env {
        now_ms: Box::new(|| 0),
        entropy: Box::new(|| [7; 16]),
    };
    let config = Config {
        endpoint: EndpointId::parse("claude").expect("endpoint"),
        desired_channels: Ok(Vec::new()),
    };
    tokio::spawn(serve(
        binding,
        config,
        env,
        BufReader::new(bridge_in),
        bridge_out,
    ));

    let (stream, _) = tokio::time::timeout(PATIENCE, listener.accept())
        .await
        .expect("the bridge connects")
        .expect("accepts");
    let mut daemon = Daemon {
        stream,
        buf: Vec::new(),
    };
    assert!(matches!(daemon.read().await, Frame::Hello(_)));
    daemon
        .write(&json!({
            "type": "hello_response",
            "ipc_version": {"major": 2, "minor": 1},
            "transport_contract_version": "2.0",
            "peer": PEER,
            "endpoint": "claude",
            "endpoint_lease_epoch": "AAAAAAAAAAAAAAAAAAAAAQ",
            "event_queue": QUEUE,
            "granted_capabilities": ["events", "commands"]
        }))
        .await;

    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "send", "arguments": {"peer": PEER, "content": "x"}}});
    host_in
        .write_all(format!("{call}\n").as_bytes())
        .await
        .expect("written");
    let Frame::Request(request) = daemon.read().await else {
        panic!("the send's request")
    };
    for sequence in 0..BURST {
        daemon
            .write(&json!({
                "type": "event", "sequence": sequence, "event_type": "peer.disconnected",
                "data": {"peer": PEER, "reason_class": "policy"}
            }))
            .await;
    }
    daemon
        .write(
            &json!({"type": "response", "id": request.id.as_str(), "ok": true,
                       "result": {"resolved_endpoint": "human"}}),
        )
        .await;

    let mut lines = BufReader::new(host_out).lines();
    let answer = tokio::time::timeout(PATIENCE, async {
        loop {
            let line = lines.next_line().await.expect("readable").expect("a line");
            let value: Value = serde_json::from_str(&line).expect("json");
            if value["id"] == json!(1) {
                return value;
            }
        }
    })
    .await
    .expect("the send answers behind the burst, not never");
    assert_eq!(
        answer["result"]["content"][0]["text"],
        json!("remote transport accepted at endpoint human")
    );
}
