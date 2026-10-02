// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `RETENTION.md` §9's ordering cases, the CLIENT's half (plan §17 (1):
//! "retention cases 1 and 5 get their client"). `retention_conformance.rs`
//! proves the store's half -- a commit is durable when the call returns.
//! Here the facade (`crates/human/transport-client`) is driven over the
//! in-memory fake, and the order is OBSERVED from outside the client:
//! - case 1: the transport is called only while the pending copy is on
//!   disk;
//! - case 5: every message `drain` hands over is already an unread row;
//! - case 14: a store that cannot hold unread content takes no lease.
//!
//! The observation reads the store's file through a second connection,
//! as `retention_conformance.rs` does, so a buffered write could not
//! pass for a committed one.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use interweave_human_chat_protocol::{HumanChatV2, MessageKind};
use interweave_human_store::{HumanStore, StoreOptions};
use interweave_human_transport_client::{ClientConfig, Destination, SessionState, TransportClient};
use interweave_local_client_api::{
    DataCapability, DataSessionBinding, DataSessionPort, LocalDataSession, SessionEvent,
    SessionRequest,
};
use interweave_local_client_fake::{FakeConfig, FakeEndpoint, FakeNetwork, FakeNode};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointDirectoryV1, EndpointId, MessageId,
    Payload, TransportError, TransportIdentity,
};

const LIMIT: usize = 49_152;

fn human() -> EndpointId {
    EndpointId::parse("human").expect("valid")
}

fn agent() -> EndpointId {
    EndpointId::parse("agent").expect("valid")
}

fn node() -> FakeConfig {
    FakeConfig {
        peer: ProfileIdentity::generate()
            .transport_identity()
            .expect("peer id"),
        endpoints: vec![
            FakeEndpoint::open(human(), false),
            FakeEndpoint::open(agent(), true),
        ],
        default_endpoint: Some(human()),
        queue_bound: 16,
    }
}

fn config(endpoint: EndpointId) -> ClientConfig {
    ClientConfig {
        client_kind: "human-client".to_owned(),
        endpoint: Some(endpoint),
        channels: vec![],
        max_payload_bytes: LIMIT,
    }
}

fn envelope(id: u8) -> HumanChatV2 {
    HumanChatV2 {
        v: 2,
        kind: MessageKind::Text,
        app_message_id: format!("{id:02x}").repeat(16),
        text: "retained".to_owned(),
        reply_to: None,
        sent_at_ms: None,
        from_endpoint: None,
    }
}

fn store_at(dir: &Path) -> (PathBuf, HumanStore) {
    let path = dir.join("state").join("human.sqlite3");
    let store = HumanStore::open(&path, StoreOptions::default()).expect("store");
    (path, store)
}

/// Rows in `table`, read through a connection the client does not hold.
fn rows(path: &Path, table: &str) -> i64 {
    rusqlite::Connection::open(path)
        .expect("independent reader")
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .expect("count")
}

/// A binding that, at the moment a send reaches it, counts the pending
/// rows in the sender's store file.
#[derive(Clone)]
struct Observing {
    inner: FakeNode,
    store: PathBuf,
    seen: Arc<Mutex<Vec<i64>>>,
}

struct ObservingSession {
    inner: <FakeNode as DataSessionBinding>::Session,
    store: PathBuf,
    seen: Arc<Mutex<Vec<i64>>>,
}

impl DataSessionBinding for Observing {
    type Session = ObservingSession;
    async fn open(&self, request: SessionRequest) -> Result<ObservingSession, TransportError> {
        Ok(ObservingSession {
            inner: self.inner.open(request).await?,
            store: self.store.clone(),
            seen: Arc::clone(&self.seen),
        })
    }
}

impl DataSessionPort for ObservingSession {
    fn session(&self) -> &LocalDataSession {
        self.inner.session()
    }
    async fn join(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.inner.join(channel).await
    }
    async fn leave(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.inner.leave(channel).await
    }
    async fn broadcast(
        &self,
        channel: ChannelId,
        message: BroadcastMessageV1,
    ) -> Result<(), TransportError> {
        self.inner.broadcast(channel, message).await
    }
    async fn send_direct(
        &self,
        destination: DirectDestination,
        message_id: MessageId,
        payload: Payload,
    ) -> Result<EndpointId, TransportError> {
        self.seen
            .lock()
            .expect("lock")
            .push(rows(&self.store, "pending_outbound"));
        self.inner
            .send_direct(destination, message_id, payload)
            .await
    }
    async fn events(&self, max: usize) -> Result<Vec<SessionEvent>, TransportError> {
        self.inner.events(max).await
    }
    async fn query_endpoints(
        &self,
        peer: TransportIdentity,
    ) -> Result<EndpointDirectoryV1, TransportError> {
        self.inner.query_endpoints(peer).await
    }
    async fn close(self) -> Result<(), TransportError> {
        self.inner.close().await
    }
}

async fn holder(node: &FakeNode) -> impl DataSessionPort {
    node.open(
        SessionRequest::new(
            "holder",
            Some(human()),
            [DataCapability::Commands, DataCapability::Events],
        )
        .expect("bounds"),
    )
    .await
    .expect("holds the endpoint")
}

#[tokio::test]
async fn case_1_client_the_transport_is_called_only_while_the_pending_copy_is_on_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, store) = store_at(dir.path());
    let (a, b) = FakeNetwork::pair(node(), node());
    let _held = holder(&b).await;
    let seen = Arc::default();
    let mut client = TransportClient::new(
        Observing {
            inner: a.clone(),
            store: path.clone(),
            seen: Arc::clone(&seen),
        },
        a.clone(),
        store,
        config(agent()),
        0,
    )
    .expect("client");
    client.tick(0).await;
    // The first attempt fails transiently, so a retry is made too: both
    // calls, the first and the retry, must find the copy on disk.
    a.inject_send(TransportError::PeerUnreachable);
    client
        .send(
            Destination::Direct {
                peer: b.peer().clone(),
                endpoint: None,
            },
            &envelope(1),
            0,
        )
        .await
        .expect("committed");
    client.tick(10_000).await;
    assert_eq!(*seen.lock().expect("lock"), [1, 1]);
    // The control: terminal, the copy is gone.
    assert_eq!(rows(&path, "pending_outbound"), 0);
}

#[tokio::test]
async fn case_5_client_every_message_drain_hands_over_is_already_an_unread_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, store) = store_at(dir.path());
    let (a, b) = FakeNetwork::pair(node(), node());
    let mut receiver =
        TransportClient::new(b.clone(), b.clone(), store, config(human()), 0).expect("client");
    receiver.tick(0).await;
    let mut sender = TransportClient::new(
        a.clone(),
        a.clone(),
        HumanStore::open_in_memory(StoreOptions::default()).expect("store"),
        config(agent()),
        0,
    )
    .expect("client");
    sender.tick(0).await;
    for id in 1..=3 {
        sender
            .send(
                Destination::Direct {
                    peer: b.peer().clone(),
                    endpoint: None,
                },
                &envelope(id),
                0,
            )
            .await
            .expect("sent");
    }
    // One at a time, so each hand-over is checked at its own moment.
    for expected in 1..=3 {
        let got = receiver.drain(1, 5).await;
        assert_eq!(got.len(), 1);
        assert_eq!(
            rows(&path, "unread_inbound"),
            expected,
            "message {expected} is on disk when the client receives it"
        );
    }
    assert!(receiver.drain(16, 6).await.is_empty(), "all three, no more");
}

#[tokio::test]
async fn case_14_client_a_store_that_cannot_hold_unread_content_takes_no_lease() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("create"));
    // A quota below the file's size opens degraded (StoreOptions docs):
    // the real SQLITE_FULL path, not an injected one.
    let tight = HumanStore::open(&path, StoreOptions { max_pages: Some(1) }).expect("opens");
    let (_a, b) = FakeNetwork::pair(node(), node());
    let mut receiver =
        TransportClient::new(b.clone(), b.clone(), tight, config(human()), 0).expect("client");
    receiver.tick(0).await;
    assert_eq!(receiver.session_state(), &SessionState::StorageDegraded);
    // The human endpoint is free: nothing is accepted in its name that
    // the store would then lose.
    let _other = holder(&b).await;
}
