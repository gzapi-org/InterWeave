// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! IPC v2 on the wire: the server over a runtime composed from a profile,
//! driven by raw frames on real Unix sockets. What a client sees, held to
//! the frozen schemas -- not what the server's types promise.
//!
//! EVERY FRAME A CLIENT WROTE OR READ IS AUDITED, by every client of
//! every test, when it drops ([`Client::audit`]): what it wrote against
//! `ipc/frame.schema.json`, each request's `(method, params)` against
//! `ipc/request.schema.json`; what it read against the frame schema, each
//! event's `(event_type, data)` against `ipc/event.schema.json`, and each
//! `ok: true` result against the result schema its method names in
//! `LOCAL-IPC.md`'s method table -- read from that table, so the mapping
//! is the contract's and not a copy of it. The frame schema leaves an
//! event's data and a result `{}` and defers the pair, which is why those
//! halves exist (plan §16 exit gate (a)). A frame the server wrote and no
//! test read is not audited: what a test does not read, it does not see.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use interweave_ipc_protocol::{DecodedFrame, Frame, FrameError, decode_frame, encode_frame};
use interweave_ipc_server::{KeepalivePolicy, Limits, ServerConfig, SocketPaths, bind, serve};
use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{TransportError, TransportRuntime as _};
use interweave_transport_composition::{ComposedRuntime, CompositionOptions};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const PATIENCE: Duration = Duration::from_secs(10);

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tests/<pkg> is two levels below the root")
        .to_path_buf()
}

fn profile(trusted: &str) -> ProfileConfig {
    let doc = format!(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: [\"{trusted}\"]
endpoints:
  default_direct_endpoint: human
  entries:
    - id: human
      enabled: true
      advertise: false
    - id: agent
      enabled: true
      advertise: true
    - id: off
      enabled: false
      advertise: false
    - id: kept
      enabled: true
      advertise: false
      allowed_client_kinds: [claude-channel]
channels:
  desired: [general]
discovery:
  providers: []
"
    );
    serde_norway::from_str(&doc).expect("the document parses")
}

/// One composed runtime and the server over it.
struct Node {
    _root: tempfile::TempDir,
    paths: SocketPaths,
    stop: Option<oneshot::Sender<()>>,
    server: JoinHandle<()>,
    runtime: Option<ComposedRuntime>,
}

impl Node {
    async fn start(limits: Limits, keepalive: KeepalivePolicy) -> Self {
        let other = ProfileIdentity::generate()
            .transport_identity()
            .expect("peer");
        Self::start_with(
            &ProfileIdentity::generate(),
            &profile(other.as_str()),
            "/ip4/127.0.0.1/tcp/0",
            limits,
            keepalive,
        )
        .await
    }

    async fn start_with(
        identity: &ProfileIdentity,
        profile: &ProfileConfig,
        listen: &str,
        limits: Limits,
        keepalive: KeepalivePolicy,
    ) -> Self {
        let options = CompositionOptions {
            listen: vec![listen.to_owned()],
            ..CompositionOptions::default()
        };
        let runtime = ComposedRuntime::start(identity, profile, options)
            .await
            .expect("composes");
        let root = tempfile::tempdir().expect("tempdir");
        let run_dir = root.path().join("interweave");
        let paths = SocketPaths {
            data: run_dir.join("data.sock"),
            admin: run_dir.join("admin.sock"),
            run_dir,
        };
        let listeners = bind(&paths).expect("binds");
        let config = ServerConfig {
            peer: identity.transport_identity().expect("peer"),
            limits,
            keepalive,
            shutdown_grace: Duration::from_secs(1),
            command_deadline: Duration::from_secs(10),
            write_stall: interweave_ipc_server::WRITE_STALL,
        };
        let (stop, stopped) = oneshot::channel();
        let binding = runtime.sessions();
        let server = tokio::spawn(async move {
            let _ = serve(listeners, binding, config, async {
                let _ = stopped.await;
            })
            .await;
        });
        Self {
            _root: root,
            paths,
            stop: Some(stop),
            server,
            runtime: Some(runtime),
        }
    }

    async fn stop(mut self) {
        let _ = self.stop.take().expect("once").send(());
        self.server.await.expect("the server stops");
        self.runtime
            .take()
            .expect("once")
            .shutdown()
            .await
            .expect("the runtime stops");
    }
}

/// A raw client: frames written as bytes, frames read as bodies, and
/// both kept for [`Self::audit`].
struct Client {
    stream: UnixStream,
    buf: Vec<u8>,
    /// Every body the server wrote, in order.
    seen: Vec<String>,
    /// Every body this client wrote, in order.
    sent: Vec<String>,
    /// Set once audited, so an explicit audit is not repeated at drop.
    audited: bool,
}

impl Client {
    async fn connect(path: &Path) -> Self {
        Self {
            stream: UnixStream::connect(path).await.expect("connects"),
            buf: Vec::new(),
            seen: Vec::new(),
            sent: Vec::new(),
            audited: false,
        }
    }

    async fn send(&mut self, body: &str) {
        self.sent.push(body.to_owned());
        let frame = encode_frame(body).expect("a frame");
        self.stream.write_all(&frame).await.expect("sent");
    }

    /// Hold every frame this client wrote and read to its schemas (the
    /// module doc), and return the methods answered `ok: true`.
    fn audit(&mut self) -> BTreeSet<String> {
        self.audited = true;
        let schemas = Schemas::get();
        let mut methods: BTreeMap<String, String> = BTreeMap::new();
        for body in &self.sent {
            let value: Value = serde_json::from_str(body).expect("the client wrote json");
            assert_valid(&schemas.frame, &value, "a client frame");
            if value["type"] == "request" {
                let mut pair = serde_json::json!({"method": value["method"]});
                if let Some(params) = value.get("params") {
                    pair["params"] = params.clone();
                }
                assert_valid(&schemas.request, &pair, "a request's (method, params)");
                methods.insert(
                    value["id"].as_str().expect("an id").to_owned(),
                    value["method"].as_str().expect("a method").to_owned(),
                );
            }
        }
        let mut answered = BTreeSet::new();
        for body in &self.seen {
            let value: Value = serde_json::from_str(body).expect("the server wrote json");
            assert_valid(&schemas.frame, &value, "a server frame");
            if value["type"] == "event" {
                let pair = serde_json::json!({
                    "event_type": value["event_type"],
                    "data": value["data"],
                });
                assert_valid(&schemas.event, &pair, "an event's (event_type, data)");
            }
            if value["type"] == "response" && value["ok"] == true {
                let id = value["id"].as_str().expect("an id");
                let method = methods
                    .get(id)
                    .unwrap_or_else(|| panic!("a response to an id never asked: {body}"));
                let result = schemas
                    .results
                    .get(method)
                    .unwrap_or_else(|| panic!("{method} has no row in LOCAL-IPC.md's table"));
                assert_valid(result, &value["result"], method);
                answered.insert(method.clone());
            }
        }
        answered
    }

    /// The next frame, or `None` once the server closed the stream.
    async fn next(&mut self) -> Option<Frame> {
        tokio::time::timeout(PATIENCE, async {
            loop {
                match decode_frame(&self.buf) {
                    Ok(DecodedFrame { body, consumed }) => {
                        self.buf.drain(..consumed);
                        self.seen.push(body.clone());
                        return Some(Frame::parse(&body).expect("the server writes frames"));
                    }
                    Err(FrameError::Incomplete { .. }) => {}
                    Err(e) => panic!("the server wrote a bad frame: {e:?}"),
                }
                let mut chunk = [0_u8; 8192];
                let read = self.stream.read(&mut chunk).await.ok()?;
                if read == 0 {
                    return None;
                }
                self.buf.extend_from_slice(&chunk[..read]);
            }
        })
        .await
        .expect("the server answers in time")
    }

    /// The next frame that is not a push (`server_state`, `ping`).
    async fn reply(&mut self) -> Option<Frame> {
        loop {
            match self.next().await? {
                Frame::ServerState(_) | Frame::Ping(_) => {}
                frame => return Some(frame),
            }
        }
    }

    async fn hello(&mut self, hello: &str) {
        self.send(hello).await;
        match self.reply().await {
            Some(Frame::HelloResponse(_)) => {}
            other => panic!("a hello_response, got {other:?}"),
        }
    }

    async fn close_code(&mut self) -> TransportError {
        match self.reply().await {
            Some(Frame::Close(close)) => close.code,
            other => panic!("a close, got {other:?}"),
        }
    }

    async fn response(&mut self) -> String {
        match self.reply().await {
            Some(frame @ Frame::Response(_)) => frame.to_body(),
            other => panic!("a response, got {other:?}"),
        }
    }
}

impl Drop for Client {
    /// Every client is audited, whatever its test asserted: a test
    /// already failing is left to report its own failure.
    fn drop(&mut self) {
        if !self.audited && !std::thread::panicking() {
            let _ = self.audit();
        }
    }
}

const DATA: &str = r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
    "client":{"kind":"human-client"},"endpoint":{"id":"human"},
    "requested_capabilities":["events","commands"],"features":["keepalive"]}"#;
const ADMIN: &str = r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
    "client":{"kind":"transportctl"},
    "requested_capabilities":["admin.status","admin.endpoints"]}"#;

fn schema_docs(dir: &Path, out: &mut Vec<Value>) {
    for entry in std::fs::read_dir(dir).expect("schema dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            schema_docs(&path, out);
        } else if path.extension().is_some_and(|x| x == "json")
            && path.file_name().is_some_and(|n| n != "manifest.json")
        {
            let text = std::fs::read_to_string(&path).expect("read");
            out.push(serde_json::from_str(&text).expect("json"));
        }
    }
}

/// The validators every audit uses, built once: the frame, request and
/// event catalogues, and each method's result schema as `LOCAL-IPC.md`'s
/// method table names it.
struct Schemas {
    frame: jsonschema::Validator,
    request: jsonschema::Validator,
    event: jsonschema::Validator,
    results: BTreeMap<String, jsonschema::Validator>,
}

impl Schemas {
    fn get() -> &'static Self {
        static SCHEMAS: OnceLock<Schemas> = OnceLock::new();
        SCHEMAS.get_or_init(Self::build)
    }

    fn build() -> Self {
        let dir = root().join("architecture/contracts/schemas");
        let mut docs = Vec::new();
        schema_docs(&dir, &mut docs);
        let pairs: Vec<(String, jsonschema::Resource)> = docs
            .into_iter()
            .filter_map(|doc| {
                let id = doc.get("$id").and_then(Value::as_str)?.to_owned();
                Some((id, jsonschema::Resource::from_contents(doc)))
            })
            .collect();
        let registry = jsonschema::Registry::new()
            .extend(pairs)
            .expect("register")
            .prepare()
            .expect("prepare");
        let compile = |relative: &str| {
            let doc: Value =
                serde_json::from_str(&std::fs::read_to_string(dir.join(relative)).expect("read"))
                    .expect("json");
            jsonschema::options()
                .with_registry(&registry)
                .build(&doc)
                .unwrap_or_else(|e| panic!("{relative} compiles: {e}"))
        };
        let results = method_table()
            .into_iter()
            .map(|(method, result)| {
                let validator = compile(&result);
                (method, validator)
            })
            .collect();
        Self {
            frame: compile("ipc/frame.schema.json"),
            request: compile("ipc/request.schema.json"),
            event: compile("ipc/event.schema.json"),
            results,
        }
    }
}

fn assert_valid(validator: &jsonschema::Validator, value: &Value, what: &str) {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{what}: {value}: {errors:?}");
}

/// `LOCAL-IPC.md`'s method table, method -> its result schema's path
/// under `architecture/contracts/schemas`: `name` is `ipc/<name>`, and a
/// `family:name` is `<family>/<name>`. Every method of
/// `ipc/method.schema.json` has exactly one row, asserted here.
fn method_table() -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(root().join("architecture/contracts/LOCAL-IPC.md"))
        .expect("LOCAL-IPC.md");
    let header = "| Method | Domain | Capability | Params | Result | Since |";
    let start = text.find(header).expect("the method table");
    let mut table = BTreeMap::new();
    for line in text[start..].lines().skip(2) {
        if !line.starts_with('|') {
            break;
        }
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let method = cells[1].trim_matches('`');
        let result = cells[5].trim_matches('`');
        let path = match result.split_once(':') {
            Some((family, name)) => format!("{family}/{name}.schema.json"),
            None => format!("ipc/{result}.schema.json"),
        };
        assert!(
            table.insert(method.to_owned(), path).is_none(),
            "{method} has two rows"
        );
    }
    let catalogue: Value = serde_json::from_str(
        &std::fs::read_to_string(
            root().join("architecture/contracts/schemas/ipc/method.schema.json"),
        )
        .expect("read"),
    )
    .expect("json");
    let names: BTreeSet<&str> = catalogue["enum"]
        .as_array()
        .expect("an enum")
        .iter()
        .map(|m| m.as_str().expect("a name"))
        .collect();
    assert_eq!(
        table.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        names,
        "the table and the catalogue name the same methods"
    );
    table
}

/// Every class the server writes in a session -- `hello_response`,
/// `server_state`, `response`, a real `endpoint.lease_changed` event and
/// `close` -- validates against `ipc/frame.schema.json`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_frame_the_server_writes_validates_against_the_schema() {
    let node = Node::start(Limits::default(), KeepalivePolicy::default()).await;
    let mut data = Client::connect(&node.paths.data).await;
    data.hello(DATA).await;
    data.send(
        r#"{"type":"request","id":"1","method":"channel.join","params":{"channel":"general"}}"#,
    )
    .await;
    assert!(data.response().await.contains(r#""ok":true"#));
    let mut admin = Client::connect(&node.paths.admin).await;
    admin.hello(ADMIN).await;
    admin
        .send(r#"{"type":"request","id":"a","method":"admin.status"}"#)
        .await;
    assert!(admin.response().await.contains(r#""ok":true"#));
    admin
        .send(r#"{"type":"request","id":"r","method":"admin.endpoints.revoke","params":{"endpoint":"human"}}"#)
        .await;
    assert!(admin.response().await.contains(r#""ok":true"#));
    let lease_changed = loop {
        match data.next().await.expect("the lease notice") {
            Frame::Event(event) => break event,
            Frame::ServerState(_) | Frame::Ping(_) => {}
            other => panic!("an event, got {other:?}"),
        }
    };
    assert_eq!(lease_changed.event_type, "endpoint.lease_changed");
    let mut refused = Client::connect(&node.paths.data).await;
    refused
        .send(r#"{"type":"hello","ipc_version":{"major":3,"minor":0},"client":{"kind":"k"}}"#)
        .await;
    assert_eq!(
        refused.close_code().await,
        TransportError::VersionIncompatible
    );

    // Each frame is validated by its client's audit at drop; what this
    // test adds is that every class the server writes was seen.
    let mut classes = BTreeSet::new();
    for body in data.seen.iter().chain(&admin.seen).chain(&refused.seen) {
        let value: Value = serde_json::from_str(body).expect("json");
        classes.insert(value["type"].as_str().expect("type").to_owned());
    }
    for class in [
        "hello_response",
        "server_state",
        "response",
        "event",
        "close",
    ] {
        assert!(classes.contains(class), "no {class} was seen: {classes:?}");
    }
    drop((data, admin, refused));
    node.stop().await;
}

/// Each refusal of the hello phase closes with its own code, and the
/// socket -- not the frame -- decides the domain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_handshake_refusal_has_its_code() {
    let node = Node::start(Limits::default(), KeepalivePolicy::default()).await;
    let holder = {
        let mut client = Client::connect(&node.paths.data).await;
        client.hello(DATA).await;
        client
    };
    let cases = [
        (
            &node.paths.data,
            DATA.replace(r#""id":"human""#, r#""id":"nowhere""#),
            TransportError::EndpointUnknown,
        ),
        (
            &node.paths.data,
            DATA.to_owned(),
            TransportError::EndpointInUse,
        ),
        (
            &node.paths.data,
            DATA.replace(r#""id":"human""#, r#""id":"agent""#)
                .replace(r#","features":["keepalive"]"#, ""),
            TransportError::CapabilityDenied,
        ),
        (
            &node.paths.data,
            ADMIN.to_owned(),
            TransportError::CapabilityDenied,
        ),
        (
            &node.paths.admin,
            DATA.replace(r#""id":"human""#, r#""id":"agent""#),
            TransportError::CapabilityDenied,
        ),
        (
            &node.paths.data,
            r#"{"type":"request","id":"1","method":"admin.status"}"#.to_owned(),
            TransportError::ProtocolViolation,
        ),
        (
            &node.paths.data,
            DATA.replace(r#""id":"human""#, r#""id":"off""#),
            TransportError::EndpointDisabled,
        ),
        (
            &node.paths.data,
            DATA.replace(r#""id":"human""#, r#""id":"kept""#),
            TransportError::EndpointClientKindDenied,
        ),
        (
            &node.paths.data,
            DATA.replace(r#""id":"human""#, r#""id":"Not An Id!""#),
            TransportError::InvalidArgument,
        ),
    ];
    for (socket, hello, code) in cases {
        let mut client = Client::connect(socket).await;
        client.send(&hello).await;
        assert_eq!(client.close_code().await, code, "{hello}");
        assert!(client.next().await.is_none(), "and the stream ends");
        // The malformed claim is outside ipc/hello's grammar by design:
        // what is tested is the server's answer to it, so this client's
        // own frame leaves the audit and the server's `close` stays in it.
        if code == TransportError::InvalidArgument {
            client.sent.clear();
        }
    }
    drop(holder);
    node.stop().await;
}

/// After `hello`, each socket refuses the other domain's methods before
/// dispatch, and the connection stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_socket_refuses_the_other_domains_methods() {
    let node = Node::start(Limits::default(), KeepalivePolicy::default()).await;
    let mut data = Client::connect(&node.paths.data).await;
    data.hello(DATA).await;
    data.send(r#"{"type":"request","id":"1","method":"admin.endpoints.list"}"#)
        .await;
    assert!(data.response().await.contains("CapabilityDenied"));
    let mut admin = Client::connect(&node.paths.admin).await;
    admin.hello(ADMIN).await;
    admin
        .send(
            r#"{"type":"request","id":"2","method":"channel.join","params":{"channel":"general"}}"#,
        )
        .await;
    assert!(admin.response().await.contains("CapabilityDenied"));
    admin
        .send(r#"{"type":"request","id":"3","method":"admin.status"}"#)
        .await;
    let status = admin.response().await;
    assert!(
        status.contains(r#""cross_domain_capability_denied_total":2"#),
        "both refusals counted: {status}"
    );
    drop((data, admin));
    node.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_ceilings_hold_across_both_sockets() {
    let limits = Limits {
        max_clients: 2,
        max_admin_clients: 1,
    };
    let node = Node::start(limits, KeepalivePolicy::default()).await;
    let mut admin = Client::connect(&node.paths.admin).await;
    admin.hello(ADMIN).await;
    let mut second_admin = Client::connect(&node.paths.admin).await;
    assert_eq!(
        second_admin.close_code().await,
        TransportError::Overloaded,
        "the admin sublimit"
    );
    let mut data = Client::connect(&node.paths.data).await;
    data.hello(DATA).await;
    let mut third = Client::connect(&node.paths.data).await;
    assert_eq!(
        third.close_code().await,
        TransportError::Overloaded,
        "the total"
    );
    drop((admin, data));
    node.stop().await;
}

/// On the real runtime: unanswered probes close the connection, and the
/// lease it held is free for the next client at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lapsed_keepalive_releases_the_lease() {
    let keepalive = KeepalivePolicy {
        interval: Duration::from_millis(100),
        response_timeout: Duration::from_millis(100),
        max_missed: 2,
        ..KeepalivePolicy::default()
    };
    let node = Node::start(Limits::default(), keepalive).await;
    let mut lapsed = Client::connect(&node.paths.data).await;
    lapsed.hello(DATA).await;
    assert_eq!(lapsed.close_code().await, TransportError::Timeout);
    let mut next = Client::connect(&node.paths.data).await;
    next.hello(DATA).await;
    drop(next);
    node.stop().await;
}

/// A data connection that claims no endpoint may still hold `commands`
/// (LOCAL-IPC.md, A 2026-09-30): it joins and publishes, and its
/// `direct.send` is answered `EndpointNotRegistered` at the port -- it
/// has no lease, so no source, not a forged one. That the unleased
/// session never RECEIVES a direct message is item 1 of
/// `tests/local-client-conformance`, which needs a second runtime to send
/// one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unleased_connection_holds_commands_and_cannot_send_direct() {
    use interweave_ipc_protocol::{ChannelParams, PublishParams, Request, RequestId, SendParams};
    use interweave_transport_api::{ChannelId, MessageId, Payload};
    let node = Node::start(Limits::default(), KeepalivePolicy::default()).await;
    let mut unleased = Client::connect(&node.paths.data).await;
    unleased
        .send(
            r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
            "client":{"kind":"broadcaster"},"requested_capabilities":["commands","events"]}"#,
        )
        .await;
    match unleased.reply().await {
        Some(Frame::HelloResponse(response)) => {
            assert_eq!(response.lease, None, "no endpoint claimed, none granted");
            assert_eq!(
                response.granted_capabilities.len(),
                2,
                "commands and events both granted: {response:?}"
            );
        }
        other => panic!("a hello_response, got {other:?}"),
    }
    let request = |id: &str, request: Request| {
        Frame::Request(request.into_frame(RequestId::new(id).expect("id"), None)).to_body()
    };
    let general = ChannelId::parse("general").expect("channel");
    unleased
        .send(&request(
            "join",
            Request::ChannelJoin(ChannelParams {
                channel: general.clone(),
            }),
        ))
        .await;
    let joined = unleased.response().await;
    assert!(joined.contains(r#""ok":true"#), "joined: {joined}");
    unleased
        .send(&request(
            "publish",
            Request::BroadcastPublish(PublishParams {
                channel: general,
                message_id: MessageId::from_bytes([9; 16]),
                payload: Payload::at_ceiling(None, b"to the channel".to_vec()).expect("payload"),
            }),
        ))
        .await;
    let published = unleased.response().await;
    assert!(published.contains(r#""ok":true"#), "published: {published}");
    let someone = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer");
    unleased
        .send(&request(
            "send",
            Request::DirectSend(SendParams {
                peer: someone,
                endpoint: None,
                message_id: MessageId::from_bytes([10; 16]),
                payload: Payload::at_ceiling(None, b"nowhere".to_vec()).expect("payload"),
            }),
        ))
        .await;
    let refused = unleased.response().await;
    assert!(
        refused.contains("EndpointNotRegistered"),
        "no lease, no source, no send: {refused}"
    );
    drop(unleased);
    node.stop().await;
}

/// The golden client frames of `fixtures/ipc-v2/ipc-v2-frame-golden.json`
/// are accepted as written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_golden_client_frames_are_accepted() {
    let fixture: Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("fixtures/ipc-v2/ipc-v2-frame-golden.json"))
            .expect("fixture"),
    )
    .expect("json");
    let body = |name: &str| -> String {
        fixture["vectors"]
            .as_array()
            .expect("vectors")
            .iter()
            .find(|v| v["name"] == name)
            .unwrap_or_else(|| panic!("no vector {name}"))["body"]
            .to_string()
    };
    let node = Node::start(Limits::default(), KeepalivePolicy::default()).await;
    let mut admin = Client::connect(&node.paths.admin).await;
    let minimal = body("hello-minimal");
    admin.send(&minimal).await;
    assert!(matches!(admin.reply().await, Some(Frame::HelloResponse(_))));
    admin.send(&body("request-admin-status")).await;
    // hello-minimal asks for no capability, so the status request is
    // refused -- but answered, on a connection that stays.
    assert!(admin.response().await.contains("CapabilityDenied"));
    admin.send(&body("cancel")).await;
    admin
        .send(r#"{"type":"request","id":"after","method":"admin.status"}"#)
        .await;
    assert!(
        admin.response().await.contains(r#""id":"after""#),
        "a cancel of nothing is silent"
    );
    drop(admin);
    node.stop().await;
}

/// Two nodes that trust each other, the subject naming the other in its
/// static bootstrap, on the host's private address (a learned loopback
/// address is refused, ADR-0052).
fn pair_profile(trusted: &str, statics: &[String]) -> ProfileConfig {
    let peers: Vec<String> = statics.iter().map(|s| format!("\"{s}\"")).collect();
    let doc = format!(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: [\"{trusted}\"]
endpoints:
  default_direct_endpoint: human
  directory: {{ enabled: true }}
  entries:
    - id: human
      enabled: true
      advertise: true
    - id: agent
      enabled: true
      advertise: true
channels:
  desired: [general]
discovery:
  providers:
    - type: static-bootstrap
      enabled: true
      priority: 10
      config:
        peers: [{}]
",
        peers.join(", ")
    );
    serde_norway::from_str(&doc).expect("the document parses")
}

/// One request frame's body.
fn request_body(id: &str, request: interweave_ipc_protocol::Request) -> String {
    use interweave_ipc_protocol::RequestId;
    Frame::Request(request.into_frame(RequestId::new(id).expect("id"), None)).to_body()
}

/// Ask until answered `ok: true`, a fresh id each time: for the two
/// methods that need the other node reached first.
async fn until_ok(
    client: &mut Client,
    name: &str,
    mut request: impl FnMut() -> interweave_ipc_protocol::Request,
) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    for attempt in 0_u32.. {
        client
            .send(&request_body(&format!("{name}-{attempt}"), request()))
            .await;
        let answer = client.response().await;
        if answer.contains(r#""ok":true"#) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{name} never answered ok: {answer}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Every method of the catalogue answered `ok: true` over the wire, so
/// every result schema is held to a frame a running server wrote -- the
/// ones no other test here reaches (`send-result`, `set-enabled-result`,
/// `endpoints:directory-response`) included -- and every params schema
/// to a frame a client wrote. The audits' union is asserted to be the
/// whole catalogue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_method_is_answered_ok_and_held_to_its_schemas() {
    use interweave_ipc_protocol::{
        ChannelParams, EndpointParams, PublishParams, QueryParams, Request, SendParams,
        SetDefaultParams, SetEnabledParams, ShutdownParams,
    };
    use interweave_transport_api::{ChannelId, EndpointId, MessageId, Payload};
    let ip = interweave_test_support::net::require_private_interface_v4();
    let (a_id, b_id) = (ProfileIdentity::generate(), ProfileIdentity::generate());
    let (a, b) = (
        a_id.transport_identity().expect("peer"),
        b_id.transport_identity().expect("peer"),
    );
    let port = std::net::TcpListener::bind((ip, 0))
        .expect("a free port")
        .local_addr()
        .expect("addr")
        .port();
    let b_listen = format!("/ip4/{ip}/tcp/{port}");
    let target = Node::start_with(
        &b_id,
        &pair_profile(a.as_str(), &[]),
        &b_listen,
        Limits::default(),
        KeepalivePolicy::default(),
    )
    .await;
    let subject = Node::start_with(
        &a_id,
        &pair_profile(b.as_str(), &[format!("{b_listen}/p2p/{}", b.as_str())]),
        &format!("/ip4/{ip}/tcp/0"),
        Limits::default(),
        KeepalivePolicy::default(),
    )
    .await;
    let mut receiver = Client::connect(&target.paths.data).await;
    receiver.hello(DATA).await;
    let mut data = Client::connect(&subject.paths.data).await;
    data.hello(
        r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
        "client":{"kind":"human-client"},"endpoint":{"id":"human"},
        "requested_capabilities":["events","commands","endpoints.query"],
        "features":["keepalive"]}"#,
    )
    .await;
    let mut admin = Client::connect(&subject.paths.admin).await;
    admin
        .hello(
            r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
            "client":{"kind":"transportctl"},
            "requested_capabilities":["admin.status","admin.endpoints","admin.shutdown"]}"#,
        )
        .await;

    let general = || ChannelId::parse("general").expect("channel");
    let endpoint = |id: &str| EndpointId::parse(id).expect("endpoint");
    let payload = || Payload::at_ceiling(None, b"held to its schema".to_vec()).expect("payload");
    let plain: [(&str, Request); 3] = [
        (
            "join",
            Request::ChannelJoin(ChannelParams { channel: general() }),
        ),
        (
            "publish",
            Request::BroadcastPublish(PublishParams {
                channel: general(),
                message_id: MessageId::from_bytes([1; 16]),
                payload: payload(),
            }),
        ),
        (
            "leave",
            Request::ChannelLeave(ChannelParams { channel: general() }),
        ),
    ];
    for (id, request) in plain {
        data.send(&request_body(id, request)).await;
        let answer = data.response().await;
        assert!(answer.contains(r#""ok":true"#), "{id}: {answer}");
    }
    let mut n = 0_u8;
    until_ok(&mut data, "send", || {
        n = n.wrapping_add(1);
        Request::DirectSend(SendParams {
            peer: b.clone(),
            endpoint: Some(endpoint("human")),
            message_id: MessageId::from_bytes([n; 16]),
            payload: payload(),
        })
    })
    .await;
    until_ok(&mut data, "query", || {
        Request::EndpointsQuery(QueryParams { peer: b.clone() })
    })
    .await;

    let admin_requests: [(&str, Request); 7] = [
        ("status", Request::AdminStatus),
        ("list", Request::AdminEndpointsList),
        (
            "disable",
            Request::AdminEndpointsSetEnabled(SetEnabledParams {
                endpoint: endpoint("agent"),
                enabled: false,
            }),
        ),
        (
            "enable",
            Request::AdminEndpointsSetEnabled(SetEnabledParams {
                endpoint: endpoint("agent"),
                enabled: true,
            }),
        ),
        (
            "default",
            Request::AdminEndpointsSetDefault(SetDefaultParams {
                endpoint: Some(endpoint("agent")),
            }),
        ),
        (
            "revoke",
            Request::AdminEndpointsRevoke(EndpointParams {
                endpoint: endpoint("agent"),
            }),
        ),
        (
            "shutdown",
            Request::AdminShutdown(ShutdownParams {
                grace_ms: Some(1_000),
            }),
        ),
    ];
    for (id, request) in admin_requests {
        admin.send(&request_body(id, request)).await;
        let answer = admin.response().await;
        assert!(answer.contains(r#""ok":true"#), "{id}: {answer}");
    }

    let mut answered = data.audit();
    answered.extend(admin.audit());
    assert_eq!(
        answered,
        method_table().into_keys().collect::<BTreeSet<_>>(),
        "every method of the catalogue answered ok and held to its result schema"
    );
    drop((receiver, data, admin));
    subject.stop().await;
    target.stop().await;
}
