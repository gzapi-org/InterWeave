// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The upgrade matrix's "previous build" axis (testing.md §Compatibility
//! fixtures, A 2026-10-08): each production build in
//! `tools/ci/previous-builds.txt`, built from its own commit as a second
//! binary, against HEAD's own binaries.
//!
//! - **The entry set.** The built entries under `INTERWEAVE_PREVIOUS_BUILDS`
//!   are exactly the list's labels, each with both binaries. An entry added
//!   without its build fails here, and so does a stale build.
//! - **HEAD's client against the previous daemon.** HEAD's IPC client
//!   opens a leased data session and an admin port on that daemon's
//!   sockets, negotiating down to the minor the old build speaks.
//! - **The previous client against HEAD's daemon.** The old
//!   `transportctl`, that build's only IPC client, reads `status` and the
//!   endpoint list from HEAD's `transport-daemon` binary, run in the same
//!   XDG tree. Each build resolves the profile's sockets by its own path
//!   rules, so the row also holds the socket layout across builds.
//! - **Peer to peer.** The previous daemon and HEAD's daemon exchange
//!   direct messages and broadcasts in both directions, each side's sessions
//!   opened by HEAD's IPC client on that daemon's sockets.
//!
//! Every IPC await and every child process is bounded by `PATIENCE`. A
//! daemon that accepts a connection and never answers fails the row with
//! that daemon's log; it does not run out the CI job's clock.
//!
//! Every row is `#[ignore]`: a plain `cargo test` has no second binary.
//! CI builds the list and HEAD's `transport-daemon`, then runs this file
//! with `--ignored`. Run that way, `builds()` panics when the variable is
//! unset, and the entry-set row fails when an entry is missing or extra
//! (devex-tooling's interface, 01a11c9e).

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::future::Future;
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use interweave_ipc_client::{IpcBinding, SocketPaths as ClientSockets};
use interweave_local_client_api::{
    AdminBinding as _, AdminCapability, AdminPort as _, DataCapability, DataSessionBinding as _,
    DataSessionPort, SessionEvent, SessionRequest,
};
use interweave_local_client_conformance_tests::{PATIENCE, receive};
use interweave_profile_config::{ProfilePaths, XdgRoots};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointId, MessageId, Payload,
    TransportIdentity,
};

const VARIABLE: &str = "INTERWEAVE_PREVIOUS_BUILDS";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tests/<pkg> is two levels below the root")
        .to_path_buf()
}

/// `tools/ci/previous-builds.txt`: `<label> <sha>` per line. Anything from
/// a `#` to the line's end is a comment, as `build_previous_builds.sh`
/// reads it, and blank lines are skipped.
fn listed() -> Vec<(String, String)> {
    let path = root().join("tools/ci/previous-builds.txt");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let entries: Vec<(String, String)> = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or_default().trim())
        .filter(|l| !l.is_empty())
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

/// The directory CI built into; panics when the variable is unset.
fn builds() -> PathBuf {
    PathBuf::from(
        std::env::var_os(VARIABLE)
            .unwrap_or_else(|| panic!("{VARIABLE} is unset: these rows need the previous builds")),
    )
}

/// HEAD's `transport-daemon`, beside this test's own build:
/// `target/<profile>/deps/<test>` -> `target/<profile>/transport-daemon`.
fn head_daemon() -> PathBuf {
    let exe = std::env::current_exe().expect("this test's path");
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

/// One XDG tree for one profile. Each build resolves its paths in it by
/// its own rules; HEAD's are what this side writes and connects to.
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

    fn write_key(&self, identity: &ProfileIdentity) {
        let file = self.paths().identity_file();
        private_dir(file.parent().expect("an identity directory"));
        identity.save(&file).expect("the key saved");
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

/// A daemon process of either build; killed on drop, its log kept for
/// every failure message.
struct Daemon {
    child: Child,
    log: PathBuf,
    which: String,
}

impl Daemon {
    async fn start(home: &Home, bin: &Path, which: &str, extra: &[&str]) -> Self {
        let log = home.roots.state_home.with_file_name(format!("{which}.log"));
        let child = home
            .command(bin)
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).expect("a log"))
            .spawn()
            .unwrap_or_else(|e| panic!("{which}: {}: {e}", bin.display()));
        let mut daemon = Self {
            child,
            log,
            which: which.to_owned(),
        };
        // Serving once its admin socket answers a status: a socket file is
        // not enough, a killed daemon leaves one behind. Each attempt is
        // bounded, so a daemon that accepts and never answers fails here.
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let attempt = tokio::time::timeout(Duration::from_secs(2), async {
                let admin = home
                    .client()
                    .admin([AdminCapability::Status].into())
                    .await
                    .ok()?;
                admin.status().await.ok()
            });
            if let Ok(Some(_)) = attempt.await {
                return daemon;
            }
            if let Ok(Some(status)) = daemon.child.try_wait() {
                panic!("{which} exited ({status}):\n{}", daemon.log());
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{which} never served:\n{}",
                daemon.log()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// `fut`, or a panic naming `what` and carrying this daemon's log.
    /// Boxed: the IPC client's futures are large, and a caller holding
    /// several on its stack would be larger still.
    fn within<'a, T: 'a>(
        &'a self,
        what: &'a str,
        fut: impl Future<Output = T> + 'a,
    ) -> Pin<Box<dyn Future<Output = T> + 'a>> {
        Box::pin(async move {
            tokio::time::timeout(PATIENCE, fut)
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "{}: {what} took more than {PATIENCE:?}:\n{}",
                        self.which,
                        self.log()
                    )
                })
        })
    }

    /// The daemon's own `PeerId`, from its admin status.
    fn peer<'a>(&'a self, home: &'a Home) -> Pin<Box<dyn Future<Output = TransportIdentity> + 'a>> {
        self.within("the admin status", async move {
            home.client()
                .admin([AdminCapability::Status].into())
                .await
                .expect("an admin port")
                .status()
                .await
                .expect("a status")
                .peer
        })
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run `command` to completion within `PATIENCE`, killing it if it hangs.
fn run_bounded(mut command: Command, what: &str) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{what}: {e}"));
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        if child.try_wait().expect("a status").is_some() {
            return child.wait_with_output().expect("its output");
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let out = child.wait_with_output().expect("its output");
            panic!(
                "{what} took more than {PATIENCE:?}:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A profile both builds read: only keys the Stage 13 build already had,
/// which its `deny_unknown_fields` holds this to. `profile.name` must equal
/// `--profile`.
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

/// Row: HEAD's IPC client on each previous daemon's sockets — a leased
/// data session and the admin status, at the minor the old build speaks.
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
        let old = Daemon::start(&home, &entry.daemon, &entry.label, &["--create-identity"]).await;

        let peer = old.peer(&home).await;
        assert!(
            peer.as_str().starts_with("12D3KooW"),
            "{}: {peer:?}",
            entry.label
        );

        let session = old
            .within("a leased session", home.client().open(human_session()))
            .await
            .unwrap_or_else(|e| panic!("{}: lease: {e:?}\n{}", entry.label, old.log()));
        old.within(
            "a join",
            session.join(ChannelId::parse("interop").expect("valid")),
        )
        .await
        .unwrap_or_else(|e| panic!("{}: join: {e:?}", entry.label));
        old.within("a close", session.close())
            .await
            .expect("closes");
    }
}

/// Row: each previous `transportctl` against HEAD's daemon binary, in one
/// XDG tree — `status` names HEAD's `PeerId`, `endpoints list` names the
/// configured endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs INTERWEAVE_PREVIOUS_BUILDS"]
async fn each_previous_transportctl_is_served_by_heads_daemon() {
    let head_bin = head_daemon();
    for entry in entries() {
        let home = Home::new("head");
        let identity = ProfileIdentity::generate();
        let peer = identity.transport_identity().expect("peer");
        home.write_key(&identity);
        home.write_config(&profile_yaml(
            "head",
            &stranger(),
            "/ip4/127.0.0.1/tcp/0",
            &[],
        ));
        let head = Daemon::start(&home, &head_bin, "HEAD's daemon", &[]).await;

        for args in [&["status"][..], &["endpoints", "list"][..]] {
            let mut command = home.command(&entry.transportctl);
            command.args(args);
            let out = run_bounded(command, &format!("{}: transportctl {args:?}", entry.label));
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success(),
                "{}: transportctl {args:?}: {}\n{stdout}\n{}\n{}",
                entry.label,
                out.status,
                String::from_utf8_lossy(&out.stderr),
                head.log()
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
    }
}

/// A port free on `ip` now, so an address can be written into the other
/// side's profile before its daemon starts. Picked, released and bound
/// later: another process can take it in between, which fails the row
/// loudly (the daemon refuses to listen), never silently.
fn free_port(ip: Ipv4Addr) -> u16 {
    TcpListener::bind((ip, 0))
        .expect("binds")
        .local_addr()
        .expect("an address")
        .port()
}

/// Row: each previous daemon and HEAD's daemon, peer to peer — direct both
/// ways and broadcast both ways.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs INTERWEAVE_PREVIOUS_BUILDS"]
async fn each_previous_daemon_exchanges_with_head_both_ways() {
    let head_bin = head_daemon();
    let ip = interweave_test_support::net::require_private_interface_v4();
    for entry in entries() {
        // HEAD's key and address first, so the old profile can name them;
        // the old daemon creates its own key, read back from its status,
        // and HEAD's profile names it. Each side holds a static route to
        // the other, so whichever starts second connects.
        let head_identity = ProfileIdentity::generate();
        let head_peer = head_identity.transport_identity().expect("peer");
        let (head_port, old_port) = loop {
            let (a, b) = (free_port(ip), free_port(ip));
            if a != b {
                break (a, b);
            }
        };
        let head_listen = format!("/ip4/{ip}/tcp/{head_port}");
        let old_listen = format!("/ip4/{ip}/tcp/{old_port}");

        let old_home = Home::new("old");
        old_home.write_config(&profile_yaml(
            "old",
            head_peer.as_str(),
            &old_listen,
            &[format!("{head_listen}/p2p/{}", head_peer.as_str())],
        ));
        let old = Daemon::start(
            &old_home,
            &entry.daemon,
            &entry.label,
            &["--create-identity"],
        )
        .await;
        let old_peer = old.peer(&old_home).await;

        let head_home = Home::new("head");
        head_home.write_key(&head_identity);
        head_home.write_config(&profile_yaml(
            "head",
            old_peer.as_str(),
            &head_listen,
            &[format!("{old_listen}/p2p/{}", old_peer.as_str())],
        ));
        let head = Daemon::start(&head_home, &head_bin, "HEAD's daemon", &[]).await;

        let at_head = head
            .within("a leased session", head_home.client().open(human_session()))
            .await
            .expect("leases");
        let at_old = old
            .within("a leased session", old_home.client().open(human_session()))
            .await
            .expect("leases");
        let human = EndpointId::parse("human").expect("valid");
        let payload = |s: &str| Payload::at_ceiling(None, s.as_bytes().to_vec()).expect("fits");
        let logs = || format!("{}\n--- HEAD ---\n{}", old.log(), head.log());

        // Direct, HEAD to old: retried until the two daemons are connected,
        // on whichever static route connects first.
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let sent = head
                .within(
                    "HEAD's direct send",
                    at_head.send_direct(
                        DirectDestination {
                            peer: old_peer.clone(),
                            endpoint: Some(human.clone()),
                        },
                        MessageId::from_bytes([1; 16]),
                        payload("head to old"),
                    ),
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
                    logs()
                ),
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let got = old.within("a receive", receive(&at_old, PATIENCE)).await;
        assert!(
            got.iter().any(
                |e| matches!(e, SessionEvent::Direct(m) if m.payload.bytes() == b"head to old")
            ),
            "{}: the old daemon's session got: {got:?}",
            entry.label
        );

        // Direct, old to HEAD.
        old.within(
            "the old daemon's direct send",
            at_old.send_direct(
                DirectDestination {
                    peer: head_peer.clone(),
                    endpoint: Some(human.clone()),
                },
                MessageId::from_bytes([2; 16]),
                payload("old to head"),
            ),
        )
        .await
        .unwrap_or_else(|e| panic!("{}: the old daemon's send: {e:?}\n{}", entry.label, logs()));
        let got = head.within("a receive", receive(&at_head, PATIENCE)).await;
        assert!(
            got.iter().any(
                |e| matches!(e, SessionEvent::Direct(m) if m.payload.bytes() == b"old to head")
            ),
            "{}: HEAD's session got: {got:?}",
            entry.label
        );

        // Broadcast both ways, published until it arrives.
        let channel = ChannelId::parse("interop").expect("valid");
        head.within("a join", at_head.join(channel.clone()))
            .await
            .expect("joins");
        old.within("a join", at_old.join(channel.clone()))
            .await
            .expect("joins");
        let ctx = format!("{}\n{}", entry.label, logs());
        broadcast_until_received(&at_head, &at_old, &channel, "head broadcast", &ctx).await;
        broadcast_until_received(&at_old, &at_head, &channel, "old broadcast", &ctx).await;

        head.within("a close", at_head.close())
            .await
            .expect("closes");
        old.within("a close", at_old.close()).await.expect("closes");
    }
}

/// Publish on `channel` from `from` until `to` receives it: the mesh forms
/// on its own schedule, and each republish is a new message. Every await
/// is bounded, and so is the whole.
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
        let message = BroadcastMessageV1 {
            message_id: MessageId::from_bytes([n; 16]),
            sent_at_ms: 0,
            payload: Payload::at_ceiling(None, what.as_bytes().to_vec()).expect("fits"),
        };
        let _ = tokio::time::timeout(PATIENCE, from.broadcast(channel.clone(), message)).await;
        let got = tokio::time::timeout(PATIENCE, receive(to, Duration::from_millis(500)))
            .await
            .unwrap_or_default();
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
