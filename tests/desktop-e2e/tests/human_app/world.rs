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
    DirectDestination, EndpointId, MAX_PAYLOAD_BYTES, MediaType, MessageId, TransportIdentity,
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
    let ip = interweave_test_support::net::require_private_interface_v4();
    let (a, b) = (Home::new("human-desktop"), Home::new("human-desktop"));
    let (a_peer, b_peer) = (a.write_key(), b.write_key());
    let (a_port, b_port) = (free_port(ip), free_port(ip));
    let at = |port: u16| format!("/ip4/{ip}/tcp/{port}");
    let route_to =
        |port: u16, peer: &TransportIdentity| format!("{}/p2p/{}", at(port), peer.as_str());
    a.write_config(&example(
        "human-desktop.yaml",
        &b_peer,
        &at(a_port),
        Some(&route_to(b_port, &b_peer)),
    ));
    b.write_config(&example(
        "human-desktop.yaml",
        &a_peer,
        &at(b_port),
        Some(&route_to(a_port, &a_peer)),
    ));
    let mut a_daemon = a.start(&[]);
    a_daemon.serving(&a).await;
    let mut b_daemon = b.start(&[]);
    b_daemon.serving(&b).await;
    World {
        a,
        a_daemon,
        a_peer,
        b,
        b_daemon,
        b_peer,
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
    _store_dir: tempfile::TempDir,
}

impl Peer {
    pub(crate) fn new(home: &Home) -> Self {
        let store_dir = tempfile::tempdir().expect("a store directory");
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
                self.outbound
                    .insert(update.app_message_id.as_str().to_owned(), update.status);
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
                "{what} did not happen\n{}",
                logs()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
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
    home.paths.human_dir().join("human.sqlite")
}

/// Rows of `table` in the app's store, read-only, while the app may be
/// running: a count of rows, never their content.
pub(crate) fn rows(home: &Home, table: &str) -> usize {
    let path = app_store(home);
    if !path.exists() {
        return 0;
    }
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
    let mut store =
        HumanStore::open(&app_store(home), StoreOptions::default()).expect("the app's store");
    let encoded = encode_outbound(envelope, MAX_PAYLOAD_BYTES).expect("it fits");
    let serial = u128::from_str_radix(&envelope.app_message_id, 16).expect("a hex id");
    store
        .commit_pending_outbound(&NewOutbound {
            app_message_id: AppMessageId::parse(envelope.app_message_id.clone()).expect("an id"),
            transport_message_id: MessageId::from_bytes(serial.to_be_bytes()),
            destination: OutboundDestination::Direct(DirectDestination {
                peer: peer.clone(),
                endpoint: Some(endpoint.clone()),
            }),
            media_type: Some(MediaType::parse(encoded.media_type).expect("a media type")),
            payload: encoded.bytes,
            created_at: wall_ms(),
        })
        .expect("committed");
}
