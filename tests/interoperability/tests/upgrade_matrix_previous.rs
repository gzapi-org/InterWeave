// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The upgrade matrix's "previous build" axis (testing.md §Compatibility
//! fixtures, A 2026-10-08): each production build in
//! `tools/ci/previous-builds.txt`, built from its own commit as a second
//! binary, against HEAD.
//!
//! - **The entry set.** The built entries under `INTERWEAVE_PREVIOUS_BUILDS`
//!   are exactly the list's labels, each with both binaries. An entry added
//!   without its build fails here, and so does a stale build.
//! - **HEAD's client against the old daemon.** HEAD's IPC client opens a
//!   leased data session and an admin port on the old daemon's sockets,
//!   negotiating down to the minor the old build speaks.
//! - **The old client against HEAD's daemon.** The old `transportctl`,
//!   which was that build's only IPC client, reads `status` and the
//!   endpoint list from HEAD's IPC server. HEAD's server is bound where
//!   HEAD's own path rules put it for the profile, so this also holds the
//!   socket layout across builds.
//! - **Peer to peer.** The old daemon and HEAD's runtime exchange direct
//!   messages and broadcasts in both directions. The old side's sessions are
//!   HEAD's IPC client on the old daemon's socket.
//!
//! Every row is `#[ignore]`: a plain `cargo test` has no second binary.
//! CI builds the list and runs this file with `--ignored`, and then a missing
//! variable or entry FAILS, never skips (devex-tooling's interface,
//! 01a11c9e).

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use interweave_ipc_client::{IpcBinding, SocketPaths as ClientSockets};
use interweave_ipc_server::{KeepalivePolicy, Limits, ServerConfig, SocketPaths, bind, serve};
use interweave_local_client_api::{
    AdminBinding as _, AdminCapability, AdminPort as _, DataCapability, DataSessionBinding as _,
    DataSessionPort, SessionEvent, SessionRequest,
};
use interweave_local_client_conformance_tests::{PATIENCE, receive};
use interweave_profile_config::{ProfileConfig, ProfilePaths, XdgRoots};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointId, MessageId, Payload,
    TransportRuntime as _,
};
use interweave_transport_composition::{ComposedRuntime, CompositionOptions};
use tokio::sync::oneshot;

const VARIABLE: &str = "INTERWEAVE_PREVIOUS_BUILDS";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tests/<pkg> is two levels below the root")
        .to_path_buf()
}

/// `tools/ci/previous-builds.txt`: `<label> <sha>` per line; `#` comments
/// and blank lines skipped.
fn listed() -> Vec<(String, String)> {
    let path = root().join("tools/ci/previous-builds.txt");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let entries: Vec<(String, String)> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut words = l.split_whitespace();
            let label = words.next().expect("a label").to_owned();
            let sha = words
                .next()
                .unwrap_or_else(|| panic!("{label}: no sha"))
                .to_owned();
            assert!(words.next().is_none(), "{l}: more than a label and a sha");
            (label, sha)
        })
        .collect();
    assert!(!entries.is_empty(), "{} lists no build", path.display());
    entries
}

/// The directory CI built into. Unset is a failure: these rows are only
/// ever run when CI means them to.
fn builds() -> PathBuf {
    PathBuf::from(
        std::env::var_os(VARIABLE)
            .unwrap_or_else(|| panic!("{VARIABLE} is unset: these rows need the previous builds")),
    )
}

/// One listed build's two binaries.
struct Entry {
    label: String,
    daemon: PathBuf,
    transportctl: PathBuf,
}

fn entries() -> Vec<Entry> {
    let dir = builds();
    listed()
        .into_iter()
        .map(|(label, _)| Entry {
            daemon: dir.join(&label).join("transport-daemon"),
            transportctl: dir.join(&label).join("transportctl"),
            label,
        })
        .collect()
}

#[test]
#[ignore = "needs INTERWEAVE_PREVIOUS_BUILDS"]
fn the_built_entries_are_exactly_the_list() {
    let dir = builds();
    let mut built: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    built.sort();
    let mut want: Vec<String> = listed().into_iter().map(|(l, _)| l).collect();
    want.sort();
    assert_eq!(
        built, want,
        "the built entries and tools/ci/previous-builds.txt differ"
    );
    for entry in entries() {
        for bin in [&entry.daemon, &entry.transportctl] {
            use std::os::unix::fs::PermissionsExt as _;
            let meta = std::fs::metadata(bin)
                .unwrap_or_else(|e| panic!("{}: {}: {e}", entry.label, bin.display()));
            assert!(
                meta.permissions().mode() & 0o111 != 0,
                "{}: not executable",
                bin.display()
            );
        }
    }
}

fn private_dir(path: &Path) {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .expect("a private directory");
}

/// One XDG tree for one profile. The old build resolves its paths in it
/// by its own rules, and HEAD by HEAD's.
struct Home {
    _root: tempfile::TempDir,
    roots: XdgRoots,
    profile: String,
}

impl Home {
    fn new(profile: &str) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let at = |n: &str| root.path().join(n);
        let roots = XdgRoots {
            config_home: at("config"),
            data_home: at("data"),
            state_home: at("state"),
            cache_home: at("cache"),
            runtime_dir: Some(at("run")),
        };
        private_dir(&at("run"));
        Self {
            _root: root,
            roots,
            profile: profile.to_owned(),
        }
    }

    /// HEAD's paths for this profile.
    fn paths(&self) -> ProfilePaths {
        ProfilePaths::resolve(&self.profile, &self.roots).expect("paths")
    }

    fn command(&self, bin: &Path) -> Command {
        let mut c = Command::new(bin);
        c.args(["--profile", &self.profile])
            .env_clear()
            .env("XDG_CONFIG_HOME", &self.roots.config_home)
            .env("XDG_DATA_HOME", &self.roots.data_home)
            .env("XDG_STATE_HOME", &self.roots.state_home)
            .env("XDG_CACHE_HOME", &self.roots.cache_home)
            .env(
                "XDG_RUNTIME_DIR",
                self.roots.runtime_dir.as_ref().expect("run"),
            );
        c
    }

    fn write_config(&self, yaml: &str) {
        let file = self.paths().config_file();
        private_dir(file.parent().expect("a config directory"));
        std::fs::write(file, yaml).expect("the profile written");
    }

    fn client(&self) -> IpcBinding {
        let p = self.paths();
        IpcBinding::new(
            ClientSockets {
                data: p.data_socket().expect("data"),
                admin: p.admin_socket().expect("admin"),
            },
            "matrix-admin",
        )
    }
}

/// An old daemon process; killed on drop, its log kept for the failure
/// message.
struct OldDaemon {
    child: Child,
    log: PathBuf,
}

impl OldDaemon {
    async fn start(home: &Home, bin: &Path) -> Self {
        let log = home.roots.state_home.with_file_name("daemon.log");
        let child = home
            .command(bin)
            .arg("--create-identity")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).expect("a log"))
            .spawn()
            .expect("the old daemon starts");
        let mut daemon = Self { child, log };
        // Serving once its admin socket answers a status: a socket file is
        // not enough, a killed daemon leaves one behind.
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let ready = async {
                let admin = home
                    .client()
                    .admin([AdminCapability::Status].into())
                    .await
                    .ok()?;
                admin.status().await.ok()
            };
            if ready.await.is_some() {
                return daemon;
            }
            if let Ok(Some(status)) = daemon.child.try_wait() {
                panic!("the old daemon exited ({status}):\n{}", daemon.log());
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the old daemon never served:\n{}",
                daemon.log()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for OldDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A profile both builds read: only keys the Stage 13 build already had.
fn profile_yaml(name: &str, trusted: &str, listen: &str, statics: &[String]) -> String {
    let providers = if statics.is_empty() {
        "[]".to_owned()
    } else {
        format!(
            "\n    - type: static-bootstrap\n      enabled: true\n      priority: 5\n      config:\n        peers: [{}]",
            statics
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "schema_version: 2
profile: {{ name: {name} }}
transport:
  listen: {{ addresses: [\"{listen}\"] }}
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
  providers: {providers}
"
    )
}

fn human_session() -> SessionRequest {
    SessionRequest::new(
        "human-client",
        Some(EndpointId::parse("human").expect("valid")),
        [DataCapability::Commands, DataCapability::Events],
    )
    .expect("in bounds")
}

fn stranger() -> String {
    ProfileIdentity::generate()
        .transport_identity()
        .expect("peer")
        .as_str()
        .to_owned()
}

/// Row: HEAD's IPC client on the old daemon's sockets — a leased data
/// session and the admin status, at the minor the old build speaks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs INTERWEAVE_PREVIOUS_BUILDS"]
async fn heads_client_is_served_by_each_previous_daemon() {
    for entry in entries() {
        let home = Home::new("old");
        home.write_config(&profile_yaml(
            "old",
            &stranger(),
            "/ip4/127.0.0.1/tcp/0",
            &[],
        ));
        let daemon = OldDaemon::start(&home, &entry.daemon).await;

        let admin = home
            .client()
            .admin([AdminCapability::Status].into())
            .await
            .unwrap_or_else(|e| panic!("{}: admin port: {e:?}\n{}", entry.label, daemon.log()));
        let status = admin.status().await.expect("status");
        assert!(
            status.peer.as_str().starts_with("12D3KooW"),
            "{}: {status:?}",
            entry.label
        );

        let session = home
            .client()
            .open(human_session())
            .await
            .unwrap_or_else(|e| panic!("{}: lease: {e:?}\n{}", entry.label, daemon.log()));
        session
            .join(ChannelId::parse("interop").expect("valid"))
            .await
            .unwrap_or_else(|e| panic!("{}: join: {e:?}", entry.label));
        session.close().await.expect("closes");
        drop(daemon);
    }
}

/// HEAD's daemon, in-process: the composed runtime and HEAD's IPC server,
/// bound where HEAD's path rules put the profile's sockets.
struct HeadDaemon {
    stop: Option<oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<()>,
    runtime: Option<ComposedRuntime>,
}

impl HeadDaemon {
    async fn start(
        home: &Home,
        identity: &ProfileIdentity,
        profile: &ProfileConfig,
        listen: &str,
    ) -> Self {
        let runtime = ComposedRuntime::start(
            identity,
            profile,
            CompositionOptions {
                listen: vec![listen.to_owned()],
                ..CompositionOptions::default()
            },
        )
        .await
        .expect("HEAD composes");
        let p = home.paths();
        let paths = SocketPaths {
            data: p.data_socket().expect("data"),
            admin: p.admin_socket().expect("admin"),
            run_dir: p
                .data_socket()
                .expect("data")
                .parent()
                .expect("a run dir")
                .to_path_buf(),
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
            stop: Some(stop),
            server,
            runtime: Some(runtime),
        }
    }

    fn runtime(&self) -> &ComposedRuntime {
        self.runtime.as_ref().expect("running")
    }

    async fn stop(mut self) {
        let _ = self.stop.take().expect("once").send(());
        self.server.await.expect("the server stops");
        self.runtime
            .take()
            .expect("once")
            .shutdown()
            .await
            .expect("HEAD stops");
    }
}

/// Row: the old `transportctl` against HEAD's daemon — `status` and
/// `endpoints list` answered, the profile's own `PeerId` in the status.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs INTERWEAVE_PREVIOUS_BUILDS"]
async fn each_previous_transportctl_is_served_by_heads_daemon() {
    for entry in entries() {
        let home = Home::new("head");
        let identity = ProfileIdentity::generate();
        let peer = identity.transport_identity().expect("peer");
        let profile: ProfileConfig = serde_norway::from_str(&profile_yaml(
            "head",
            &stranger(),
            "/ip4/127.0.0.1/tcp/0",
            &[],
        ))
        .expect("parses");
        let head = HeadDaemon::start(&home, &identity, &profile, "/ip4/127.0.0.1/tcp/0").await;

        for args in [&["status"][..], &["endpoints", "list"][..]] {
            let out = home
                .command(&entry.transportctl)
                .args(args)
                .stdin(Stdio::null())
                .output()
                .expect("the old transportctl runs");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success(),
                "{}: transportctl {args:?}: {}\n{stdout}\n{}",
                entry.label,
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
            if args == ["status"] {
                assert!(
                    stdout.contains(peer.as_str()),
                    "{}: status names HEAD's peer: {stdout}",
                    entry.label
                );
            } else {
                assert!(
                    stdout.contains("human"),
                    "{}: endpoints list: {stdout}",
                    entry.label
                );
            }
        }
        head.stop().await;
    }
}

/// A port free on `ip` now, so HEAD's address can be written into the
/// old daemon's profile before HEAD starts.
fn free_port(ip: Ipv4Addr) -> u16 {
    TcpListener::bind((ip, 0))
        .expect("binds")
        .local_addr()
        .expect("an address")
        .port()
}

/// Row: the old daemon and HEAD's runtime, peer to peer — direct both ways
/// and broadcast both ways.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs INTERWEAVE_PREVIOUS_BUILDS"]
async fn each_previous_daemon_exchanges_with_head_both_ways() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    for entry in entries() {
        // HEAD's identity and address first, so the old profile can name
        // them; the old daemon creates its own key, read back from its status.
        let head_identity = ProfileIdentity::generate();
        let head_peer = head_identity.transport_identity().expect("peer");
        let head_listen = format!("/ip4/{ip}/tcp/{}", free_port(ip));
        let head_route = format!("{head_listen}/p2p/{}", head_peer.as_str());

        let old_home = Home::new("old");
        // A port picked for the old daemon too: HEAD starts second, so HEAD
        // holds a static route to the old daemon, which is already up.
        let old_listen = format!("/ip4/{ip}/tcp/{}", free_port(ip));
        old_home.write_config(&profile_yaml(
            "old",
            head_peer.as_str(),
            &old_listen,
            &[head_route],
        ));
        let old = OldDaemon::start(&old_home, &entry.daemon).await;
        let old_peer = old_home
            .client()
            .admin([AdminCapability::Status].into())
            .await
            .expect("admin")
            .status()
            .await
            .expect("status")
            .peer;

        let head_profile: ProfileConfig = serde_norway::from_str(&profile_yaml(
            "head",
            old_peer.as_str(),
            &head_listen,
            &[format!("{old_listen}/p2p/{}", old_peer.as_str())],
        ))
        .expect("parses");
        let head_home = Home::new("head");
        let head = HeadDaemon::start(&head_home, &head_identity, &head_profile, &head_listen).await;

        let at_head = head
            .runtime()
            .sessions()
            .open(human_session())
            .await
            .expect("leases");
        let at_old = old_home
            .client()
            .open(human_session())
            .await
            .expect("leases");
        let human = EndpointId::parse("human").expect("valid");
        let payload = |s: &str| Payload::at_ceiling(None, s.as_bytes().to_vec()).expect("fits");

        // Direct, HEAD to old: retried while the old daemon's static route
        // to HEAD has not yet connected.
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let sent = at_head
                .send_direct(
                    DirectDestination {
                        peer: old_peer.clone(),
                        endpoint: Some(human.clone()),
                    },
                    MessageId::from_bytes([1; 16]),
                    payload("head to old"),
                )
                .await;
            match sent {
                Ok(endpoint) => {
                    assert_eq!(endpoint, human, "{}", entry.label);
                    break;
                }
                Err(e) => assert!(
                    tokio::time::Instant::now() < deadline,
                    "{}: HEAD's send never accepted: {e:?}\n{}",
                    entry.label,
                    old.log()
                ),
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let got = receive(&at_old, PATIENCE).await;
        assert!(
            got.iter().any(
                |e| matches!(e, SessionEvent::Direct(m) if m.payload.bytes() == b"head to old")
            ),
            "{}: the old daemon's session got: {got:?}",
            entry.label
        );

        // Direct, old to HEAD.
        at_old
            .send_direct(
                DirectDestination {
                    peer: head_peer.clone(),
                    endpoint: Some(human.clone()),
                },
                MessageId::from_bytes([2; 16]),
                payload("old to head"),
            )
            .await
            .unwrap_or_else(|e| panic!("{}: the old daemon's send: {e:?}", entry.label));
        let got = receive(&at_head, PATIENCE).await;
        assert!(
            got.iter().any(
                |e| matches!(e, SessionEvent::Direct(m) if m.payload.bytes() == b"old to head")
            ),
            "{}: HEAD's session got: {got:?}",
            entry.label
        );

        // Broadcast both ways: published until it arrives, the mesh forming
        // on its own schedule.
        let channel = ChannelId::parse("interop").expect("valid");
        at_head.join(channel.clone()).await.expect("joins");
        at_old.join(channel.clone()).await.expect("joins");
        let ctx = format!("{}\n{}", entry.label, old.log());
        broadcast_until_received(&at_head, &at_old, &channel, "head broadcast", &ctx).await;
        broadcast_until_received(&at_old, &at_head, &channel, "old broadcast", &ctx).await;

        at_head.close().await.expect("closes");
        at_old.close().await.expect("closes");
        head.stop().await;
        drop(old);
    }
}

/// Publish on `channel` from `from` until `to` receives it: the mesh forms
/// on its own schedule, and each republish is a new message.
async fn broadcast_until_received<F, T>(
    from: &F,
    to: &T,
    channel: &ChannelId,
    what: &str,
    ctx: &str,
) where
    F: DataSessionPort,
    T: DataSessionPort,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut n = 0u8;
    loop {
        n = n.wrapping_add(1);
        let _ = from
            .broadcast(
                channel.clone(),
                BroadcastMessageV1 {
                    message_id: MessageId::from_bytes([n; 16]),
                    sent_at_ms: 0,
                    payload: Payload::at_ceiling(None, what.as_bytes().to_vec()).expect("fits"),
                },
            )
            .await;
        let got = receive(to, Duration::from_millis(500)).await;
        if got.iter().any(
            |e| matches!(e, SessionEvent::Broadcast(m) if m.payload.bytes() == what.as_bytes()),
        ) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} never arrived: {ctx}"
        );
    }
}
