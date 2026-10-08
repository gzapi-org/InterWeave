// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The upgrade matrix's IPC rows that need no older build (testing.md
//! §Compatibility fixtures, A 2026-10-08), against HEAD's daemon over a
//! real Unix socket:
//!
//! - **unsupported major**: a hello of major 3 is answered
//!   `close{VersionIncompatible, supported: [{2, IPC_MAX_MINOR}]}` and the
//!   connection ends. No `hello_response` comes first, so there is no
//!   half-handshake.
//! - **minor bump**: a hello offering a minor above HEAD's negotiates
//!   `min(client, server)`, HEAD's own, and the connection then serves a
//!   request.
//! - **lower minor from its frozen vector**: the 2.0 golden client frames,
//!   written to the socket byte for byte, negotiate minor 0 and are served.
//!
//! Every frame the daemon writes is read through the INDEPENDENT codec
//! (`interweave-independent-codecs`), not production's, so each row also
//! decodes production-encoded frames captured from the wire. The rows that
//! need a previous build are the matrix's other axis. Its first entry is
//! the Stage 13 build that spoke IPC 2.0, and the second-binary build in CI
//! is devex-tooling's.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use interweave_independent_codecs::ipc_v2::{self, Class, Envelope};
use interweave_independent_codecs::json::Value;
use interweave_ipc_protocol::IPC_MAX_MINOR;
use interweave_ipc_server::{KeepalivePolicy, Limits, ServerConfig, SocketPaths, bind, serve};
use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportRuntime as _;
use interweave_transport_composition::{ComposedRuntime, CompositionOptions};
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

/// The golden vectors' `frame_hex` by name: the exact bytes a 2.0 client
/// wrote, never re-serialized here.
fn golden_frame(name: &str) -> Vec<u8> {
    let text = std::fs::read_to_string(root().join("fixtures/ipc-v2/ipc-v2-frame-golden.json"))
        .expect("the fixture");
    let doc: serde_json::Value = serde_json::from_str(&text).expect("json");
    let hex = doc["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .find(|v| v["name"] == name)
        .unwrap_or_else(|| panic!("no vector {name}"))["frame_hex"]
        .as_str()
        .expect("frame_hex")
        .to_owned();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

/// The golden `hello-data-client` claims `human` with `events` and
/// `commands` and joins `ops/alerts`, so the profile has that endpoint.
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
channels:
  desired: []
discovery:
  providers: []
"
    );
    serde_norway::from_str(&doc).expect("the document parses")
}

/// HEAD: one composed runtime and the IPC server over it.
struct Daemon {
    _root: tempfile::TempDir,
    paths: SocketPaths,
    stop: Option<oneshot::Sender<()>>,
    server: JoinHandle<()>,
    runtime: Option<ComposedRuntime>,
}

impl Daemon {
    async fn start() -> Self {
        let identity = ProfileIdentity::generate();
        let other = ProfileIdentity::generate()
            .transport_identity()
            .expect("peer");
        let root = tempfile::tempdir().expect("tempdir");
        let runtime = ComposedRuntime::start(
            &identity,
            &profile(other.as_str()),
            CompositionOptions {
                listen: vec!["/ip4/127.0.0.1/tcp/0".to_owned()],
                ..CompositionOptions::default()
            },
        )
        .await
        .expect("composes");
        let run_dir = root.path().join("interweave");
        let paths = SocketPaths {
            data: run_dir.join("data.sock"),
            admin: run_dir.join("admin.sock"),
            run_dir,
        };
        let listeners = bind(&paths).expect("binds");
        let config = ServerConfig {
            peer: identity.transport_identity().expect("peer"),
            limits: Limits::default(),
            keepalive: KeepalivePolicy::default(),
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

/// A client that writes bytes and reads every frame through the
/// independent codec.
struct Client {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Client {
    async fn connect(path: &Path) -> Self {
        Self {
            stream: UnixStream::connect(path).await.expect("connects"),
            buf: Vec::new(),
        }
    }

    async fn write(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.expect("written");
    }

    async fn send(&mut self, body: &str) {
        self.write(&ipc_v2::encode_frame(body.as_bytes()).expect("a frame"))
            .await;
    }

    /// The next frame, decoded by the independent codec; `None` at EOF.
    /// A frame the codec refuses fails the test: HEAD wrote something
    /// the contract text does not allow.
    async fn next(&mut self) -> Option<Envelope> {
        tokio::time::timeout(PATIENCE, async {
            loop {
                if self.buf.len() >= 4 {
                    match ipc_v2::split_frame(&self.buf) {
                        Ok((body, _)) => {
                            let n = 4 + body.len();
                            let env = Envelope::decode_body(body).unwrap_or_else(|e| {
                                panic!(
                                    "HEAD wrote a frame the contract refuses: {e}: {}",
                                    String::from_utf8_lossy(body)
                                )
                            });
                            self.buf.drain(..n);
                            return Some(env);
                        }
                        Err(e) if e.0.contains("declared") => {}
                        Err(e) => panic!("HEAD wrote a bad prefix: {e}"),
                    }
                }
                let mut chunk = [0_u8; 8192];
                let read = self.stream.read(&mut chunk).await.ok()?;
                if read == 0 {
                    assert!(self.buf.is_empty(), "EOF inside a frame");
                    return None;
                }
                self.buf.extend_from_slice(&chunk[..read]);
            }
        })
        .await
        .expect("HEAD answers in time")
    }

    /// The next frame that is not a push.
    async fn reply(&mut self) -> Option<Envelope> {
        loop {
            let env = self.next().await?;
            if !matches!(env.class, Class::ServerState | Class::Ping) {
                return Some(env);
            }
        }
    }
}

fn int(v: Option<&Value>) -> i64 {
    v.and_then(Value::as_i64).expect("an integer")
}

fn minor_of(hello_response: &Envelope) -> i64 {
    assert_eq!(
        hello_response.class,
        Class::HelloResponse,
        "{hello_response:?}"
    );
    let version = hello_response.body.get("ipc_version");
    assert_eq!(int(version.and_then(|v| v.get("major"))), 2);
    int(version.and_then(|v| v.get("minor")))
}

/// Unleased (no endpoint claim): the rows ask about the version, and a
/// lease would make each reconnect race the previous connection's release.
/// An unleased data connection holds `commands` and joins (LOCAL-IPC.md
/// §Handshake).
const DATA_HELLO: &str = r#"{"type":"hello","ipc_version":{"major":MAJOR,"minor":MINOR},"client":{"kind":"human-client"},"requested_capabilities":["events","commands"]}"#;

fn hello(major: u64, minor: u64) -> String {
    DATA_HELLO
        .replace("MAJOR", &major.to_string())
        .replace("MINOR", &minor.to_string())
}

/// Row: an unsupported major is refused clearly, with no half-handshake.
/// The control is the same hello at major 2, which is answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipc_unsupported_major_is_closed_version_incompatible_with_no_half_handshake() {
    let daemon = Daemon::start().await;

    let mut control = Client::connect(&daemon.paths.data).await;
    control.send(&hello(2, 0)).await;
    let first = control.reply().await.expect("a reply");
    assert_eq!(minor_of(&first), 0);
    drop(control);

    let mut refused = Client::connect(&daemon.paths.data).await;
    refused.send(&hello(3, 0)).await;
    // EVERY frame HEAD writes on this connection, pushes included: a
    // hello_response or any other class before the close would be a
    // half-handshake.
    let mut frames = Vec::new();
    while let Some(env) = refused.next().await {
        frames.push(env);
    }
    assert_eq!(frames.len(), 1, "exactly the close, then EOF: {frames:?}");
    let close = &frames[0];
    assert_eq!(close.class, Class::Close);
    assert_eq!(
        close.body.get("code").and_then(Value::as_str),
        Some("VersionIncompatible")
    );
    let Some(Value::Array(supported)) = close.body.get("supported") else {
        panic!("no supported list: {close:?}");
    };
    assert_eq!(supported.len(), 1, "{supported:?}");
    assert_eq!(int(supported[0].get("major")), 2);
    assert_eq!(
        int(supported[0].get("minor")),
        i64::try_from(IPC_MAX_MINOR).unwrap(),
        "supported names HEAD's highest minor, never an older literal"
    );

    daemon.stop().await;
}

/// Row: a minor above HEAD's negotiates HEAD's own, and the connection
/// serves. The control is a hello at HEAD's own minor, which gets the same
/// minor back, so the bump is shown to be what the server lowered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipc_minor_bump_negotiates_min_of_client_and_server_and_serves() {
    let daemon = Daemon::start().await;
    let head = IPC_MAX_MINOR;

    for (offered, why) in [
        (head, "HEAD's own"),
        (head + 1, "one above"),
        (head + 40, "far above"),
    ] {
        let mut client = Client::connect(&daemon.paths.data).await;
        client.send(&hello(2, offered)).await;
        let first = client.reply().await.expect("a reply");
        assert_eq!(
            minor_of(&first),
            i64::try_from(IPC_MAX_MINOR).unwrap(),
            "{why}: offered {offered}"
        );
        client
            .send(r#"{"type":"request","id":"j","method":"channel.join","params":{"channel":"ops/alerts"}}"#)
            .await;
        let answer = client.reply().await.expect("an answer");
        assert_eq!(answer.class, Class::Response, "{why}: {answer:?}");
        assert_eq!(
            answer.body.get("ok"),
            Some(&Value::Bool(true)),
            "{why}: {answer:?}"
        );
    }

    daemon.stop().await;
}

/// Row: the frozen 2.0 client frames, written byte for byte, negotiate
/// minor 0 and are served. A data hello of 2.0 and its channel join from
/// `ipc-v2-frame-golden.json`; an admin `hello-minimal` likewise.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipc_lower_minor_frozen_frames_are_served_by_head() {
    let daemon = Daemon::start().await;

    let mut data = Client::connect(&daemon.paths.data).await;
    data.write(&golden_frame("hello-data-client")).await;
    let first = data.reply().await.expect("a reply");
    assert_eq!(minor_of(&first), 0, "a 2.0 hello negotiates 2.0");
    data.write(&golden_frame("request-channel-join")).await;
    let answer = data.reply().await.expect("an answer");
    assert_eq!(answer.class, Class::Response, "{answer:?}");
    assert_eq!(
        answer.body.get("ok"),
        Some(&Value::Bool(true)),
        "{answer:?}"
    );
    assert_eq!(answer.body.get("id").and_then(Value::as_str), Some("1"));

    let mut admin = Client::connect(&daemon.paths.admin).await;
    admin.write(&golden_frame("hello-minimal")).await;
    assert_eq!(minor_of(&admin.reply().await.expect("a reply")), 0);

    daemon.stop().await;
}
