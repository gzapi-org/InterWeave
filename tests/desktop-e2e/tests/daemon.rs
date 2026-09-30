// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The transport daemon as a process (plan §16 Required suites,
//! `desktop-e2e`; exit gate (d)): started from a profile file alone, each
//! in its own XDG tree, stopped by signal or by an admin port, and driven
//! over its sockets. `transportctl` is B5's and is not here.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::fs::DirBuilder;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use interweave_ipc_client::{IpcBinding, SocketPaths};
use interweave_ipc_protocol::{DecodedFrame, Frame, FrameError, decode_frame, encode_frame};
use interweave_local_client_api::{
    AdminBinding as _, AdminCapability, AdminPort as _, DataCapability, DataSessionBinding as _,
    DataSessionPort as _, SessionEvent, SessionRequest,
};
use interweave_profile_config::{ProfilePaths, XdgRoots};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointId, MediaType, MessageId, Payload,
    TransportError, TransportIdentity,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const PATIENCE: Duration = Duration::from_secs(30);

/// The workspace's own `transport-daemon`, beside this test's build.
fn daemon_binary() -> PathBuf {
    let exe = std::env::current_exe().expect("this test's path");
    // target/<profile>/deps/<test> -> target/<profile>/transport-daemon
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .expect("a target directory")
        .join("transport-daemon");
    assert!(
        bin.exists(),
        "{} is missing: build it first (cargo build -p interweave-transport-daemon)",
        bin.display()
    );
    bin
}

fn private_dir(path: &Path) {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .expect("a private directory");
}

/// One daemon's world: its own XDG tree, and the paths the daemon itself
/// resolves in it.
struct Home {
    root: tempfile::TempDir,
    roots: XdgRoots,
    paths: ProfilePaths,
}

impl Home {
    fn new(profile: &str) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let mut home = Self::within(root.path(), profile);
        home.root = root;
        home
    }

    /// A home sharing `base`'s XDG tree -- two profiles of one user.
    fn within(base: &Path, profile: &str) -> Self {
        let root = tempfile::tempdir_in(base).expect("tempdir");
        let at = |name: &str| base.join(name);
        let roots = XdgRoots {
            config_home: at("config"),
            data_home: at("data"),
            state_home: at("state"),
            cache_home: at("cache"),
            runtime_dir: Some(at("run")),
        };
        private_dir(&at("run"));
        let paths = ProfilePaths::resolve(profile, &roots).expect("paths");
        Self { root, roots, paths }
    }

    fn write_config(&self, yaml: &str) {
        let file = self.paths.config_file();
        private_dir(file.parent().expect("a config directory"));
        std::fs::write(file, yaml).expect("the profile written");
    }

    /// An identity key where the profile's default names it.
    fn write_key(&self) -> TransportIdentity {
        let identity = ProfileIdentity::generate();
        let file = self.paths.identity_file();
        private_dir(file.parent().expect("an identity directory"));
        identity.save(&file).expect("the key saved");
        identity.transport_identity().expect("a peer id")
    }

    fn data_socket(&self) -> PathBuf {
        self.paths.data_socket().expect("a data socket path")
    }

    fn admin_socket(&self) -> PathBuf {
        self.paths.admin_socket().expect("an admin socket path")
    }

    fn binding(&self) -> IpcBinding {
        IpcBinding::new(
            SocketPaths {
                data: self.data_socket(),
                admin: self.admin_socket(),
            },
            "e2e-admin",
        )
    }

    /// Start the daemon for this home's profile, its stderr to a file.
    fn start(&self, extra: &[&str]) -> Daemon {
        let log = self.root.path().join(format!(
            "daemon-{}.log",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("a clock")
                .as_nanos()
        ));
        let stderr = std::fs::File::create(&log).expect("a log file");
        let env = |p: &Path| p.as_os_str().to_owned();
        let child = Command::new(daemon_binary())
            .args(["--profile", self.paths.profile()])
            .args(extra)
            .env_clear()
            .env("XDG_CONFIG_HOME", env(&self.roots.config_home))
            .env("XDG_DATA_HOME", env(&self.roots.data_home))
            .env("XDG_STATE_HOME", env(&self.roots.state_home))
            .env("XDG_CACHE_HOME", env(&self.roots.cache_home))
            .env(
                "XDG_RUNTIME_DIR",
                env(self.roots.runtime_dir.as_deref().expect("a runtime dir")),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("the daemon starts");
        Daemon { child, log }
    }
}

struct Daemon {
    child: Child,
    log: PathBuf,
}

impl Daemon {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Until both sockets accept a connection -- a socket FILE is not
    /// enough: a killed daemon leaves its stale ones behind -- or the
    /// daemon exits, which fails the test with its log.
    async fn serving(&mut self, home: &Home) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let accepted =
                |socket: PathBuf| std::os::unix::net::UnixStream::connect(socket).is_ok();
            if accepted(home.data_socket()) && accepted(home.admin_socket()) {
                return;
            }
            if let Some(status) = self.child.try_wait().expect("a status") {
                panic!(
                    "the daemon exited ({status}) instead of serving:\n{}",
                    self.log()
                );
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "not serving in time:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Its exit, within `PATIENCE`.
    async fn exit(&mut self) -> ExitStatus {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait().expect("a status") {
                return status;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "did not exit in time:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn terminate(&mut self) -> ExitStatus {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .expect("kill runs");
        assert!(status.success(), "SIGTERM sent");
        self.exit().await
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A small profile of one endpoint, `human`, listening on loopback.
fn profile(name: &str, trusted: &TransportIdentity, extra: &str) -> String {
    format!(
        "schema_version: 2
profile: {{ name: {name} }}
trust: {{ policy: static-allowlist, allowed_peers: [\"{}\"] }}
endpoints:
  default_direct_endpoint: human
  entries:
    - {{ id: human, enabled: true, advertise: false }}
channels: {{ desired: [] }}
discovery: {{ providers: [] }}
transport: {{ listen: {{ addresses: [\"/ip4/127.0.0.1/tcp/0\"] }} }}
{extra}",
        trusted.as_str()
    )
}

fn stranger() -> TransportIdentity {
    ProfileIdentity::generate()
        .transport_identity()
        .expect("a peer id")
}

fn human() -> EndpointId {
    EndpointId::parse("human").expect("endpoint")
}

fn lease_request() -> SessionRequest {
    SessionRequest::new(
        "human-client",
        Some(human()),
        [DataCapability::Events, DataCapability::Commands],
    )
    .expect("a request")
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).expect("metadata").mode() & 0o777
}

/// From a profile file alone: owner-only sockets in an owner-only
/// directory; SIGTERM stops it cleanly, unlinks both sockets and releases
/// the lock without unlinking it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_serves_owner_only_sockets_and_stops_cleanly_on_sigterm() {
    let home = Home::new("e2e");
    home.write_key();
    home.write_config(&profile("e2e", &stranger(), ""));
    let mut daemon = home.start(&[]);
    daemon.serving(&home).await;
    let run_dir = home
        .data_socket()
        .parent()
        .expect("a run dir")
        .to_path_buf();
    assert_eq!(mode(&run_dir), 0o700, "the run directory");
    assert_eq!(mode(&home.data_socket()), 0o600, "the data socket");
    assert_eq!(mode(&home.admin_socket()), 0o600, "the admin socket");
    let status = daemon.terminate().await;
    assert!(status.success(), "a clean stop: {status}\n{}", daemon.log());
    assert!(!home.data_socket().exists() && !home.admin_socket().exists());
    assert!(
        home.paths.state_dir().join("profile.lock").exists(),
        "the lock is released, never unlinked"
    );
}

/// An admin port's `admin.shutdown` stops the daemon as a signal does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_admin_shutdown_stops_the_daemon() {
    let home = Home::new("e2e");
    home.write_key();
    home.write_config(&profile("e2e", &stranger(), ""));
    let mut daemon = home.start(&[]);
    daemon.serving(&home).await;
    let admin = home
        .binding()
        .admin([AdminCapability::Shutdown].into())
        .await
        .expect("an admin port");
    admin
        .shutdown(Duration::from_millis(500))
        .await
        .expect("asked");
    let status = daemon.exit().await;
    assert!(status.success(), "{status}\n{}", daemon.log());
    assert!(!home.admin_socket().exists());
}

/// An admin shutdown's grace is the one the runtime settles for: with a
/// direct exchange in flight to a peer that never answers, a 200 ms grace
/// ends the daemon well inside the default's five seconds. The send is
/// retried until it is unanswered rather than refused, so the exchange is
/// in flight over a connection that exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_admin_shutdowns_grace_reaches_the_runtime() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let silent = interweave_test_support::silent::silent_direct_peer(ip).await;
    let silent_peer = TransportIdentity::parse(&silent.peer).expect("a peer id");
    let home = Home::new("e2e");
    home.write_key();
    let config = profile("e2e", &silent_peer, "")
        .replace("/ip4/127.0.0.1/tcp/0", &format!("/ip4/{ip}/tcp/0"))
        .replace(
            "discovery: { providers: [] }",
            &format!(
                "discovery: {{ providers: [{{ type: static-bootstrap, enabled: true, \
                 priority: 10, config: {{ peers: [\"{}\"] }} }}] }}",
                silent.address
            ),
        );
    home.write_config(&config);
    let mut daemon = home.start(&[]);
    daemon.serving(&home).await;
    let session = home.binding().open(lease_request()).await.expect("leases");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let sent = tokio::time::timeout(
            Duration::from_millis(500),
            session.send_direct(
                DirectDestination {
                    peer: silent_peer.clone(),
                    endpoint: Some(human()),
                },
                MessageId::from_bytes([9; 16]),
                Payload::at_ceiling(None, b"held".to_vec()).expect("within the ceiling"),
            ),
        )
        .await;
        match sent {
            Err(_) => break,
            Ok(answer) => assert!(
                tokio::time::Instant::now() < deadline,
                "never in flight: {answer:?}\n{}",
                daemon.log()
            ),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let admin = home
        .binding()
        .admin([AdminCapability::Shutdown].into())
        .await
        .expect("an admin port");
    let started = tokio::time::Instant::now();
    admin
        .shutdown(Duration::from_millis(200))
        .await
        .expect("asked");
    let status = daemon.exit().await;
    let waited = started.elapsed();
    assert!(status.success(), "{status}\n{}", daemon.log());
    assert!(
        waited < Duration::from_secs(3),
        "the admin's grace, not the default's: took {waited:?}\n{}",
        daemon.log()
    );
}

/// The lock makes "a daemon is running" a fact: a second daemon for the
/// same profile fails at once, naming the lock, and the first serves on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_daemon_for_the_profile_fails_fast() {
    let home = Home::new("e2e");
    home.write_key();
    home.write_config(&profile("e2e", &stranger(), ""));
    let mut first = home.start(&[]);
    first.serving(&home).await;
    let started = tokio::time::Instant::now();
    let mut second = home.start(&[]);
    let status = second.exit().await;
    assert_eq!(status.code(), Some(1), "refused: {}", second.log());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "fast: {:?}",
        started.elapsed()
    );
    assert!(second.log().contains("lock"), "{}", second.log());
    assert!(
        first.child.try_wait().expect("a status").is_none(),
        "the first serves on"
    );
    assert!(home.data_socket().exists());
}

/// A killed daemon leaves no lock -- flock dies with the process -- and
/// leaves its sockets, which the next daemon, as the lock's holder,
/// replaces.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_9_leaves_no_lock_and_the_next_daemon_replaces_its_stale_sockets() {
    let home = Home::new("e2e");
    home.write_key();
    home.write_config(&profile("e2e", &stranger(), ""));
    let mut first = home.start(&[]);
    first.serving(&home).await;
    first.child.kill().expect("SIGKILL");
    first.child.wait().expect("reaped");
    assert!(
        home.data_socket().exists(),
        "a killed daemon leaves its socket"
    );
    let mut next = home.start(&[]);
    next.serving(&home).await;
    let session = home.binding().open(lease_request()).await;
    assert!(session.is_ok(), "the replaced socket serves: {session:?}");
    drop(session);
    assert!(next.terminate().await.success());
}

/// Two profiles of one user whose names differ by `-admin` run side by
/// side: `<profile>.admin.sock` cannot be another profile's data socket
/// (`LOCAL-IPC.md`, A 2026-10-01).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profiles_p_and_p_admin_run_side_by_side() {
    let root = tempfile::tempdir().expect("tempdir");
    let (p, p_admin) = (
        Home::within(root.path(), "p"),
        Home::within(root.path(), "p-admin"),
    );
    assert_ne!(
        p.admin_socket(),
        p_admin.data_socket(),
        "the names cannot meet"
    );
    let mut daemons = Vec::new();
    for h in [&p, &p_admin] {
        h.write_key();
        h.write_config(&profile(h.paths.profile(), &stranger(), ""));
        let mut daemon = h.start(&[]);
        daemon.serving(h).await;
        daemons.push(daemon);
    }
    for h in [&p, &p_admin] {
        let binding = h.binding();
        let port = binding
            .admin([AdminCapability::Status].into())
            .await
            .expect("each profile's own admin socket");
        port.status().await.expect("status");
    }
    for mut daemon in daemons {
        assert!(daemon.terminate().await.success());
    }
}

/// A socket in the daemon's place that another process is SERVING is
/// never replaced, though it is this user's: the lock proves only that no
/// daemon of this profile serves it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_socket_in_the_daemons_place_is_refused_and_left() {
    let home = Home::new("e2e");
    home.write_key();
    home.write_config(&profile("e2e", &stranger(), ""));
    private_dir(home.data_socket().parent().expect("a run dir"));
    let live = std::os::unix::net::UnixListener::bind(home.data_socket()).expect("a live socket");
    let mut daemon = home.start(&[]);
    assert_eq!(daemon.exit().await.code(), Some(1), "{}", daemon.log());
    assert!(daemon.log().contains("live socket"), "{}", daemon.log());
    assert!(
        std::os::unix::net::UnixStream::connect(home.data_socket()).is_ok(),
        "still served"
    );
    drop(live);
}

/// The sockets are bound before the runtime starts, so a runtime that
/// then fails to start -- here, a listen address the host refuses --
/// leaves no socket behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runtime_that_fails_to_start_leaves_no_socket() {
    let home = Home::new("e2e");
    home.write_key();
    let config =
        profile("e2e", &stranger(), "").replace("/ip4/127.0.0.1/tcp/0", "/ip4/192.0.2.1/tcp/4001");
    home.write_config(&config);
    let mut daemon = home.start(&[]);
    assert_eq!(daemon.exit().await.code(), Some(1), "{}", daemon.log());
    assert!(daemon.log().contains("runtime"), "{}", daemon.log());
    assert!(!home.data_socket().exists() && !home.admin_socket().exists());
}

/// The ORDER (lifecycle.md steps 4 then 5): given a socket's place that
/// refuses the bind AND a listen address that refuses the runtime, the
/// start is refused for the socket -- the runtime never started, so the
/// profile was never on the network.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sockets_are_judged_before_the_runtime_starts() {
    let home = Home::new("e2e");
    home.write_key();
    let config =
        profile("e2e", &stranger(), "").replace("/ip4/127.0.0.1/tcp/0", "/ip4/192.0.2.1/tcp/4001");
    home.write_config(&config);
    private_dir(home.data_socket().parent().expect("a run dir"));
    std::fs::write(home.data_socket(), b"not a socket").expect("planted");
    let mut daemon = home.start(&[]);
    assert_eq!(daemon.exit().await.code(), Some(1), "{}", daemon.log());
    let log = daemon.log();
    assert!(log.contains("the IPC sockets: "), "{log}");
    assert!(
        !log.contains("the runtime: "),
        "the runtime never ran: {log}"
    );
}

/// Anything but this user's socket in a socket's place is fatal, and
/// left as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_file_in_a_sockets_place_is_fatal_and_left() {
    let home = Home::new("e2e");
    home.write_key();
    home.write_config(&profile("e2e", &stranger(), ""));
    private_dir(home.data_socket().parent().expect("a run dir"));
    std::fs::write(home.data_socket(), b"not a socket").expect("planted");
    let mut daemon = home.start(&[]);
    let status = daemon.exit().await;
    assert_eq!(status.code(), Some(1), "{}", daemon.log());
    assert_eq!(
        std::fs::read(home.data_socket()).expect("still there"),
        b"not a socket"
    );
}

/// A missing key is fatal -- never a silent new `PeerId` -- unless
/// `--create-identity`; the created key is the one the next start loads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_key_is_fatal_unless_asked_to_create_one() {
    let home = Home::new("e2e");
    home.write_config(&profile("e2e", &stranger(), ""));
    let mut refused = home.start(&[]);
    assert_eq!(refused.exit().await.code(), Some(1), "{}", refused.log());
    assert!(
        refused.log().contains("--create-identity"),
        "{}",
        refused.log()
    );
    assert!(!home.paths.identity_file().exists(), "nothing created");

    let mut created = home.start(&["--create-identity"]);
    created.serving(&home).await;
    let first_peer = status_peer(&home).await;
    assert!(created.terminate().await.success());

    let mut again = home.start(&[]);
    again.serving(&home).await;
    let second_peer = status_peer(&home).await;
    assert_eq!(
        first_peer, second_peer,
        "the created key is kept and loaded"
    );
    assert!(again.terminate().await.success());
}

/// The profile's identity, as a status port reads it.
async fn status_peer(home: &Home) -> TransportIdentity {
    let binding = home.binding();
    let port = binding
        .admin([AdminCapability::Status].into())
        .await
        .expect("a port");
    port.status().await.expect("status").peer
}

/// A restart keeps the `PeerId` and gives every lease a fresh epoch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_keeps_the_peer_and_issues_fresh_epochs() {
    let home = Home::new("e2e");
    let peer = home.write_key();
    home.write_config(&profile("e2e", &stranger(), ""));
    let mut epochs = Vec::new();
    for _ in 0..2 {
        let mut daemon = home.start(&[]);
        daemon.serving(&home).await;
        let session = home.binding().open(lease_request()).await.expect("leases");
        epochs.push(
            session
                .session()
                .endpoint_lease()
                .expect("a lease")
                .epoch
                .clone(),
        );
        let status = home
            .binding()
            .admin([AdminCapability::Status].into())
            .await
            .expect("a port")
            .status()
            .await
            .expect("status");
        assert_eq!(status.peer, peer, "the same PeerId");
        drop(session);
        assert!(daemon.terminate().await.success());
    }
    assert_ne!(epochs[0], epochs[1], "a fresh epoch after the restart");
}

/// An `embedded-android` profile runs inside the app, never as a daemon:
/// the shipped Android example is refused before anything is bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_embedded_android_profile_is_refused() {
    let home = Home::new("human-android");
    home.write_key();
    home.write_config(&example("human-android.yaml", &stranger(), "", None));
    let mut daemon = home.start(&[]);
    assert_eq!(daemon.exit().await.code(), Some(1), "{}", daemon.log());
    assert!(
        daemon.log().contains("embedded-android"),
        "{}",
        daemon.log()
    );
    assert!(!home.data_socket().exists(), "nothing bound");
}

/// The data socket grants no admin authority, whatever the client calls
/// itself; the admin socket does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_data_socket_grants_no_admin_authority_to_a_client_calling_itself_transportctl() {
    let home = Home::new("e2e");
    home.write_key();
    home.write_config(&profile("e2e", &stranger(), ""));
    let mut daemon = home.start(&[]);
    daemon.serving(&home).await;
    let hello = r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
        "client":{"kind":"transportctl"},"requested_capabilities":["admin.status"]}"#;
    match first_answer(&home.data_socket(), hello).await {
        Frame::Close(close) => assert_eq!(close.code, TransportError::CapabilityDenied),
        other => panic!("refused on the data socket, got {other:?}"),
    }
    match first_answer(&home.admin_socket(), hello).await {
        Frame::HelloResponse(response) => assert_eq!(response.granted_capabilities.len(), 1),
        other => panic!("granted on the admin socket, got {other:?}"),
    }
    assert!(daemon.terminate().await.success());
}

/// Send `hello` on `socket` and read the first frame back.
async fn first_answer(socket: &Path, hello: &str) -> Frame {
    let mut stream = tokio::net::UnixStream::connect(socket)
        .await
        .expect("connects");
    stream
        .write_all(&encode_frame(hello).expect("a frame"))
        .await
        .expect("sent");
    let mut buf = Vec::new();
    tokio::time::timeout(PATIENCE, async {
        loop {
            match decode_frame(&buf) {
                Ok(DecodedFrame { body, .. }) => {
                    return Frame::parse(&body).expect("a frame");
                }
                Err(FrameError::Incomplete { .. }) => {}
                Err(e) => panic!("a bad frame: {e:?}"),
            }
            let mut chunk = [0_u8; 4096];
            let n = stream.read(&mut chunk).await.expect("reads");
            assert!(n > 0, "closed before answering");
            buf.extend_from_slice(&chunk[..n]);
        }
    })
    .await
    .expect("answered in time")
}

/// `name` from the shipped examples as a daemon runs it: placeholders made
/// concrete (the allowlist's is `other`), the fixed listen port replaced
/// by `listen`, debug logging, and -- when given -- a static entry.
fn example(name: &str, other: &TransportIdentity, listen: &str, route: Option<&str>) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../architecture/config/examples")
        .join(name);
    let mut raw = std::fs::read_to_string(&path)
        .expect("the example is readable")
        .replace("<PEER_A>", other.as_str())
        .replace("/ip4/0.0.0.0/tcp/4001", listen);
    while let Some(start) = raw.find('<') {
        let Some(len) = raw[start..].find('>') else {
            break;
        };
        let token = raw[start..=start + len].to_owned();
        raw = raw.replace(&token, stranger().as_str());
    }
    if let Some(route) = route {
        raw = raw.replacen(
            "discovery:\n  providers:\n",
            &format!(
                "discovery:\n  providers:\n    - {{ type: static-bootstrap, enabled: true, \
                 priority: 5, config: {{ peers: [\"{route}\"] }} }}\n"
            ),
            1,
        );
    }
    raw.push_str("observability: { log_level: debug }\n");
    raw
}

/// A port nothing listens on at the moment of asking.
fn free_port(ip: std::net::Ipv4Addr) -> u16 {
    std::net::TcpListener::bind((ip, 0))
        .expect("a port")
        .local_addr()
        .expect("an address")
        .port()
}

const MARKER: &str = "E2E-PAYLOAD-MARKER-5c1e";

/// Two daemons from the shipped desktop example exchange a direct message
/// and a broadcast over IPC, and the payload -- sent at debug level --
/// appears in neither daemon's log (`payload_logging: false`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_example_profile_daemons_exchange_direct_and_broadcast_over_ipc() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let (a, b) = (Home::new("human-desktop"), Home::new("human-desktop"));
    let (a_peer, b_peer) = (a.write_key(), b.write_key());
    let (a_port, b_port) = (free_port(ip), free_port(ip));
    let at = |port: u16| format!("/ip4/{ip}/tcp/{port}");
    b.write_config(&example("human-desktop.yaml", &a_peer, &at(b_port), None));
    let route = format!("{}/p2p/{}", at(b_port), b_peer.as_str());
    a.write_config(&example(
        "human-desktop.yaml",
        &b_peer,
        &at(a_port),
        Some(&route),
    ));
    let mut b_daemon = b.start(&[]);
    b_daemon.serving(&b).await;
    let mut a_daemon = a.start(&[]);
    a_daemon.serving(&a).await;

    let from = a.binding().open(lease_request()).await.expect("A leases");
    let to = b.binding().open(lease_request()).await.expect("B leases");
    let payload = || {
        Payload::at_ceiling(
            Some(MediaType::parse("text/plain").expect("a media type")),
            MARKER.as_bytes().to_vec(),
        )
        .expect("a payload")
    };

    // Direct: retried until the daemons have found each other.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let sent = from
            .send_direct(
                DirectDestination {
                    peer: b_peer.clone(),
                    endpoint: Some(human()),
                },
                MessageId::from_bytes([7; 16]),
                payload(),
            )
            .await;
        if sent.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no direct route: {sent:?}\nA:\n{}\nB:\n{}",
            a_daemon.log(),
            b_daemon.log()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let got = interweave_local_client_conformance_tests::receive(&to, PATIENCE).await;
    let Some(SessionEvent::Direct(message)) = got.first() else {
        panic!("a direct message: {got:?}");
    };
    assert_eq!(message.payload.bytes(), MARKER.as_bytes());
    assert_eq!(&message.source_peer, &a_peer);

    // Broadcast: published until the mesh has formed.
    let general = ChannelId::parse("general").expect("a channel");
    from.join(general.clone()).await.expect("A joins");
    to.join(general.clone()).await.expect("B joins");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut n = 0_u8;
    let delivered = loop {
        n = n.wrapping_add(1);
        from.broadcast(
            general.clone(),
            BroadcastMessageV1 {
                message_id: MessageId::from_bytes([n; 16]),
                sent_at_ms: 0,
                payload: payload(),
            },
        )
        .await
        .expect("accepted locally");
        let got =
            interweave_local_client_conformance_tests::receive(&to, Duration::from_millis(500))
                .await;
        if let Some(SessionEvent::Broadcast(message)) = got
            .into_iter()
            .find(|e| matches!(e, SessionEvent::Broadcast(_)))
        {
            break message;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no broadcast arrived"
        );
    };
    assert_eq!(delivered.payload.bytes(), MARKER.as_bytes());
    assert_eq!(&delivered.source_peer, &a_peer);

    drop((from, to));
    assert!(a_daemon.terminate().await.success(), "{}", a_daemon.log());
    assert!(b_daemon.terminate().await.success(), "{}", b_daemon.log());
    for (who, log) in [("A", a_daemon.log()), ("B", b_daemon.log())] {
        assert!(
            log.contains("DEBUG"),
            "{who} logged at debug, so the absence below is not vacuous"
        );
        // Every form a log line could carry the bytes in: as text, as hex,
        // as `Payload`'s derived `Debug` (a decimal array), and as the
        // base64url the IPC wire carries.
        for form in marker_forms() {
            assert!(
                !log.contains(&form),
                "{who}'s log carries the payload as {form:?}:\n{log}"
            );
        }
    }
}

/// The marker as text, hex, `Debug` decimals, and base64url -- whole, and
/// the leading run a truncating formatter would print.
fn marker_forms() -> Vec<String> {
    use std::fmt::Write as _;
    let bytes = MARKER.as_bytes();
    let hex = bytes.iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    let decimal = format!("{:?}", &bytes[..8]);
    let decimal = decimal.trim_end_matches(']').to_owned();
    vec![
        MARKER.to_owned(),
        hex[..16].to_owned(),
        decimal,
        base64url(bytes)[..12].to_owned(),
    ]
}

/// Unpadded base64url, as the IPC wire encodes a payload.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}
