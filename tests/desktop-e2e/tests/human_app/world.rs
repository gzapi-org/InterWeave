// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Two daemons, each other's trusted peer, for the cases where the shipped
//! client talks to someone: A runs the app, and B is a person scripted
//! by the test through the human client's own facade. And the app's store
//! file, read-only, for what the client kept.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use interweave_human_chat_protocol::{HumanChatV2, MessageKind, encode_outbound};
use interweave_human_store::{
    AppMessageId, HumanStore, NewOutbound, OutboundDestination, StoreOptions,
};
use interweave_human_transport_client::{
    ClientConfig, ClientEvent, Destination, OutboundStatus, Received, SessionState, TransportClient,
};
use interweave_ipc_client::IpcBinding;
use interweave_transport_api::{
    ChannelId, DirectDestination, EndpointId, MAX_PAYLOAD_BYTES, MediaType, MessageId,
    TransportError, TransportIdentity,
};

use crate::common::{Daemon, Home, PATIENCE, example, free_port, human};

/// The app's side and the scripted side, both serving.
pub(crate) struct World {
    pub(crate) a: Home,
    pub(crate) a_daemon: Daemon,
    pub(crate) a_peer: TransportIdentity,
    pub(crate) b: Home,
    pub(crate) b_daemon: Daemon,
    pub(crate) b_peer: TransportIdentity,
}

/// Two daemons from the shipped desktop example on this host's private
/// address, each with a static route to the other.
pub(crate) async fn two_daemons() -> World {
    two_daemons_joining(None).await
}

/// [`two_daemons`], each profile also joining `channel` -- the shipped
/// example joins none, and the desktop client joins what its profile
/// desires.
pub(crate) async fn two_daemons_joining(channel: Option<&ChannelId>) -> World {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let (a, b) = (Home::new("human-desktop"), Home::new("human-desktop"));
    let (a_peer, b_peer) = (a.write_key(), b.write_key());
    let (a_port, b_port) = (free_port(ip), free_port(ip));
    let at = |port: u16| format!("/ip4/{ip}/tcp/{port}");
    let route_to =
        |port: u16, peer: &TransportIdentity| format!("{}/p2p/{}", at(port), peer.as_str());
    let joining = |config: String| match channel {
        None => config,
        Some(channel) => {
            let joined = format!("channels: {{ desired: [{}] }}", channel.as_str());
            assert!(
                config.contains("channels: { desired: [] }"),
                "the example joins none"
            );
            config.replace("channels: { desired: [] }", &joined)
        }
    };
    a.write_config(&joining(example(
        "human-desktop.yaml",
        &b_peer,
        &at(a_port),
        Some(&route_to(b_port, &b_peer)),
    )));
    b.write_config(&joining(example(
        "human-desktop.yaml",
        &a_peer,
        &at(b_port),
        Some(&route_to(a_port, &a_peer)),
    )));
    // B is started only once A's runtime is up, so B's start-up dial of
    // its static route finds A listening and the connection it makes
    // serves both ways. Without that, either start-up dial can land on a
    // daemon not yet listening and hold that peer off for the retry
    // base, 30 s (p2p-network-dev, 01a10db9-0be3): `serving()` sees the
    // IPC sockets, bound before the runtime starts, while the "serving"
    // line is logged after it.
    let mut a_daemon = a.start(&[]);
    a_daemon.serving(&a).await;
    runtime_up(&mut a_daemon).await;
    let mut b_daemon = b.start(&[]);
    b_daemon.serving(&b).await;
    runtime_up(&mut b_daemon).await;
    World {
        a,
        a_daemon,
        a_peer,
        b,
        b_daemon,
        b_peer,
    }
}

/// Until `daemon` has logged that it serves, which it does once its
/// runtime has started; a daemon that exited instead fails at once.
async fn runtime_up(daemon: &mut Daemon) {
    let deadline = Instant::now() + PATIENCE;
    while !daemon.log().contains("serving") {
        if let Some(status) = daemon.child.try_wait().expect("a status") {
            panic!(
                "the daemon exited ({status}) before its runtime came up:\n{}",
                daemon.log()
            );
        }
        assert!(
            Instant::now() < deadline,
            "the runtime never came up:\n{}",
            daemon.log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

impl World {
    /// Both daemons' logs, for a failure message.
    pub(crate) fn logs(&self) -> String {
        format!("A:\n{}\nB:\n{}", self.a_daemon.log(), self.b_daemon.log())
    }
}

pub(crate) fn wall_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock")
            .as_millis(),
    )
    .expect("a Unix time in ms")
}

/// A one-line text envelope; `serial` makes its application id.
pub(crate) fn envelope(serial: u64, text: &str) -> HumanChatV2 {
    HumanChatV2 {
        v: 2,
        kind: MessageKind::Text,
        app_message_id: format!("{serial:032x}"),
        text: text.to_owned(),
        reply_to: None,
        sent_at_ms: Some(wall_ms()),
        from_endpoint: Some(human()),
    }
}

/// The scripted person on B: the human client's facade over B's daemon,
/// leasing B's `human` endpoint, with a store of its own.
pub(crate) struct Peer {
    client: TransportClient<IpcBinding, IpcBinding>,
    clock: Instant,
    pub(crate) received: Vec<Received>,
    pub(crate) outbound: BTreeMap<String, OutboundStatus>,
    /// Each row's last raw failure code, for a failure message: a status
    /// class can stand for more than one code.
    codes: BTreeMap<String, TransportError>,
    _store_dir: tempfile::TempDir,
}

impl Peer {
    pub(crate) fn new(home: &Home) -> Self {
        // Owner-only at creation, whatever the umask: under umask 002 a
        // bare `tempdir()` is group-writable and the store refuses it as
        // an ancestor (ADR-0028 A 2026-10-08).
        let store_dir = {
            use std::os::unix::fs::PermissionsExt as _;
            tempfile::Builder::new()
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir()
                .expect("a store directory")
        };
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(store_dir.path())
                    .expect("stat")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700,
                "owner-only whatever the umask"
            );
        }
        let store = HumanStore::open(
            &store_dir.path().join("state/human.sqlite"),
            StoreOptions::default(),
        )
        .expect("a store");
        let client = TransportClient::new(
            home.binding(),
            home.binding(),
            store,
            ClientConfig {
                client_kind: "human-client".to_owned(),
                endpoint: Some(human()),
                channels: Vec::new(),
                max_payload_bytes: MAX_PAYLOAD_BYTES,
            },
            Box::new(wall_ms),
            0,
        )
        .expect("an empty store's pending rows");
        Self {
            client,
            clock: Instant::now(),
            received: Vec::new(),
            outbound: BTreeMap::new(),
            codes: BTreeMap::new(),
            _store_dir: store_dir,
        }
    }

    fn now(&self) -> u64 {
        u64::try_from(self.clock.elapsed().as_millis()).expect("ms")
    }

    /// One turn of the facade's loop: tick, drain, read the events.
    pub(crate) async fn step(&mut self) {
        let now = self.now();
        Box::pin(self.client.tick(now)).await;
        let drained = Box::pin(self.client.drain(64, now)).await;
        self.received.extend(drained);
        while let Some(event) = self.client.next_event() {
            if let ClientEvent::Outbound(update) = event {
                let id = update.app_message_id.as_str().to_owned();
                if let Some(code) = update.last_code {
                    self.codes.insert(id.clone(), code);
                }
                self.outbound.insert(id, update.status);
            }
        }
    }

    /// Step until `done` holds, failing with `logs()` when `PATIENCE` runs
    /// out.
    pub(crate) async fn until(
        &mut self,
        what: &str,
        logs: impl Fn() -> String,
        done: impl Fn(&Self) -> bool,
    ) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.step().await;
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{what} did not happen\n{}\n{}",
                self.state(),
                logs()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// B's side of a wait that timed out: its session and each outbound
    /// row's last status (attempts, next retry, the problem's class) and
    /// last raw failure code, by application id -- what the daemons' logs
    /// do not record.
    pub(crate) fn state(&self) -> String {
        format!(
            "B's facade: {:?}, outbound {:?}, last codes {:?}",
            self.client.session_state(),
            self.outbound,
            self.codes
        )
    }

    pub(crate) fn is_ready(&self) -> bool {
        matches!(self.client.session_state(), SessionState::Ready { .. })
    }

    /// Commit a send of `envelope` to `peer`'s `endpoint`.
    pub(crate) async fn send(
        &mut self,
        peer: &TransportIdentity,
        endpoint: Option<EndpointId>,
        envelope: &HumanChatV2,
    ) {
        let now = self.now();
        Box::pin(self.client.send(
            Destination::Direct {
                peer: peer.clone(),
                endpoint,
            },
            envelope,
            now,
        ))
        .await
        .expect("the facade commits it");
    }

    /// Send `envelope` and step B until A's daemon has admitted it
    /// (`Accepted`). A send makes one attempt at once and every retry
    /// waits for B's next step, so a case that then waits on A alone --
    /// its store, its lease, its window -- delivers through here, or a
    /// first attempt refused while the daemons were still connecting is
    /// never retried.
    pub(crate) async fn deliver(
        &mut self,
        peer: &TransportIdentity,
        endpoint: Option<EndpointId>,
        envelope: &HumanChatV2,
        logs: impl Fn() -> String,
    ) {
        self.send(peer, endpoint, envelope).await;
        let id = envelope.app_message_id.clone();
        self.until("B's send admitted at A", logs, |p| p.accepted(&id))
            .await;
    }

    pub(crate) fn accepted(&self, app_message_id: &str) -> bool {
        matches!(
            self.outbound.get(app_message_id),
            Some(OutboundStatus::Accepted { .. })
        )
    }

    pub(crate) fn got(&self, app_message_id: &str) -> bool {
        self.received
            .iter()
            .any(|r| r.envelope.app_message_id == app_message_id)
    }
}

/// The app's store file under `home`'s profile.
pub(crate) fn app_store(home: &Home) -> PathBuf {
    home.paths
        .human_dir()
        .join(interweave_human_store::STORE_FILE)
}

/// Rows of `table` in the app's store, read-only, while the app may be
/// running: a count of rows, never their content.
pub(crate) fn rows(home: &Home, table: &str) -> usize {
    let path = app_store(home);
    // A missing store is a failure, never zero rows: every "now holds
    // none" would pass if the store moved.
    assert!(path.exists(), "the app's store is at {}", path.display());
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("the app's store opens read-only");
    conn.busy_timeout(Duration::from_secs(5))
        .expect("a timeout");
    let count: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .expect("a count");
    usize::try_from(count).expect("a count")
}

/// The application ids of `table`'s rows in the app's store, read-only:
/// the sender's ids, never content.
pub(crate) fn ids(home: &Home, table: &str) -> std::collections::BTreeSet<String> {
    let conn = rusqlite::Connection::open_with_flags(
        app_store(home),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("the app's store opens read-only");
    conn.busy_timeout(Duration::from_secs(5))
        .expect("a timeout");
    let mut stmt = conn
        .prepare(&format!("SELECT app_message_id FROM {table}"))
        .expect("a select");
    stmt.query_map([], |r| r.get::<_, String>(0))
        .expect("rows")
        .map(|r| r.expect("an id"))
        .collect()
}

/// Wait until `table` holds `count` rows.
pub(crate) async fn until_rows(home: &Home, table: &str, count: usize, logs: impl Fn() -> String) {
    let deadline = Instant::now() + PATIENCE;
    while rows(home, table) != count {
        assert!(
            Instant::now() < deadline,
            "{table} never held {count} rows (holds {})\n{}",
            rows(home, table),
            logs()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Commit `envelope` as pending outbound to `peer`'s `endpoint` in the
/// app's store while the app is stopped, as the composer would have: the
/// send half of the shipped binary's proof (plan section 18 (7)).
pub(crate) fn seed_pending(
    home: &Home,
    peer: &TransportIdentity,
    endpoint: &EndpointId,
    envelope: &HumanChatV2,
) {
    seed_pending_to(
        home,
        OutboundDestination::Direct(DirectDestination {
            peer: peer.clone(),
            endpoint: Some(endpoint.clone()),
        }),
        envelope,
    );
}

/// Commit `envelope` as pending outbound to `destination` in the app's
/// store under `home`, encoded as the client encodes it: compressed when
/// it does not fit the payload limit plain.
pub(crate) fn seed_pending_to(
    home: &Home,
    destination: OutboundDestination,
    envelope: &HumanChatV2,
) {
    let mut store =
        HumanStore::open(&app_store(home), StoreOptions::default()).expect("the app's store");
    let encoded = encode_outbound(envelope, MAX_PAYLOAD_BYTES).expect("it fits");
    let serial = u128::from_str_radix(&envelope.app_message_id, 16).expect("a hex id");
    store
        .commit_pending_outbound(&NewOutbound {
            app_message_id: AppMessageId::parse(envelope.app_message_id.clone()).expect("an id"),
            transport_message_id: MessageId::from_bytes(serial.to_be_bytes()),
            destination,
            media_type: Some(MediaType::parse(encoded.media_type).expect("a media type")),
            payload: encoded.bytes,
            created_at: wall_ms(),
        })
        .expect("committed");
}

/// One unread row of the app's store under `home`, as the receiving
/// binary committed it: whose, on which channel if any, and how it was
/// encoded on the wire -- never its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnreadRow {
    pub(crate) app_message_id: String,
    pub(crate) source_peer: String,
    pub(crate) channel_id: Option<String>,
    pub(crate) media_type: Option<String>,
}

/// The app's unread rows under `home`, read-only, while it may run.
pub(crate) fn unread_rows(home: &Home) -> Vec<UnreadRow> {
    let conn = rusqlite::Connection::open_with_flags(
        app_store(home),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("the app's store opens read-only");
    conn.busy_timeout(Duration::from_secs(5))
        .expect("a timeout");
    let mut stmt = conn
        .prepare("SELECT app_message_id, source_peer, channel_id, media_type FROM unread_inbound")
        .expect("a select");
    stmt.query_map([], |r| {
        Ok(UnreadRow {
            app_message_id: r.get(0)?,
            source_peer: r.get(1)?,
            channel_id: r.get(2)?,
            media_type: r.get(3)?,
        })
    })
    .expect("rows")
    .map(|r| r.expect("a row"))
    .collect()
}
