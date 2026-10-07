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

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
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

/// The bridge's clock in these tests: fixed.
const NOW_MS: u64 = 1_791_227_222_497;

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
    /// The next join ends the session, as a daemon stopping between the
    /// open and the re-join does: the session is down from then on, and
    /// the join answers `ShuttingDown`, the code the IPC binding gives a
    /// graceful stop.
    die_on_join: AtomicBool,
    /// Leaves are refused by a live daemon.
    refuse_leave: AtomicBool,
    /// The next leave ends the session, answering `ShuttingDown`.
    die_on_leave: AtomicBool,
    /// When each open was asked, on the test's clock.
    opens: Mutex<Vec<tokio::time::Instant>>,
    /// Every open succeeds and the session it gives has already ended:
    /// a daemon that accepts and closes at once.
    accept_and_close: AtomicBool,
    /// Direct sends never answer, as a daemon whose answer waits behind
    /// undrained events: each is recorded `send_direct cancelled` when the
    /// bridge drops it, which is what sends the IPC binding's cancel.
    hold_send: AtomicBool,
    /// Joins never answer, recorded `join cancelled` the same way.
    hold_join: AtomicBool,
    /// `ready` does not resolve, so events gather at the daemon until it
    /// is lifted and the bridge sees them all at once.
    hold_ready: AtomicBool,
    /// How many times the bridge has asked `ready`.
    readies: AtomicU64,
}

/// Records `<call> cancelled` when a held call is dropped unanswered.
struct Cancelled(Arc<Record>, &'static str);

impl Drop for Cancelled {
    fn drop(&mut self) {
        self.0.call(&format!("{} cancelled", self.1));
    }
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
            .opens
            .lock()
            .expect("lock")
            .push(tokio::time::Instant::now());
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
        if self.record.hold_join.load(Ordering::SeqCst) {
            let _cancelled = Cancelled(Arc::clone(&self.record), "join");
            std::future::pending::<()>().await;
        }
        if self.record.refuse_join.load(Ordering::SeqCst) {
            return Err(TransportError::Overloaded);
        }
        if self.record.die_on_join.swap(false, Ordering::SeqCst) {
            self.record.down.store(true, Ordering::SeqCst);
            return Err(TransportError::ShuttingDown);
        }
        self.inner.join(channel).await
    }
    async fn leave(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.record.call("leave");
        self.record.down()?;
        if self.record.refuse_leave.load(Ordering::SeqCst) {
            return Err(TransportError::Overloaded);
        }
        if self.record.die_on_leave.swap(false, Ordering::SeqCst) {
            self.record.down.store(true, Ordering::SeqCst);
            return Err(TransportError::ShuttingDown);
        }
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
        if self.record.hold_send.load(Ordering::SeqCst) {
            let _cancelled = Cancelled(Arc::clone(&self.record), "send_direct");
            std::future::pending::<()>().await;
        }
        self.inner
            .send_direct(destination, message_id, payload)
            .await
    }
    async fn events(&self, max: usize) -> Result<Vec<SessionEvent>, TransportError> {
        self.record.down()?;
        if self.record.accept_and_close.load(Ordering::SeqCst) {
            return Err(TransportError::BackendUnavailable);
        }
        self.inner.events(max).await
    }
    async fn ready(&self) -> Result<(), TransportError> {
        self.record.readies.fetch_add(1, Ordering::SeqCst);
        // Polled: a daemon taken away is seen within a tick, as a real
        // connection's end would be.
        loop {
            self.record.down()?;
            if self.record.hold_ready.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            if self.record.accept_and_close.load(Ordering::SeqCst) {
                return Err(TransportError::BackendUnavailable);
            }
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
    /// Pull mode: a push notification on the host's pipe fails the test.
    pull: bool,
}

impl World {
    async fn start() -> Self {
        Self::start_in(interweave_claude_channel_core::Delivery::Push).await
    }

    async fn start_pull() -> Self {
        Self::start_in(interweave_claude_channel_core::Delivery::Pull).await
    }

    async fn start_in(delivery: interweave_claude_channel_core::Delivery) -> Self {
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
            now_ms: Box::new(|| NOW_MS),
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
            delivery,
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
            pull: delivery == interweave_claude_channel_core::Delivery::Pull,
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
            assert!(!self.pull, "pull mode pushes nothing: {answer}");
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

    /// `receive(max)`'s structured answer.
    async fn receive(&mut self, max: Option<u64>) -> Value {
        let arguments = max.map_or_else(|| json!({}), |max| json!({"max": max}));
        let (text, error) = self.tool("receive", arguments).await;
        assert!(!error, "{text}");
        serde_json::from_str(&text).expect("receive is JSON")
    }

    /// Wait until the pull queue holds `depth`.
    async fn wait_depth(&mut self, depth: u64) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while self.status().await["pull_queue"]["depth"] != json!(depth) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "never reached {depth}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
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
    // From B's `claude`, which is not B's default (`human`): a reply that
    // dropped its route would land on the default instead.
    let b_claude =
        w.b.open(
            SessionRequest::new(
                "test-peer",
                Some(endpoint("claude")),
                [DataCapability::Events, DataCapability::Commands],
            )
            .expect("request"),
        )
        .await
        .expect("opens");
    b_claude
        .send_direct(
            DirectDestination {
                peer: w.a.peer().clone(),
                endpoint: Some(endpoint("claude")),
            },
            MessageId::from_bytes([4; 16]),
            Payload::new(None, b"hello claude".to_vec(), MAX_PAYLOAD_BYTES).expect("payload"),
        )
        .await
        .expect("accepted");
    let n = w.notification().await;
    assert_eq!(n["content"], json!("hello claude"));
    let meta = &n["meta"];
    assert_eq!(meta["delivery_mode"], json!("direct"));
    assert_eq!(meta["source_peer"], json!(w.b.peer().as_str()));
    assert_eq!(meta["source_endpoint"], json!("claude"));
    assert_eq!(meta["destination_endpoint"], json!("claude"));
    assert!(meta.get("source").is_none(), "no source key: {meta}");
    let token = meta["reply_token"].as_str().expect("a token").to_owned();

    let (text, error) = w
        .tool(
            "reply",
            json!({"reply_token": token, "content": "hello back"}),
        )
        .await;
    assert!(!error, "{text}");
    assert_eq!(text, "remote transport accepted at endpoint claude");
    let events = b_claude.events(usize::MAX).await.expect("events");
    let reply = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::Direct(d) => Some(d),
            _ => None,
        })
        .expect("the reply arrived");
    assert_eq!(reply.source_endpoint, endpoint("claude"));
    assert_eq!(reply.payload.bytes(), b"hello back");

    let (text, error) = w
        .tool(
            "reply",
            json!({"reply_token": "not-a-token", "content": "x"}),
        )
        .await;
    assert!(error, "an unknown token fails: {text}");
    assert!(text.starts_with("InvalidArgument"), "{text}");
}

/// `send` names the endpoint, or the remote default when it does not:
/// the explicit target is B's `claude`, not its default `human`, and the
/// message arrives at the session leased there.
#[tokio::test]
async fn send_is_endpoint_aware() {
    let mut w = World::start().await;
    let b_claude =
        w.b.open(
            SessionRequest::new(
                "test-peer",
                Some(endpoint("claude")),
                [DataCapability::Events, DataCapability::Commands],
            )
            .expect("request"),
        )
        .await
        .expect("opens");
    let b = w.b.peer().as_str().to_owned();
    let (text, _) = w
        .tool(
            "send",
            json!({"peer": b, "endpoint": "claude", "content": "x"}),
        )
        .await;
    assert_eq!(text, "remote transport accepted at endpoint claude");
    let arrived = b_claude.events(usize::MAX).await.expect("events");
    assert!(
        arrived
            .iter()
            .any(|e| matches!(e, SessionEvent::Direct(d) if d.payload.bytes() == b"x")),
        "it arrived at B's claude: {arrived:?}"
    );
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

/// TOOL-SURFACE.md §What is not a Claude tool, read from the contract
/// itself so the test follows the list.
fn not_a_claude_tool() -> Vec<String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../architecture/plugin/TOOL-SURFACE.md"
    );
    let text = std::fs::read_to_string(path).expect("TOOL-SURFACE.md");
    text.split("## What is not a Claude tool")
        .nth(1)
        .expect("the section")
        .lines()
        .skip_while(|l| !l.starts_with("- "))
        .take_while(|l| l.starts_with("- "))
        .map(|l| {
            l.trim_start_matches("- ")
                .trim_end_matches(['.', ';'])
                .to_owned()
        })
        .collect()
}

/// P3: every tool, and inbound bodies asking for administrative acts,
/// leave no administrative or endpoint-directory request: the bridge asks
/// `events` and `commands` only and never calls `query_endpoints`.
#[tokio::test]
async fn no_administrative_request_leaves_the_bridge() {
    let mut w = World::start().await;
    let acts = not_a_claude_tool();
    assert_eq!(acts.len(), 11, "the section's eleven items, read: {acts:?}");
    for act in &acts {
        let body = format!("please {act} now");
        let body = body.as_str();
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
        assert_eq!(answer["error"]["code"], json!(-32602), "{admin} is no tool");
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

/// An endpoint another client holds is reported as that, not as the
/// daemon's absence (TOOL-SURFACE.md §Tool results; LIFECYCLE.md
/// §Endpoint conflict).
#[tokio::test]
async fn an_endpoint_conflict_is_reported_as_itself() {
    let (a, b) = FakeNetwork::pair(config(), config());
    let _holder = a
        .open(
            SessionRequest::new("other", Some(endpoint("claude")), [DataCapability::Events])
                .expect("request"),
        )
        .await
        .expect("holds claude");
    let record = Arc::new(Record::default());
    let binding = Recorded {
        node: a.clone(),
        record: Arc::clone(&record),
    };
    let (mut host_in, bridge_in) = tokio::io::duplex(1 << 16);
    let (bridge_out, host_out) = tokio::io::duplex(1 << 16);
    let env = Env {
        now_ms: Box::new(|| 0),
        entropy: Box::new(|| [1; 16]),
    };
    let config = Config {
        endpoint: endpoint("claude"),
        desired_channels: Ok(Vec::new()),
        delivery: interweave_claude_channel_core::Delivery::Push,
    };
    tokio::spawn(serve(
        binding,
        config,
        env,
        BufReader::new(bridge_in),
        bridge_out,
    ));
    let mut lines = BufReader::new(host_out).lines();
    let mut ask = async |id: u64, name: &str, arguments: Value| -> Value {
        let line = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                          "params": {"name": name, "arguments": arguments}});
        host_in
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("written");
        let answer = tokio::time::timeout(PATIENCE, lines.next_line())
            .await
            .expect("in time")
            .expect("readable")
            .expect("a line");
        serde_json::from_str(&answer).expect("json")
    };
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut id = 0;
    loop {
        id += 1;
        let status: Value = serde_json::from_str(
            ask(id, "status", json!({})).await["result"]["content"][0]["text"]
                .as_str()
                .expect("text"),
        )
        .expect("status json");
        if status["endpoint_lease_state"] == json!("endpoint refused") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never refused: {status}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let b_peer = b.peer().as_str().to_owned();
    let answer = ask(id + 1, "send", json!({"peer": b_peer, "content": "x"})).await;
    let text = answer["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.starts_with("EndpointInUse"), "{text}");
}

/// A daemon that stops during the re-join refused nothing: its session
/// ended with `ShuttingDown`, and the join is kept and re-taken by the
/// next open (LIFECYCLE.md step 6: only what "the daemon refuses" is a
/// row). Its control is `a_refused_rejoin_is_a_status_row_until_the_next_join`,
/// where a live daemon refuses.
#[tokio::test]
async fn a_daemon_stopping_during_the_rejoin_keeps_the_join() {
    let mut w = World::start().await;
    w.tool("join", json!({"channel": "general"})).await;
    w.record.down.store(true, Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while w.status().await["endpoint_lease_state"] != json!("daemon unavailable") {
        assert!(tokio::time::Instant::now() < deadline, "never saw it go");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    w.record.die_on_join.store(true, Ordering::SeqCst);
    w.record.down.store(false, Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while w.record.die_on_join.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the re-join never came"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let status = w.status().await;
    assert_eq!(status["rejoin_refused"], json!([]), "no refusal recorded");
    w.record.down.store(false, Ordering::SeqCst);
    w.wait_connected().await;
    let status = w.status().await;
    assert_eq!(status["rejoin_refused"], json!([]), "still none");
    assert_eq!(
        status["joined_channels"],
        json!(["general"]),
        "the join kept"
    );
}

/// A live daemon that refuses a leave still holds the join, so the bridge
/// keeps it; the leave that succeeds is the control.
#[tokio::test]
async fn a_refused_leave_keeps_the_join() {
    let mut w = World::start().await;
    w.tool("join", json!({"channel": "general"})).await;
    w.record.refuse_leave.store(true, Ordering::SeqCst);
    let (text, error) = w.tool("leave", json!({"channel": "general"})).await;
    assert!(error && text.starts_with("Overloaded"), "{text}");
    assert_eq!(w.status().await["joined_channels"], json!(["general"]));
    w.record.refuse_leave.store(false, Ordering::SeqCst);
    let (text, error) = w.tool("leave", json!({"channel": "general"})).await;
    assert!(!error, "{text}");
    assert_eq!(w.status().await["joined_channels"], json!([]));
}

/// A daemon that accepts and closes at once is retried on a growing
/// backoff, not at the first delay forever (LIFECYCLE.md §Reconnect).
#[tokio::test(start_paused = true)]
async fn a_daemon_that_accepts_and_closes_is_retried_on_a_growing_backoff() {
    let w = World::start().await;
    w.record.accept_and_close.store(true, Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        if w.record.opens.lock().expect("lock").len() >= 6 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "too few opens");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let opens = w.record.opens.lock().expect("lock").clone();
    let gaps: Vec<Duration> = opens.windows(2).map(|p| p[1] - p[0]).collect();
    let last = gaps.len() - 1;
    assert!(gaps[last] >= gaps[last - 2] * 3, "the gaps grow: {gaps:?}");
}

/// A leave whose session ends before the daemon answers is left: the join
/// went with the session, and the reconnect does not re-take it.
#[tokio::test]
async fn a_leave_whose_session_ends_is_left_and_not_retaken() {
    let mut w = World::start().await;
    w.tool("join", json!({"channel": "general"})).await;
    w.record.die_on_leave.store(true, Ordering::SeqCst);
    let (text, error) = w.tool("leave", json!({"channel": "general"})).await;
    assert!(!error, "{text}");
    assert_eq!(text, "left general");
    let joins_before = w.record.calls().iter().filter(|c| *c == "join").count();
    w.record.down.store(false, Ordering::SeqCst);
    w.wait_connected().await;
    let status = w.status().await;
    assert_eq!(status["joined_channels"], json!([]));
    assert_eq!(
        w.record.calls().iter().filter(|c| *c == "join").count(),
        joins_before,
        "the reconnect re-took nothing"
    );
}

/// After a session that stayed up a whole ceiling, the backoff starts
/// again from the first delay: a long outage's first retry is not a
/// leftover 30 s.
#[tokio::test(start_paused = true)]
async fn the_backoff_starts_over_after_a_session_that_lasted() {
    let w = World::start().await;
    // Grow the backoff first: a daemon that accepts and closes.
    w.record.accept_and_close.store(true, Ordering::SeqCst);
    let grown = loop {
        let opens = w.record.opens.lock().expect("lock").clone();
        if opens.len() >= 6 {
            break opens[opens.len() - 1] - opens[opens.len() - 2];
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        grown >= Duration::from_secs(4),
        "the control: grown to {grown:?}"
    );
    // Then a session that lives past the ceiling, then ends.
    w.record.accept_and_close.store(false, Ordering::SeqCst);
    let held = w.record.opens.lock().expect("lock").len();
    while w.record.opens.lock().expect("lock").len() == held {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_secs(31)).await;
    w.record.accept_and_close.store(true, Ordering::SeqCst);
    let ended_at = tokio::time::Instant::now();
    let after = held + 1;
    while w.record.opens.lock().expect("lock").len() <= after {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let next = w.record.opens.lock().expect("lock")[after];
    assert!(
        next - ended_at < Duration::from_secs(1),
        "the first retry after a lasting session is the first delay, not {:?}",
        next - ended_at
    );
}

/// Each mode's surface: push advertises `claude/channel` and the seven
/// tools; pull advertises no channel extension and adds `receive`, so a
/// session never has two ways of taking a message (architect-cto's ruling,
/// relay seq 18691).
#[tokio::test]
async fn each_mode_advertises_its_own_way_of_taking_a_message() {
    for (pull, tools, channel) in [(false, 7, true), (true, 8, false)] {
        let mut w = if pull {
            World::start_pull().await
        } else {
            World::start().await
        };
        let init = w.request("initialize", json!({})).await;
        assert_eq!(
            init["result"]["capabilities"]["experimental"]
                .get("claude/channel")
                .is_some(),
            channel,
            "pull={pull}: {init}"
        );
        let list = w.request("tools/list", json!({})).await;
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| t["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(names.len(), tools, "pull={pull}: {names:?}");
        assert_eq!(names.contains(&"receive"), pull, "{names:?}");
        if !pull {
            // Push mode has no `receive`: an unknown tool, as MCP answers.
            let answer = w
                .request("tools/call", json!({"name": "receive", "arguments": {}}))
                .await;
            assert_eq!(answer["error"]["code"], json!(-32602), "{answer}");
        }
    }
}

/// Pull mode: an inbound direct is queued, never pushed, and `receive`
/// returns it with the content and meta the push would carry; its
/// `reply_token` answers on its route. The second `receive` is empty.
#[tokio::test]
async fn a_pulled_direct_carries_the_pushs_content_and_meta_and_is_replied_to() {
    let mut w = World::start_pull().await;
    w.peer_sends("hello pull").await;
    w.wait_depth(1).await;
    let received = w.receive(None).await;
    assert_eq!(received["remaining"], json!(0));
    assert_eq!(received["paused"], json!(false));
    let event = &received["events"][0];
    assert_eq!(event["kind"], json!("direct"));
    assert_eq!(event["content"], json!("hello pull"));
    assert_eq!(event["meta"]["delivery_mode"], json!("direct"));
    assert_eq!(event["meta"]["source_peer"], json!(w.b.peer().as_str()));
    assert_eq!(event["meta"]["destination_endpoint"], json!("claude"));
    let token = event["meta"]["reply_token"]
        .as_str()
        .expect("a token")
        .to_owned();
    let (text, error) = w
        .tool("reply", json!({"reply_token": token, "content": "back"}))
        .await;
    assert!(!error, "{text}");
    assert_eq!(w.receive(None).await["events"], json!([]), "taken once");
}

/// Full (the session's granted event queue, 16 here), the pull queue
/// pauses the session's draining and drops nothing it took (relay seq
/// 18784): a session-bound tool is refused at once with the bridge's own
/// error while paused, `status` and `receive` answer, `receive` lifts the
/// pause, and every message arrives in order. A `max` is clamped, never
/// refused, and `remaining` says what is left.
///
/// A take of one with two held at the daemon gives room for one: the
/// bridge takes no more than that, so the queue fills again with nothing
/// refused. `paused_since` is the env's clock at the fill, null after.
/// On two workers, so a bridge that spins while paused fails the test's
/// own deadlines rather than hanging it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pull_queue_pauses_when_full_and_drops_nothing() {
    let mut w = World::start_pull().await;
    assert_eq!(
        w.status().await["pull_queue"]["paused_since"],
        Value::Null,
        "not paused"
    );
    for i in 0..18_u64 {
        w.peer_sends(&format!("m{i:02}")).await;
        w.wait_depth((i + 1).min(16)).await;
    }
    let status = w.status().await;
    assert_eq!(status["pull_queue"]["paused"], json!(true), "{status}");
    assert_eq!(
        status["pull_queue"]["paused_since"],
        json!(NOW_MS),
        "{status}"
    );
    let (text, error) = w.tool("join", json!({"channel": "general"})).await;
    assert!(error, "refused while paused");
    assert_eq!(text, "the pull queue is full: call receive first");
    // Paused with two held at the daemon, the bridge does not ask the
    // session again: a loop that did would spin on events it cannot take.
    let readies = w.record.readies.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        w.record.readies.load(Ordering::SeqCst),
        readies,
        "no wake while paused"
    );
    let one = w.receive(Some(1)).await;
    assert_eq!(one["events"][0]["content"], json!("m00"), "nothing dropped");
    assert_eq!(one["paused"], json!(false), "a take lifts the pause");
    // Room for one, two held at the daemon: one taken, the queue full
    // again, one still held.
    w.wait_depth(16).await;
    assert_eq!(w.status().await["pull_queue"]["paused"], json!(true));
    let first = w.receive(Some(10)).await;
    assert_eq!(first["events"].as_array().map(Vec::len), Some(10));
    assert_eq!(first["events"][0]["content"], json!("m01"));
    assert_eq!(first["remaining"], json!(6));
    assert_eq!(first["paused"], json!(false));
    // The one the session held meanwhile is drained now.
    w.wait_depth(7).await;
    let rest = w.receive(Some(1_000)).await;
    let contents: Vec<&str> = rest["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|e| e["content"].as_str().expect("content"))
        .collect();
    assert_eq!(
        contents,
        ["m11", "m12", "m13", "m14", "m15", "m16", "m17"],
        "clamped, not refused, and in order"
    );
    assert_eq!(
        w.status().await["pull_queue"]["paused_since"],
        Value::Null,
        "cleared once a take leaves room"
    );
    assert_eq!(rest["remaining"], json!(0));
    let (text, error) = w.tool("join", json!({"channel": "general"})).await;
    assert!(!error, "the control, not paused: {text}");
}

/// Fill the pull queue to `depth`, one message at a time.
async fn fill(w: &mut World, depth: u64) {
    for i in 0..depth {
        w.peer_sends(&format!("f{i:02}")).await;
        w.wait_depth(i + 1).await;
    }
}

/// A call in flight when the queue fills is cancelled -- dropped, which
/// is what sends the IPC binding's cancel -- and answered with the pull
/// queue's own error, never left waiting behind the paused drain (relay
/// seq 18835). The control: the same send, not held, is accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_in_flight_when_the_queue_fills_is_cancelled() {
    let mut w = World::start_pull().await;
    fill(&mut w, 15).await;
    w.record.hold_send.store(true, Ordering::SeqCst);
    w.record.hold_ready.store(true, Ordering::SeqCst);
    w.next_id += 1;
    let id = w.next_id;
    let b = w.b.peer().as_str().to_owned();
    let line = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": "send", "arguments": {"peer": b, "content": "x"}}});
    w.host_in
        .write_all(format!("{line}\n").as_bytes())
        .await
        .expect("written");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !w.record.calls().iter().any(|c| c == "send_direct") {
        assert!(tokio::time::Instant::now() < deadline, "never sent");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Two gather at the daemon while the send is in flight; the drain
    // takes the one there is room for, which fills the queue.
    w.peer_sends("last").await;
    w.peer_sends("after").await;
    w.record.hold_ready.store(false, Ordering::SeqCst);
    let answer = w.line().await;
    assert_eq!(answer["id"], json!(id), "{answer}");
    assert_eq!(answer["result"]["isError"], json!(true), "{answer}");
    assert_eq!(
        answer["result"]["content"][0]["text"],
        json!("the pull queue is full: call receive first")
    );
    assert!(
        w.record
            .calls()
            .iter()
            .any(|c| c == "send_direct cancelled"),
        "the call was dropped: {:?}",
        w.record.calls()
    );
    assert_eq!(w.status().await["pull_queue"]["depth"], json!(16));
    w.record.hold_send.store(false, Ordering::SeqCst);
    assert_eq!(
        w.receive(None).await["events"][15]["content"],
        json!("last")
    );
    w.wait_depth(1).await;
    assert_eq!(
        w.receive(None).await["events"][0]["content"],
        json!("after"),
        "the one there was no room for stayed at the daemon"
    );
    let (text, error) = w.tool("send", json!({"peer": b, "content": "x"})).await;
    assert!(!error, "the control: {text}");
}

/// The queue survives a reconnect; a held direct message's token is stale
/// after it (the epoch moved), a held broadcast's maps to its channel and
/// survives the re-join (relay seq 18835). A join the bridge held is not
/// re-taken while the queue is full -- it would wait behind the pause --
/// and is a status row until the next join.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_queue_outlives_a_reconnect_and_only_a_direct_token_goes_stale() {
    let mut w = World::start_pull().await;
    let (text, error) = w.tool("join", json!({"channel": "general"})).await;
    assert!(!error, "{text}");
    w.peer.join(general()).await.expect("peer joins");
    w.peer_sends("direct").await;
    w.peer
        .broadcast(
            general(),
            BroadcastMessageV1 {
                message_id: MessageId::from_bytes([3; 16]),
                sent_at_ms: 0,
                payload: Payload::new(None, b"all".to_vec(), MAX_PAYLOAD_BYTES).expect("payload"),
            },
        )
        .await
        .expect("accepted");
    w.wait_depth(2).await;

    w.record.down.store(true, Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while w.status().await["endpoint_lease_state"] != json!("daemon unavailable") {
        assert!(tokio::time::Instant::now() < deadline, "never saw it go");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    w.record.down.store(false, Ordering::SeqCst);
    w.wait_connected().await;
    assert_eq!(w.status().await["joined_channels"], json!(["general"]));

    let received = w.receive(None).await;
    let token_of = |kind: &str| {
        received["events"]
            .as_array()
            .expect("events")
            .iter()
            .find(|e| e["kind"] == json!(kind))
            .and_then(|e| e["meta"]["reply_token"].as_str())
            .unwrap_or_else(|| panic!("a {kind} token: {received}"))
            .to_owned()
    };
    let (direct, broadcast) = (token_of("direct"), token_of("broadcast"));
    let (text, error) = w
        .tool("reply", json!({"reply_token": direct, "content": "stale"}))
        .await;
    assert!(error && text.starts_with("InvalidArgument"), "{text}");
    let (text, error) = w
        .tool("reply", json!({"reply_token": broadcast, "content": "ok"}))
        .await;
    assert!(!error, "the broadcast token survives: {text}");

    // A re-join in flight when the queue fills is cancelled the same
    // way, and says why until the next join.
    fill(&mut w, 15).await;
    w.record.down.store(true, Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while w.status().await["endpoint_lease_state"] != json!("daemon unavailable") {
        assert!(tokio::time::Instant::now() < deadline, "never saw it go");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let joins = || w.record.calls().iter().filter(|c| *c == "join").count();
    let joins_before = joins();
    w.record.hold_join.store(true, Ordering::SeqCst);
    w.record.down.store(false, Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while joins() == joins_before {
        assert!(tokio::time::Instant::now() < deadline, "never re-joined");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    w.peer_sends("last").await;
    w.wait_connected().await;
    let status = w.status().await;
    assert_eq!(
        status["rejoin_refused"],
        json!([{"channel": "general", "error": "PullQueueFull"}]),
        "{status}"
    );
    assert_eq!(status["joined_channels"], json!([]));
    assert!(
        w.record.calls().iter().any(|c| c == "join cancelled"),
        "the re-join was dropped"
    );
    assert_eq!(status["pull_queue"]["depth"], json!(16), "kept");
    w.record.hold_join.store(false, Ordering::SeqCst);
    w.receive(None).await;
    let (text, error) = w.tool("join", json!({"channel": "general"})).await;
    assert!(!error, "{text}");
    assert_eq!(w.status().await["rejoin_refused"], json!([]));
}
