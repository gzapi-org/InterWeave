// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Claude Code Channel bridge against two real daemons (plan §19
//! step 4). The shipped `claude-channel` binary runs as Claude Code runs
//! it: a child process speaking JSON-RPC on its stdio, started with
//! `--profile` and `--endpoint` in daemon A's XDG tree, so the path is:
//! - host lines in, through the bridge's IPC client and A's daemon;
//! - then libp2p to B's daemon and an IPC session leased on B's `human`;
//! - and back.
//!
//! Each notification's `meta` is held to the contract's key list and the
//! host's grammar (CHANNEL-EVENT.md; P1).
//!
//! What this does not prove:
//! - the host itself (step 5: the bridge loaded as a plugin in the
//!   installed Claude Code);
//! - a network other than one host's private address;
//! - B's far end as the human client's facade: B is a plain data session,
//!   which is what the facade sits on. The facade adds the envelope, and
//!   the bridge forwards an envelope as text, unparsed.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use interweave_ipc_client::IpcSession;
use interweave_local_client_api::{
    DataCapability, DataSessionBinding as _, DataSessionPort as _, SessionEvent, SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointId, MAX_PAYLOAD_BYTES, MessageId,
    Payload, TransportIdentity,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

mod common;
use common::{Daemon, Home, PATIENCE, example, free_port, human, workspace_binary};

/// CHANNEL-EVENT.md §Metadata's table, in its order.
const META_KEYS: [&str; 10] = [
    "delivery_mode",
    "source_peer",
    "source_endpoint",
    "destination_endpoint",
    "message_id",
    "received_at",
    "channel",
    "reply_token",
    "payload_encoding",
    "content_type",
];

fn claude() -> EndpointId {
    EndpointId::parse("claude").expect("endpoint")
}

fn general() -> ChannelId {
    ChannelId::parse("general").expect("channel")
}

/// The shipped bridge, started in `home`'s environment.
struct Bridge {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    /// Notifications read while waiting for an answer, each with the line
    /// it came on, whose text holds `meta`'s order.
    notifications: VecDeque<(Value, String)>,
    next_id: u64,
}

impl Bridge {
    fn start(home: &Home) -> Self {
        let env = |p: &Path| p.as_os_str().to_owned();
        let mut child = tokio::process::Command::new(workspace_binary(
            "claude-channel",
            "interweave-claude-channel",
        ))
        .args(["--profile", home.paths.profile(), "--endpoint", "claude"])
        .env_clear()
        .env("XDG_CONFIG_HOME", env(&home.roots.config_home))
        .env("XDG_DATA_HOME", env(&home.roots.data_home))
        .env("XDG_STATE_HOME", env(&home.roots.state_home))
        .env("XDG_CACHE_HOME", env(&home.roots.cache_home))
        .env(
            "XDG_RUNTIME_DIR",
            env(home.roots.runtime_dir.as_deref().expect("a runtime dir")),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("the bridge starts");
        let stdin = child.stdin.take().expect("stdin");
        let lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
        Self {
            child,
            stdin,
            lines,
            notifications: VecDeque::new(),
            next_id: 0,
        }
    }

    async fn line(&mut self) -> (Value, String) {
        let line = tokio::time::timeout(PATIENCE, self.lines.next_line())
            .await
            .expect("the bridge writes in time")
            .expect("readable")
            .expect("the bridge is running");
        let value = serde_json::from_str(&line).expect("one JSON value per line");
        (value, line)
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("written");
        loop {
            let (value, line) = self.line().await;
            if value["id"] == json!(id) {
                return value;
            }
            if value["method"] == json!("notifications/claude/channel") {
                self.notifications
                    .push_back((value["params"].clone(), line));
            }
        }
    }

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
        let (text, _) = self.tool("status", json!({})).await;
        serde_json::from_str(&text).expect("status is JSON")
    }

    /// Until the bridge holds its lease on a live daemon.
    async fn wait_leased(&mut self, daemons: &[&Daemon]) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while self.status().await["endpoint_lease_state"] != json!("held") {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the bridge never leased claude\n{}",
                logs(daemons)
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The next channel notification, held to the contract (P1).
    async fn notification(&mut self) -> Value {
        let (params, line) = match self.notifications.pop_front() {
            Some(held) => held,
            None => loop {
                let (value, line) = self.line().await;
                if value["method"] == json!("notifications/claude/channel") {
                    break (value["params"].clone(), line);
                }
            },
        };
        meta_is_the_contracts(&params["meta"], &line);
        params
    }
}

/// Every key one of the table's, in the table's order AS WRITTEN on
/// `line` -- a parsed `Value`'s map sorts its keys, so the order is read
/// from the text; each a match for the host's grammar; none `source`;
/// every value a string.
fn meta_is_the_contracts(meta: &Value, line: &str) {
    let object = meta.as_object().expect("meta is an object");
    let written = &line[line.find("\"meta\"").expect("a meta key")..];
    let mut order: Vec<(usize, &str)> = object
        .keys()
        .map(|key| {
            let at = written
                .find(&format!("\"{key}\":"))
                .unwrap_or_else(|| panic!("{key} on the line"));
            (at, key.as_str())
        })
        .collect();
    order.sort_unstable();
    let mut last = None;
    for (_, key) in order {
        let at = META_KEYS
            .iter()
            .position(|k| *k == key)
            .unwrap_or_else(|| panic!("{key} is not a CHANNEL-EVENT.md key"));
        assert!(
            last.is_none_or(|l| l < at),
            "{key} out of table order: {line}"
        );
        last = Some(at);
    }
    // The table's "direct only" and "only for broadcast" rows, both ways.
    let has = |key: &str| object.contains_key(key);
    match meta["delivery_mode"].as_str() {
        Some("direct") => {
            assert!(
                has("source_endpoint") && has("destination_endpoint"),
                "{meta}"
            );
            assert!(!has("channel"), "no channel on a direct message: {meta}");
        }
        Some("broadcast") => {
            assert!(has("channel"), "{meta}");
            assert!(
                !has("source_endpoint") && !has("destination_endpoint"),
                "no endpoint on a broadcast: {meta}"
            );
        }
        other => panic!("delivery_mode {other:?}"),
    }
    for (key, value) in object {
        assert!(value.is_string(), "{key} is a string: {meta}");
        let mut chars = key.chars();
        assert!(
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        );
        assert!(chars.all(|c| c.is_ascii_alphanumeric() || c == '_'));
        assert_ne!(key, "source");
    }
}

fn logs(daemons: &[&Daemon]) -> String {
    daemons
        .iter()
        .enumerate()
        .map(|(i, d)| format!("daemon {i}:\n{}", d.log()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Two daemons from the shipped desktop example (endpoints `human` and
/// `claude`), each with a static route to the other, both serving. Not
/// yet connected: the first exchange waits on that (`send_until_accepted`,
/// `broadcast_until`).
async fn two_daemons() -> (
    Home,
    Daemon,
    TransportIdentity,
    Home,
    Daemon,
    TransportIdentity,
) {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let (a, b) = (Home::new("human-desktop"), Home::new("human-desktop"));
    let (a_peer, b_peer) = (a.write_key(), b.write_key());
    let (a_port, b_port) = (free_port(ip), free_port(ip));
    let at = |port: u16| format!("/ip4/{ip}/tcp/{port}");
    let route = |port: u16, peer: &TransportIdentity| format!("{}/p2p/{}", at(port), peer.as_str());
    a.write_config(&example(
        "human-desktop.yaml",
        &b_peer,
        &at(a_port),
        Some(&route(b_port, &b_peer)),
    ));
    b.write_config(&example(
        "human-desktop.yaml",
        &a_peer,
        &at(b_port),
        Some(&route(a_port, &a_peer)),
    ));
    let mut a_daemon = a.start(&[]);
    a_daemon.serving(&a).await;
    let mut b_daemon = b.start(&[]);
    b_daemon.serving(&b).await;
    (a, a_daemon, a_peer, b, b_daemon, b_peer)
}

/// A human-client session leased on `home`'s `endpoint`.
async fn session(home: &Home, endpoint: EndpointId) -> IpcSession {
    home.binding()
        .open(
            SessionRequest::new(
                "human-client",
                Some(endpoint),
                [DataCapability::Events, DataCapability::Commands],
            )
            .expect("request"),
        )
        .await
        .expect("a session")
}

/// A session leased on `home`'s `claude`, under the client kind that
/// endpoint admits.
async fn claude_session(home: &Home) -> IpcSession {
    home.binding()
        .open(
            SessionRequest::new(
                "claude-channel",
                Some(claude()),
                [DataCapability::Events, DataCapability::Commands],
            )
            .expect("request"),
        )
        .await
        .expect("a session on claude")
}

/// Publish on `general` from `from` until the bridge is notified of it --
/// the mesh forms after the join, so the first publishes may reach no one
/// -- and return that notification.
async fn broadcast_until(
    from: &IpcSession,
    bridge: &mut Bridge,
    content: &str,
    daemons: &[&Daemon],
) -> Value {
    let deadline = tokio::time::Instant::now() + PATIENCE * 2;
    loop {
        from.broadcast(
            general(),
            BroadcastMessageV1 {
                message_id: MessageId::from_bytes(rand_bytes()),
                sent_at_ms: 0,
                payload: text(content),
            },
        )
        .await
        .expect("accepted for local publish");
        let _ = bridge.status().await;
        if let Some((n, line)) = bridge.notifications.pop_front() {
            meta_is_the_contracts(&n["meta"], &line);
            return n;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no broadcast reached the bridge\n{}",
            logs(daemons)
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn text(content: &str) -> Payload {
    Payload::new(None, content.as_bytes().to_vec(), MAX_PAYLOAD_BYTES).expect("payload")
}

/// Send from `from` to `peer`/`endpoint` until the far transport accepts:
/// the first exchange waits on the pair's connection, which the retry
/// scheduler makes within its base (plan §19; the restart and start-up
/// cases rust-ui-dev measured).
async fn send_until_accepted(
    from: &IpcSession,
    peer: &TransportIdentity,
    endpoint: EndpointId,
    content: &str,
    daemons: &[&Daemon],
) -> EndpointId {
    let deadline = tokio::time::Instant::now() + PATIENCE * 3;
    loop {
        match from
            .send_direct(
                DirectDestination {
                    peer: peer.clone(),
                    endpoint: Some(endpoint.clone()),
                },
                MessageId::from_bytes(rand_bytes()),
                text(content),
            )
            .await
        {
            Ok(accepted) => return accepted,
            Err(e) => assert!(
                tokio::time::Instant::now() < deadline,
                "never accepted ({e:?})\n{}",
                logs(daemons)
            ),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn rand_bytes() -> [u8; 16] {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut bytes = [0x5a; 16];
    bytes[..8].copy_from_slice(&n.to_be_bytes());
    bytes
}

/// The direct message's notification from B's `human`.
async fn direct_from(events_of: &IpcSession, content: &[u8]) -> bool {
    events_of
        .events(usize::MAX)
        .await
        .expect("events")
        .iter()
        .any(|e| matches!(e, SessionEvent::Direct(d) if d.payload.bytes() == content))
}

/// Incoming direct → a channel event with both endpoints, `meta` held to
/// the contract; `reply` takes the exact route back to B's `human`
/// (§19 required tests).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_direct_message_is_notified_and_replied_to_across_two_daemons() {
    let (a, a_daemon, a_peer, b, b_daemon, b_peer) = two_daemons().await;
    let daemons = [&a_daemon, &b_daemon];
    let mut bridge = Bridge::start(&a);
    bridge.wait_leased(&daemons).await;
    let b_human = session(&b, human()).await;

    let accepted = send_until_accepted(&b_human, &a_peer, claude(), "hello claude", &daemons).await;
    assert_eq!(accepted, claude());
    let n = bridge.notification().await;
    assert_eq!(n["content"], json!("hello claude"));
    let meta = &n["meta"];
    assert_eq!(meta["delivery_mode"], json!("direct"));
    assert_eq!(meta["source_peer"], json!(b_peer.as_str()));
    assert_eq!(meta["source_endpoint"], json!("human"));
    assert_eq!(meta["destination_endpoint"], json!("claude"));
    assert_eq!(meta["payload_encoding"], json!("utf8"));
    let token = meta["reply_token"].as_str().expect("a token").to_owned();

    let (answer, error) = bridge
        .tool(
            "reply",
            json!({"reply_token": token, "content": "hello human"}),
        )
        .await;
    assert!(!error, "{answer}\n{}", logs(&daemons));
    assert_eq!(answer, "remote transport accepted at endpoint human");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !direct_from(&b_human, b"hello human").await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the reply never arrived"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // `send` names an endpoint other than B's default (`human`), and the
    // message lands there, not on the default.
    let b_claude = claude_session(&b).await;
    let (answer, error) = bridge
        .tool(
            "send",
            json!({"peer": b_peer.as_str(), "endpoint": "claude", "content": "named"}),
        )
        .await;
    assert!(!error, "{answer}");
    assert_eq!(answer, "remote transport accepted at endpoint claude");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !direct_from(&b_claude, b"named").await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the named send never arrived at B's claude"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !direct_from(&b_human, b"named").await,
        "and not at B's default"
    );
}

/// One `PeerId`, two endpoints: B's message to A's `claude` reaches the
/// bridge alone, and its message to A's `human` reaches A's human
/// session alone (§19 required tests).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_human_and_claude_endpoints_of_one_peer_are_routed_apart() {
    let (a, a_daemon, a_peer, b, b_daemon, _) = two_daemons().await;
    let daemons = [&a_daemon, &b_daemon];
    let mut bridge = Bridge::start(&a);
    bridge.wait_leased(&daemons).await;
    let a_human = session(&a, human()).await;
    let b_human = session(&b, human()).await;

    send_until_accepted(&b_human, &a_peer, claude(), "to claude", &daemons).await;
    send_until_accepted(&b_human, &a_peer, human(), "to human", &daemons).await;
    let n = bridge.notification().await;
    assert_eq!(n["content"], json!("to claude"));
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    while seen.is_empty() {
        seen.extend(
            a_human
                .events(usize::MAX)
                .await
                .expect("events")
                .into_iter()
                .filter_map(|e| match e {
                    SessionEvent::Direct(d) => Some(d.payload.bytes().to_vec()),
                    _ => None,
                }),
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "the human message never came"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(seen, [b"to human".to_vec()], "A's human saw only its own");
    // And the bridge nothing more: a status round trip flushes any
    // notification written before it.
    let _ = bridge.status().await;
    assert!(
        bridge.notifications.is_empty(),
        "the bridge saw only claude's: {:?}",
        bridge.notifications
    );
}

/// Broadcast: the bridge joins, receives B's publish, replies on the
/// channel; once it leaves, the reply fails `ChannelNotJoined`
/// (§19 required tests).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcast_join_publish_reply_and_channel_not_joined() {
    let (a, a_daemon, a_peer, b, b_daemon, _) = two_daemons().await;
    let daemons = [&a_daemon, &b_daemon];
    let mut bridge = Bridge::start(&a);
    bridge.wait_leased(&daemons).await;
    let b_human = session(&b, human()).await;
    // A direct exchange first: the pair is connected, so the mesh can form.
    send_until_accepted(&b_human, &a_peer, claude(), "warm", &daemons).await;
    let _ = bridge.notification().await;

    let (answer, error) = bridge.tool("join", json!({"channel": "general"})).await;
    assert!(!error, "{answer}");
    b_human.join(general()).await.expect("B joins");
    let deadline = tokio::time::Instant::now() + PATIENCE * 2;
    let n = loop {
        b_human
            .broadcast(
                general(),
                BroadcastMessageV1 {
                    message_id: MessageId::from_bytes(rand_bytes()),
                    sent_at_ms: 0,
                    payload: text("hi all"),
                },
            )
            .await
            .expect("accepted for local publish");
        let _ = bridge.status().await;
        if let Some((n, line)) = bridge.notifications.pop_front() {
            meta_is_the_contracts(&n["meta"], &line);
            break n;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no broadcast reached the bridge\n{}",
            logs(&daemons)
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(n["meta"]["delivery_mode"], json!("broadcast"));
    assert_eq!(n["meta"]["channel"], json!("general"));
    assert!(n["meta"].get("source_endpoint").is_none());
    let token = n["meta"]["reply_token"].as_str().expect("token").to_owned();
    let (answer, error) = bridge
        .tool("reply", json!({"reply_token": token, "content": "hi back"}))
        .await;
    assert!(!error, "{answer}");
    assert_eq!(answer, "accepted for local publish");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let arrived = b_human
            .events(usize::MAX)
            .await
            .expect("events")
            .iter()
            .any(|e| {
                matches!(e, SessionEvent::Broadcast(m)
                if m.channel == general() && m.payload.bytes() == b"hi back")
            });
        if arrived {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the bridge's broadcast never reached B"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let (answer, error) = bridge.tool("leave", json!({"channel": "general"})).await;
    assert!(!error, "{answer}");
    let (answer, error) = bridge
        .tool("reply", json!({"reply_token": token, "content": "after"}))
        .await;
    assert!(error && answer.starts_with("ChannelNotJoined"), "{answer}");
}

/// The daemon away and back: A's daemon stopped, the bridge keeps
/// answering `status` and refuses network tools clearly; restarted, the
/// bridge claims afresh, and a reply token from before fails rather than
/// switching routes (§19 required tests, LIFECYCLE.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_away_and_back_with_a_stale_token() {
    let (a, mut a_daemon, a_peer, b, b_daemon, b_peer) = two_daemons().await;
    let mut bridge = Bridge::start(&a);
    bridge.wait_leased(&[&a_daemon, &b_daemon]).await;
    let b_human = session(&b, human()).await;
    send_until_accepted(
        &b_human,
        &a_peer,
        claude(),
        "before",
        &[&a_daemon, &b_daemon],
    )
    .await;
    let token = bridge.notification().await["meta"]["reply_token"]
        .as_str()
        .expect("token")
        .to_owned();
    let before = bridge.status().await["endpoint_lease_epoch"].clone();
    let (answer, error) = bridge.tool("join", json!({"channel": "general"})).await;
    assert!(!error, "{answer}");
    b_human.join(general()).await.expect("B joins");
    // The control: the join carries a broadcast before the restart.
    broadcast_until(
        &b_human,
        &mut bridge,
        "before restart",
        &[&a_daemon, &b_daemon],
    )
    .await;

    a_daemon.terminate().await;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while bridge.status().await["endpoint_lease_state"] != json!("daemon unavailable") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "never saw the daemon go"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (answer, error) = bridge
        .tool("send", json!({"peer": b_peer.as_str(), "content": "x"}))
        .await;
    assert!(
        error && answer.starts_with("BackendUnavailable"),
        "{answer}"
    );

    let mut a_daemon = a.start(&[]);
    a_daemon.serving(&a).await;
    bridge.wait_leased(&[&a_daemon, &b_daemon]).await;
    assert_ne!(
        bridge.status().await["endpoint_lease_epoch"],
        before,
        "a fresh claim"
    );
    let (answer, error) = bridge
        .tool("reply", json!({"reply_token": token, "content": "stale"}))
        .await;
    assert!(error && answer.starts_with("InvalidArgument"), "{answer}");
    // Fresh joins: the restarted daemon knew nothing of the bridge's join,
    // and B's broadcast reaches the bridge again.
    assert_eq!(bridge.status().await["joined_channels"], json!(["general"]));
    let n = broadcast_until(
        &b_human,
        &mut bridge,
        "after restart",
        &[&a_daemon, &b_daemon],
    )
    .await;
    assert_eq!(n["content"], json!("after restart"));
    assert!(
        bridge.child.try_wait().expect("waitable").is_none(),
        "the bridge never exited"
    );
}
