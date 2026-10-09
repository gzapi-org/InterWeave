// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The desktop processes every suite here drives (plan §17 (7), the
//! harness extraction): a daemon's own XDG tree, the daemon and
//! `transportctl` started in it, the shipped examples made concrete, and
//! the schema tree. Lifted out of `daemon.rs` when `human_chat.rs` became
//! its second user, rather than copied -- two copies of `Home` would be
//! two definitions of where a daemon lives.

#![allow(dead_code, clippy::expect_used, clippy::panic)]

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use interweave_ipc_client::{IpcBinding, SocketPaths};
use interweave_local_client_api::{DataCapability, SessionRequest};
use interweave_profile_config::{ProfilePaths, XdgRoots};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{EndpointId, TransportIdentity};

pub(crate) mod relay;

pub(crate) const PATIENCE: Duration = Duration::from_secs(30);

/// The workspace's own `transport-daemon`, beside this test's build.
pub(crate) fn daemon_binary() -> PathBuf {
    workspace_binary("transport-daemon", "interweave-transport-daemon")
}

pub(crate) fn transportctl_binary() -> PathBuf {
    workspace_binary("transportctl", "interweave-transportctl")
}

/// A workspace binary beside this test's own: cargo builds each package's
/// binary for its integration tests, so a workspace test run leaves both
/// there.
pub(crate) fn workspace_binary(name: &str, package: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("this test's path");
    // target/<profile>/deps/<test> -> target/<profile>/<name>
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .expect("a target directory")
        .join(name);
    assert!(
        bin.exists(),
        "{} is missing: build it first (cargo build -p {package})",
        bin.display()
    );
    bin
}

/// A temporary directory made `0700` at creation, whatever the umask:
/// the daemon judges it as an ancestor of the profile, and
/// `tempfile::tempdir()` under umask `002` with a shared primary group is
/// `0775`, refused (j37).
pub(crate) fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .expect("tempdir")
}

pub(crate) fn private_dir(path: &Path) {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .expect("a private directory");
}

/// One daemon's world: its own XDG tree, and the paths the daemon itself
/// resolves in it.
pub(crate) struct Home {
    pub(crate) root: tempfile::TempDir,
    pub(crate) roots: XdgRoots,
    pub(crate) paths: ProfilePaths,
}

impl Home {
    pub(crate) fn new(profile: &str) -> Self {
        let root = private_tempdir();
        let mut home = Self::within(root.path(), profile);
        home.root = root;
        home
    }

    /// A home sharing `base`'s XDG tree -- two profiles of one user.
    pub(crate) fn within(base: &Path, profile: &str) -> Self {
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

    pub(crate) fn write_config(&self, yaml: &str) {
        let file = self.paths.config_file();
        private_dir(file.parent().expect("a config directory"));
        std::fs::write(&file, yaml).expect("the profile written");
        // 0644 whatever the umask: under 002 in a shared group a 0664
        // document is refused (j37).
        std::fs::set_permissions(
            &file,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644),
        )
        .expect("chmod");
    }

    /// An identity key where the profile's default names it.
    pub(crate) fn write_key(&self) -> TransportIdentity {
        let identity = ProfileIdentity::generate();
        let file = self.paths.identity_file();
        private_dir(file.parent().expect("an identity directory"));
        identity.save(&file).expect("the key saved");
        identity.transport_identity().expect("a peer id")
    }

    pub(crate) fn data_socket(&self) -> PathBuf {
        self.paths.data_socket().expect("a data socket path")
    }

    pub(crate) fn admin_socket(&self) -> PathBuf {
        self.paths.admin_socket().expect("an admin socket path")
    }

    pub(crate) fn binding(&self) -> IpcBinding {
        IpcBinding::new(
            SocketPaths {
                data: self.data_socket(),
                admin: self.admin_socket(),
            },
            "e2e-admin",
        )
    }

    /// Run `transportctl` in this home's environment, `input` on stdin.
    pub(crate) fn transportctl(&self, args: &[&str], input: &str) -> std::process::Output {
        use std::io::Write as _;
        let env = |p: &Path| p.as_os_str().to_owned();
        let mut child = Command::new(transportctl_binary())
            .args(["--profile", self.paths.profile()])
            .args(args)
            .env_clear()
            .env("XDG_CONFIG_HOME", env(&self.roots.config_home))
            .env("XDG_DATA_HOME", env(&self.roots.data_home))
            .env("XDG_STATE_HOME", env(&self.roots.state_home))
            .env("XDG_CACHE_HOME", env(&self.roots.cache_home))
            .env(
                "XDG_RUNTIME_DIR",
                env(self.roots.runtime_dir.as_deref().expect("a runtime dir")),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("transportctl runs");
        // A refusal before the read closes stdin unread.
        let _ = child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes());
        child.wait_with_output().expect("transportctl ends")
    }

    /// Start the daemon for this home's profile, its stderr to a file.
    pub(crate) fn start(&self, extra: &[&str]) -> Daemon {
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

pub(crate) struct Daemon {
    pub(crate) child: Child,
    log: PathBuf,
}

impl Daemon {
    pub(crate) fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Until both sockets accept a connection -- a socket FILE is not
    /// enough: a killed daemon leaves its stale ones behind -- or the
    /// daemon exits, which fails the test with its log.
    pub(crate) async fn serving(&mut self, home: &Home) {
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
    pub(crate) async fn exit(&mut self) -> ExitStatus {
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

    pub(crate) async fn terminate(&mut self) -> ExitStatus {
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

pub(crate) fn stranger() -> TransportIdentity {
    ProfileIdentity::generate()
        .transport_identity()
        .expect("a peer id")
}

pub(crate) fn human() -> EndpointId {
    EndpointId::parse("human").expect("endpoint")
}

pub(crate) fn lease_request() -> SessionRequest {
    SessionRequest::new(
        "human-client",
        Some(human()),
        [DataCapability::Events, DataCapability::Commands],
    )
    .expect("a request")
}

/// `name` from the shipped examples as a daemon runs it: placeholders made
/// concrete (the allowlist's is `other`), the fixed listen port replaced
/// by `listen`, debug logging, and -- when given -- a static entry.
pub(crate) fn example(
    name: &str,
    other: &TransportIdentity,
    listen: &str,
    route: Option<&str>,
) -> String {
    concrete(
        &example_text(name).replace("<PEER_A>", other.as_str()),
        listen,
        route,
    )
}

/// The shipped desktop example as a daemon behind `relay` runs it: the
/// relay its one static relay and its one infrastructure peer, no
/// AutoNAT server (the client is `literal[true]` in the schema, so it is
/// left on with nothing to dial), `allow` the data-plane allowlist,
/// listening on `listen` and given `route`, when given, as its one
/// static entry.
///
/// Listen on LOOPBACK to make a circuit the only route a peer learns:
/// ADR-0052's floor refuses a peer-advertised loopback address at the
/// address book's door, and DCUtR's own boundary refuses a loopback
/// candidate before any socket, while the operator's door -- this
/// profile's static relay and static entry -- admits it. So a `route`
/// through the relay is the only path, and a direct `route` (the
/// control) is the operator's own address.
pub(crate) fn relayed_example(
    allow: &[&TransportIdentity],
    listen: &str,
    relay: &relay::Relay,
    route: Option<&str>,
) -> String {
    let mut raw = example_text("human-desktop.yaml");
    let allowed = allow
        .iter()
        .map(|p| format!("\"{}\"", p.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    for (from, to) in [
        (
            r#"infrastructure: { allowed_peers: ["<INFRA_A>", "<INFRA_B>"] }"#.to_owned(),
            format!(r#"infrastructure: {{ allowed_peers: ["{}"] }}"#, relay.peer.as_str()),
        ),
        (
            r#"static_servers: ["/dns4/infra-a.example/tcp/4001/p2p/<INFRA_A>", "/dns4/infra-b.example/tcp/4001/p2p/<INFRA_B>"]"#
                .to_owned(),
            "static_servers: []".to_owned(),
        ),
        (
            r#"static_relays: ["/dns4/infra-a.example/tcp/4001/p2p/<INFRA_A>", "/dns4/infra-b.example/tcp/4001/p2p/<INFRA_B>"]"#
                .to_owned(),
            format!(r#"static_relays: ["{}"]"#, relay.address),
        ),
        (
            r#"allowed_peers: ["<PEER_A>"]"#.to_owned(),
            format!("allowed_peers: [{allowed}]"),
        ),
    ] {
        // A changed example fails here by name, rather than shipping a
        // profile whose relay block silently kept the placeholders.
        assert_eq!(raw.matches(&from).count(), 1, "the example still says {from}");
        raw = raw.replace(&from, &to);
    }
    concrete(&raw, listen, route)
}

fn example_text(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../architecture/config/examples")
        .join(name);
    std::fs::read_to_string(&path).expect("the example is readable")
}

/// An example's text made concrete: the fixed listen port replaced by
/// `listen`, every remaining placeholder a stranger, the static entry
/// `route` when given, and debug logging.
fn concrete(raw: &str, listen: &str, route: Option<&str>) -> String {
    let mut raw = raw.replace("/ip4/0.0.0.0/tcp/4001", listen);
    while let Some(start) = raw.find('<') {
        let Some(len) = raw[start..].find('>') else {
            break;
        };
        let token = raw[start..=start + len].to_owned();
        raw = raw.replace(&token, stranger().as_str());
    }
    if let Some(route) = route {
        assert_eq!(
            raw.matches("discovery:\n  providers:\n").count(),
            1,
            "the example has one providers list for the static entry"
        );
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
pub(crate) fn free_port(ip: std::net::Ipv4Addr) -> u16 {
    std::net::TcpListener::bind((ip, 0))
        .expect("a port")
        .local_addr()
        .expect("an address")
        .port()
}

/// `relative` under `architecture/contracts/schemas`, its `urn:`
/// references resolved against the whole tree.
pub(crate) fn schema_validator(relative: &str) -> jsonschema::Validator {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tests/desktop-e2e sits two below the root")
        .join("architecture/contracts/schemas");
    let mut docs = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a schema directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "json")
                && path.file_name().is_some_and(|n| n != "manifest.json")
            {
                let text = std::fs::read_to_string(&path).expect("read");
                docs.push(serde_json::from_str::<serde_json::Value>(&text).expect("json"));
            }
        }
    }
    let pairs: Vec<(String, jsonschema::Resource)> = docs
        .into_iter()
        .filter_map(|doc| {
            let id = doc.get("$id")?.as_str()?.to_owned();
            Some((id, jsonschema::Resource::from_contents(doc)))
        })
        .collect();
    let registry = jsonschema::Registry::new()
        .extend(pairs)
        .expect("register")
        .prepare()
        .expect("prepare");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(relative)).expect("the schema"))
            .expect("json");
    jsonschema::options()
        .with_registry(&registry)
        .build(&schema)
        .expect("compiles")
}
