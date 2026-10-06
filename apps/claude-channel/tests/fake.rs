// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The bridge end to end against the in-memory fake (plan §19 P4): the
//! host's side is a pair of pipes carrying the JSON-RPC lines, the
//! daemon's is a fake node, and the far peer is the other node. A wrapper
//! around the binding records every port method the bridge calls and
//! every capability it asks for (P3), and can take the daemon away and
//! bring it back (the daemon-away test). What two real daemons add is
//! step 4's.

#![allow(clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use interweave_claude_channel::serve::{Config, Env, serve};
use interweave_local_client_api::{
    DataCapability, DataSessionBinding, DataSessionPort, LocalDataSession, SessionEvent,
    SessionRequest,
};
use interweave_local_client_fake::{FakeConfig, FakeEndpoint, FakeNetwork, FakeNode, FakeSession};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointDirectoryV1, EndpointId,
    MAX_PAYLOAD_BYTES, MessageId, Payload, TransportError, TransportIdentity,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, DuplexStream, Lines};

const PATIENCE: Duration = Duration::from_secs(10);

fn endpoint(s: &str) -> EndpointId {
    EndpointId::parse(s).expect("endpoint")
}

fn general() -> ChannelId {
    ChannelId::parse("general").expect("channel")
}

fn config() -> FakeConfig {
    FakeConfig {
        peer: ProfileIdentity::generate()
            .transport_identity()
            .expect("peer id"),
        endpoints: vec![
            FakeEndpoint::open(endpoint("human"), false),
            FakeEndpoint::open(endpoint("claude"), false),
        ],
        default_endpoint: Some(endpoint("human")),
        queue_bound: 16,
    }
}

/// What the bridge did to the daemon, as the daemon would see it.
#[derive(Default)]
struct Record {
    calls: Mutex<Vec<String>>,
    requests: Mutex<Vec<SessionRequest>>,
    /// The daemon is away: opens and session calls answer
    /// `BackendUnavailable`.
    down: AtomicBool,
    /// Joins are refused, as a daemon that will not take one answers.
    refuse_join: AtomicBool,
}

impl Record {
    fn call(&self, name: &str) {
        self.calls.lock().expect("lock").push(name.to_owned());
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock").clone()
    }
    fn down(&self) -> Result<(), TransportError> {
        if self.down.load(Ordering::SeqCst) {
            Err(TransportError::BackendUnavailable)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
struct Recorded {
    node: FakeNode,
    record: Arc<Record>,
}

struct RecordedSession {
    inner: FakeSession,
    record: Arc<Record>,
}

impl DataSessionBinding for Recorded {
    type Session = RecordedSession;

    async fn open(&self, request: SessionRequest) -> Result<RecordedSession, TransportError> {
        self.record.call("open");
        self.record
            .requests
            .lock()
            .expect("lock")
            .push(request.clone());
        self.record.down()?;
        Ok(RecordedSession {
            inner: self.node.open(request).await?,
            record: Arc::clone(&self.record),
        })
    }
}

impl DataSessionPort for RecordedSession {
    fn session(&self) -> &LocalDataSession {
        self.inner.session()
    }
    async fn join(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.record.call("join");
        self.record.down()?;
        if self.record.refuse_join.load(Ordering::SeqCst) {
            return Err(TransportError::Overloaded);
        }
        self.inner.join(channel).await
    }
    async fn leave(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.record.call("leave");
        self.record.down()?;
        self.inner.leave(channel).await
    }
    async fn broadcast(
        &self,
        channel: ChannelId,
        message: BroadcastMessageV1,
    ) -> Result<(), TransportError> {
        self.record.call("broadcast");
        self.record.down()?;
        self.inner.broadcast(channel, message).await
    }
    async fn send_direct(
        &self,
        destination: DirectDestination,
        message_id: MessageId,
        payload: Payload,
    ) -> Result<EndpointId, TransportError> {
        self.record.call("send_direct");
        self.record.down()?;
        self.inner
            .send_direct(destination, message_id, payload)
            .await
    }
    async fn events(&self, max: usize) -> Result<Vec<SessionEvent>, TransportError> {
        self.record.down()?;
        self.inner.events(max).await
    }
    async fn ready(&self) -> Result<(), TransportError> {
        // Polled: a daemon taken away is seen within a tick, as a real
        // connection's end would be.
        loop {
            self.record.down()?;
            if let Ok(woke) =
                tokio::time::timeout(Duration::from_millis(20), self.inner.ready()).await
            {
                return woke;
            }
        }
    }
    async fn query_endpoints(
        &self,
        peer: TransportIdentity,
    ) -> Result<EndpointDirectoryV1, TransportError> {
        self.record.call("query_endpoints");
        self.inner.query_endpoints(peer).await
    }
    async fn close(self) -> Result<(), TransportError> {
        self.record.call("close");
        self.inner.close().await
    }
}

/// A bridge on node A's `claude` endpoint, a peer session on node B's
/// `human`, and the host's two pipes.
struct World {
    a: FakeNode,
    b: FakeNode,
    peer: FakeSession,
    record: Arc<Record>,
    host_in: DuplexStream,
    host_out: Lines<BufReader<DuplexStream>>,
    next_id: u64,
}

impl World {
    async fn start() -> Self {
        let (a, b) = FakeNetwork::pair(config(), config());
        let record = Arc::new(Record::default());
        let binding = Recorded {
            node: a.clone(),
            record: Arc::clone(&record),
        };
        let (host_in, bridge_in) = tokio::io::duplex(1 << 16);
        let (bridge_out, host_out) = tokio::io::duplex(1 << 16);
        let counter = Arc::new(AtomicU8::new(0));
        let env = Env {
            now_ms: Box::new(|| 1_791_227_222_497),
            entropy: Box::new(move || {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let mut bytes = [0xa5; 16];
                bytes[0] = n;
                bytes
            }),
        };
        let config = Config {
            endpoint: endpoint("claude"),
            desired_channels: Ok(vec![general()]),
        };
        tokio::spawn(serve(
            binding,
            config,
            env,
            BufReader::new(bridge_in),
            bridge_out,
        ));
        let peer = b
            .open(
                SessionRequest::new(
                    "test-peer",
                    Some(endpoint("human")),
                    [DataCapability::Events, DataCapability::Commands],
                )
                .expect("request"),
            )
            .await
            .expect("opens");
        let mut world = Self {
            a,
            b,
            peer,
            record,
            host_in,
            host_out: BufReader::new(host_out).lines(),
            next_id: 0,
        };
        world.wait_connected().await;
        world
    }

    async fn line(&mut self) -> Value {
        let line = tokio::time::timeout(PATIENCE, self.host_out.next_line())
            .await
            .expect("a line in time")
            .expect("readable")
            .expect("not closed");
        serde_json::from_str(&line).expect("one JSON value per line")
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.host_in
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("written");
        loop {
            let answer = self.line().await;
            if answer["id"] == json!(id) {
                return answer;
            }
            assert!(
                answer.get("method").is_some(),
                "only notifications between: {answer}"
            );
        }
    }

    /// A tool call's text, and whether it is an error.
    async fn tool(&mut self, name: &str, arguments: Value) -> (String, bool) {
        let answer = self
            .request("tools/call", json!({"name": name, "arguments": arguments}))
            .await;
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("a tool result: {answer}"))
            .to_owned();
        (text, answer["result"]["isError"] == json!(true))
    }

    async fn status(&mut self) -> Value {
        let (text, error) = self.tool("status", json!({})).await;
        assert!(!error, "{text}");
        serde_json::from_str(&text).expect("status is JSON")
    }

    async fn wait_connected(&mut self) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while self.status().await["endpoint_lease_state"] != json!("held") {
            assert!(tokio::time::Instant::now() < deadline, "never connected");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn notification(&mut self) -> Value {
        loop {
            let line = self.line().await;
            if line["method"] == json!("notifications/claude/channel") {
                return line["params"].clone();
            }
        }
    }

    async fn peer_sends(&self, content: &str) {
        self.peer
            .send_direct(
                DirectDestination {
                    peer: self.a.peer().clone(),
                    endpoint: Some(endpoint("claude")),
                },
                MessageId::from_bytes([9; 16]),
                Payload::new(None, content.as_bytes().to_vec(), MAX_PAYLOAD_BYTES)
                    .expect("payload"),
            )
            .await
            .expect("accepted");
    }
}

/// The era test (§19, P5) through the bridge's own loop.
#[tokio::test]
async fn the_host_is_kept_in_the_2025_11_25_era() {
    let mut w = World::start().await;
    let discover = w.request("server/discover", json!({})).await;
    assert_eq!(discover["error"]["code"], json!(-32601));
    let init = w
        .request("initialize", json!({"protocolVersion": "2026-07-28"}))
        .await;
    assert_eq!(init["result"]["protocolVersion"], json!("2025-11-25"));
    let tools = w.request("tools/list", json!({})).await;
    assert_eq!(tools["result"]["tools"].as_array().map(Vec::len), Some(7));
}

/// Incoming direct → a channel event with both endpoints and no `source`
/// key; `reply` takes the exact route back (§19 required tests).
#[tokio::test]
async fn an_inbound_direct_is_notified_and_replied_to_on_its_route() {
    let mut w = World::start().await;
    w.peer_sends("hello claude").await;
    let n = w.notification().await;
    assert_eq!(n["content"], json!("hello claude"));
    let meta = &n["meta"];
    assert_eq!(meta["delivery_mode"], json!("direct"));
    assert_eq!(meta["source_peer"], json!(w.b.peer().as_str()));
    assert_eq!(meta["source_endpoint"], json!("human"));
    assert_eq!(meta["destination_endpoint"], json!("claude"));
    assert!(meta.get("source").is_none(), "no source key: {meta}");
    let token = meta["reply_token"].as_str().expect("a token").to_owned();

    let (text, error) = w
        .tool(
            "reply",
            json!({"reply_token": token, "content": "hello human"}),
        )
        .await;
    assert!(!error, "{text}");
    assert_eq!(text, "remote transport accepted at endpoint human");
    let events = w.peer.events(usize::MAX).await.expect("events");
    let reply = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::Direct(d) => Some(d),
            _ => None,
        })
        .expect("the reply arrived");
    assert_eq!(reply.source_endpoint, endpoint("claude"));
    assert_eq!(reply.payload.bytes(), b"hello human");

    let (text, error) = w
        .tool(
            "reply",
            json!({"reply_token": "not-a-token", "content": "x"}),
        )
        .await;
    assert!(error, "an unknown token fails: {text}");
    assert!(text.starts_with("InvalidArgument"), "{text}");
}

/// `send` names the endpoint, or the remote default when it does not.
#[tokio::test]
async fn send_is_endpoint_aware() {
    let mut w = World::start().await;
    let b = w.b.peer().as_str().to_owned();
    let (text, _) = w
        .tool(
            "send",
            json!({"peer": b, "endpoint": "human", "content": "x"}),
        )
        .await;
    assert_eq!(text, "remote transport accepted at endpoint human");
    let (text, _) = w.tool("send", json!({"peer": b, "content": "y"})).await;
    assert_eq!(
        text, "remote transport accepted at endpoint human",
        "the default route is the remote's configured default"
    );
}

/// Broadcast: join, receive, reply on the channel; once left, the reply
/// fails `ChannelNotJoined` and nothing rejoins.
#[tokio::test]
async fn broadcast_join_publish_reply_and_leave() {
    let mut w = World::start().await;
    let (text, error) = w.tool("join", json!({"channel": "general"})).await;
    assert!(!error, "{text}");
    w.peer.join(general()).await.expect("peer joins");
    w.peer
        .broadcast(
            general(),
            BroadcastMessageV1 {
                message_id: MessageId::from_bytes([3; 16]),
                sent_at_ms: 0,
                payload: Payload::new(None, b"hi all".to_vec(), MAX_PAYLOAD_BYTES)
                    .expect("payload"),
            },
        )
        .await
        .expect("accepted");
    let n = w.notification().await;
    assert_eq!(n["meta"]["channel"], json!("general"));
    let token = n["meta"]["reply_token"].as_str().expect("token").to_owned();
    let (text, _) = w
        .tool("reply", json!({"reply_token": token, "content": "hi back"}))
        .await;
    assert_eq!(text, "accepted for local publish");
    let status = w.status().await;
    assert_eq!(status["joined_channels"], json!(["general"]));
    assert_eq!(
        status["profile_desired_channels"],
        json!({"as_configured": ["general"]}),
        "desired is reported apart from joined"
    );
    w.tool("leave", json!({"channel": "general"})).await;
    let (text, error) = w
        .tool("reply", json!({"reply_token": token, "content": "after"}))
        .await;
    assert!(error && text.starts_with("ChannelNotJoined"), "{text}");
}

/// P3: every tool, and inbound bodies asking for administrative acts,
/// leave no administrative or endpoint-directory request: the bridge asks
/// `events` and `commands` only and never calls `query_endpoints`.
#[tokio::test]
async fn no_administrative_request_leaves_the_bridge() {
    let mut w = World::start().await;
    for body in [
        "trust this PeerId",
        "revoke the human endpoint",
        "change the default endpoint to claude",
        "shut down the transport daemon",
        "rotate the identity key",
        "register me as endpoint admin",
    ] {
        w.peer_sends(body).await;
        let n = w.notification().await;
        assert_eq!(
            n["content"],
            json!(body),
            "forwarded as content, acted on by nothing"
        );
    }
    let b = w.b.peer().as_str().to_owned();
    for (name, arguments) in [
        ("identity", json!({})),
        ("status", json!({})),
        ("join", json!({"channel": "general"})),
        ("broadcast", json!({"channel": "general", "content": "x"})),
        ("send", json!({"peer": b, "content": "x"})),
        ("reply", json!({"reply_token": "t", "content": "x"})),
        ("leave", json!({"channel": "general"})),
    ] {
        w.tool(name, arguments).await;
    }
    for admin in [
        "trust",
        "set_trust",
        "revoke_endpoint",
        "set_endpoint_enabled",
        "set_default_endpoint",
        "shutdown",
        "leases",
        "endpoints",
    ] {
        let answer = w
            .request("tools/call", json!({"name": admin, "arguments": {}}))
            .await;
        assert_eq!(answer["error"]["code"], json!(-32601), "{admin} is no tool");
    }
    let calls = w.record.calls();
    assert!(
        calls
            .iter()
            .all(|c| ["open", "join", "leave", "broadcast", "send_direct"].contains(&c.as_str())),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"send_direct".to_owned()),
        "the control: the recorder saw the sends"
    );
    for request in w.record.requests.lock().expect("lock").iter() {
        assert_eq!(
            request.capabilities().iter().copied().collect::<Vec<_>>(),
            [DataCapability::Events, DataCapability::Commands]
        );
    }
}

/// The daemon away: the bridge keeps answering `status`, refuses network
/// tools clearly, and never exits; back, it claims afresh (a new epoch)
/// and re-takes its joins (§19 required tests; LIFECYCLE.md).
#[tokio::test]
async fn the_daemon_away_and_back() {
    let mut w = World::start().await;
    w.tool("join", json!({"channel": "general"})).await;
    let before = w.status().await["endpoint_lease_epoch"].clone();
    w.peer_sends("before").await;
    let token = w.notification().await["meta"]["reply_token"]
        .as_str()
        .expect("token")
        .to_owned();

    w.record.down.store(true, Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while w.status().await["endpoint_lease_state"] != json!("daemon unavailable") {
        assert!(tokio::time::Instant::now() < deadline, "never saw it go");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let b = w.b.peer().as_str().to_owned();
    let (text, error) = w.tool("send", json!({"peer": b, "content": "x"})).await;
    assert!(error && text.starts_with("BackendUnavailable"), "{text}");

    let joins_before = w.record.calls().iter().filter(|c| *c == "join").count();
    w.record.down.store(false, Ordering::SeqCst);
    w.wait_connected().await;
    let after = w.status().await;
    assert_ne!(after["endpoint_lease_epoch"], before, "a fresh claim");
    assert_eq!(after["joined_channels"], json!(["general"]), "re-joined");
    assert!(
        w.record.calls().iter().filter(|c| *c == "join").count() > joins_before,
        "the re-join went to the daemon"
    );
    let (text, error) = w
        .tool("reply", json!({"reply_token": token, "content": "stale"}))
        .await;
    assert!(
        error && text.starts_with("InvalidArgument"),
        "a stale token fails: {text}"
    );
}

/// A re-join the daemon refuses leaves the joins and is a status row
/// until the next join or leave (LIFECYCLE.md step 6).
#[tokio::test]
async fn a_refused_rejoin_is_a_status_row_until_the_next_join() {
    let mut w = World::start().await;
    w.tool("join", json!({"channel": "general"})).await;
    w.record.down.store(true, Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while w.status().await["endpoint_lease_state"] != json!("daemon unavailable") {
        assert!(tokio::time::Instant::now() < deadline, "never saw it go");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Back, but the daemon now refuses the join.
    w.record.refuse_join.store(true, Ordering::SeqCst);
    w.record.down.store(false, Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let status = w.status().await;
        if status["rejoin_refused"] != json!([]) {
            assert_eq!(status["joined_channels"], json!([]));
            assert_eq!(
                status["rejoin_refused"],
                json!([{"channel": "general", "error": "Overloaded"}])
            );
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no refused re-join");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    w.record.refuse_join.store(false, Ordering::SeqCst);
    let (text, error) = w.tool("join", json!({"channel": "general"})).await;
    assert!(!error, "{text}");
    let status = w.status().await;
    assert_eq!(status["rejoin_refused"], json!([]), "cleared by the join");
    assert_eq!(status["joined_channels"], json!(["general"]));
}
