// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! IPC v2 on the wire: the server over a runtime composed from a profile,
//! driven by raw frames on real Unix sockets. What a client sees, held to
//! the frozen schemas -- not what the server's types promise.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
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
        let identity = ProfileIdentity::generate();
        let other = ProfileIdentity::generate()
            .transport_identity()
            .expect("peer");
        let options = CompositionOptions {
            listen: vec!["/ip4/127.0.0.1/tcp/0".to_owned()],
            ..CompositionOptions::default()
        };
        let runtime = ComposedRuntime::start(&identity, &profile(other.as_str()), options)
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

/// A raw client: frames written as bytes, frames read as bodies.
struct Client {
    stream: UnixStream,
    buf: Vec<u8>,
    /// Every body the server wrote, in order.
    seen: Vec<String>,
}

impl Client {
    async fn connect(path: &Path) -> Self {
        Self {
            stream: UnixStream::connect(path).await.expect("connects"),
            buf: Vec::new(),
            seen: Vec::new(),
        }
    }

    async fn send(&mut self, body: &str) {
        let frame = encode_frame(body).expect("a frame");
        self.stream.write_all(&frame).await.expect("sent");
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

fn frame_validator() -> jsonschema::Validator {
    let mut docs = Vec::new();
    schema_docs(&root().join("architecture/contracts/schemas"), &mut docs);
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
    let schema = root().join("architecture/contracts/schemas/ipc/frame.schema.json");
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(schema).expect("read")).expect("json");
    jsonschema::options()
        .with_registry(&registry)
        .build(&doc)
        .expect("compiles")
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

    let validator = frame_validator();
    let mut classes = std::collections::BTreeSet::new();
    for body in data.seen.iter().chain(&admin.seen).chain(&refused.seen) {
        let value: Value = serde_json::from_str(body).expect("json");
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{body}: {errors:?}");
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
    ];
    for (socket, hello, code) in cases {
        let mut client = Client::connect(socket).await;
        client.send(&hello).await;
        assert_eq!(client.close_code().await, code, "{hello}");
        assert!(client.next().await.is_none(), "and the stream ends");
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
