// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `HumanChatV2` across two real daemons (plan §17 (6), (7), exit gate (3)
//! and (b)): each side is the human client's own transport facade over
//! the IPC binding, with its own store and its own `ui-model`, so a
//! message goes model -> facade -> IPC -> daemon -> libp2p -> daemon ->
//! IPC -> facade -> store -> model, and a focused view's read reaches
//! the receiver's store. Direct and
//! broadcast, plain and compressed, in both directions; and every
//! payload a daemon handed a client, captured as delivered, validates
//! against `human-chat/envelope.schema.json`.
//!
//! What this does not prove: anything about a network other than one
//! host's private address; a restart (Stage 15's process kill); or the
//! `AcceptedV2` -> unread-commit window, which is carried, not closed.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use interweave_human_chat_protocol::{
    ContentEncoding, HumanChatV2, MessageKind, decode_envelope_bytes, parse_media_type,
};
use interweave_human_store::{HumanStore, PageLimits, StoreOptions};
use interweave_human_transport_client::{
    ClientConfig, ClientEvent, Destination, Origin, OutboundStatus, Received, SessionState,
    TransportClient,
};
use interweave_human_ui_model::{
    ConversationKey, Direction, Intent, ItemStatus, LabelKey, MessageItem, Retention, UiModel,
};
use interweave_ipc_client::{IpcBinding, IpcSession};
use interweave_local_client_api::{
    DataSessionBinding, DataSessionPort, LocalDataSession, SessionEvent, SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointDirectoryV1, EndpointId,
    MAX_PAYLOAD_BYTES, MessageId, Payload, TransportError, TransportIdentity,
};

mod common;

use common::{Daemon, Home, PATIENCE, example, free_port, human, schema_validator};

/// Which way a captured payload arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Direct,
    Broadcast,
}

/// A payload exactly as the daemon handed it to the client, before the
/// facade decodes it.
#[derive(Debug, Clone)]
struct Captured {
    kind: Kind,
    media_type: Option<String>,
    bytes: Vec<u8>,
}

type Tap = Arc<Mutex<Vec<Captured>>>;

/// The IPC binding, recording every payload its sessions hand over. It
/// changes nothing it passes on: the facade sees what the daemon wrote.
struct Recording {
    inner: IpcBinding,
    tap: Tap,
}

impl DataSessionBinding for Recording {
    type Session = RecordingSession;

    fn open(
        &self,
        request: SessionRequest,
    ) -> impl Future<Output = Result<RecordingSession, TransportError>> + Send {
        let tap = Arc::clone(&self.tap);
        let opening = self.inner.open(request);
        async move {
            Ok(RecordingSession {
                inner: opening.await?,
                tap,
            })
        }
    }
}

struct RecordingSession {
    inner: IpcSession,
    tap: Tap,
}

impl DataSessionPort for RecordingSession {
    fn session(&self) -> &LocalDataSession {
        self.inner.session()
    }

    fn join(&self, channel: ChannelId) -> impl Future<Output = Result<(), TransportError>> + Send {
        self.inner.join(channel)
    }

    fn leave(&self, channel: ChannelId) -> impl Future<Output = Result<(), TransportError>> + Send {
        self.inner.leave(channel)
    }

    fn broadcast(
        &self,
        channel: ChannelId,
        message: BroadcastMessageV1,
    ) -> impl Future<Output = Result<(), TransportError>> + Send {
        self.inner.broadcast(channel, message)
    }

    fn send_direct(
        &self,
        destination: DirectDestination,
        message_id: MessageId,
        payload: Payload,
    ) -> impl Future<Output = Result<EndpointId, TransportError>> + Send {
        self.inner.send_direct(destination, message_id, payload)
    }

    async fn events(&self, max: usize) -> Result<Vec<SessionEvent>, TransportError> {
        let events = self.inner.events(max).await?;
        let mut tap = self.tap.lock().expect("the tap");
        for event in &events {
            let (kind, payload) = match event {
                SessionEvent::Direct(m) => (Kind::Direct, &m.payload),
                SessionEvent::Broadcast(m) => (Kind::Broadcast, &m.payload),
                SessionEvent::Local(_) => continue,
            };
            tap.push(Captured {
                kind,
                media_type: payload.media_type().map(|m| m.as_str().to_owned()),
                bytes: payload.bytes().to_vec(),
            });
        }
        drop(tap);
        Ok(events)
    }

    fn query_endpoints(
        &self,
        peer: TransportIdentity,
    ) -> impl Future<Output = Result<EndpointDirectoryV1, TransportError>> + Send {
        self.inner.query_endpoints(peer)
    }

    fn close(self) -> impl Future<Output = Result<(), TransportError>> + Send {
        self.inner.close()
    }
}

fn general() -> ChannelId {
    ChannelId::parse("general").expect("a channel")
}

fn wall_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock")
            .as_millis(),
    )
    .expect("a Unix time in ms")
}

/// One person's client: the facade over its daemon, its store, what it
/// has been handed, and each of its rows' latest status.
struct Side {
    peer: TransportIdentity,
    client: TransportClient<Recording, IpcBinding>,
    tap: Tap,
    received: Vec<Received>,
    outbound: BTreeMap<String, OutboundStatus>,
    /// What this side's views would show, fed as a composition root
    /// feeds it: every event, every receipt, every committed send.
    model: UiModel,
    _store_dir: tempfile::TempDir,
}

impl Side {
    fn new(home: &Home, peer: TransportIdentity, now: u64) -> Self {
        let store_dir = tempfile::tempdir().expect("a store directory");
        let store = HumanStore::open(
            // A directory the store makes owner-only itself.
            &store_dir.path().join("state/human.sqlite"),
            StoreOptions::default(),
        )
        .expect("a store");
        let tap = Tap::default();
        let client = TransportClient::new(
            Recording {
                inner: home.binding(),
                tap: Arc::clone(&tap),
            },
            home.binding(),
            store,
            ClientConfig {
                client_kind: "human-client".to_owned(),
                endpoint: Some(human()),
                channels: vec![general()],
                max_payload_bytes: MAX_PAYLOAD_BYTES,
            },
            Box::new(wall_ms),
            now,
        )
        .expect("an empty store's pending rows");
        Self {
            peer,
            client,
            tap,
            received: Vec::new(),
            outbound: BTreeMap::new(),
            model: UiModel::new(),
            _store_dir: store_dir,
        }
    }

    /// One turn of the caller's loop: tick, drain, read the events.
    async fn step(&mut self, now: u64) {
        // Boxed: the facade's futures are large, and this one is polled
        // from every turn of `pump`.
        Box::pin(self.client.tick(now)).await;
        let drained = Box::pin(self.client.drain(64, now)).await;
        for received in &drained {
            self.model.received(received.clone());
        }
        self.received.extend(drained);
        while let Some(event) = self.client.next_event() {
            if let ClientEvent::Outbound(update) = &event {
                self.outbound.insert(
                    update.app_message_id.as_str().to_owned(),
                    update.status.clone(),
                );
            }
            self.model.client_event(event);
        }
    }

    fn is_ready(&self) -> bool {
        matches!(self.client.session_state(), SessionState::Ready { .. })
    }

    fn got(&self, app_message_id: &str) -> Option<&Received> {
        self.received
            .iter()
            .find(|r| r.envelope.app_message_id == app_message_id)
    }

    fn has_broadcast_from(&self, publisher: &TransportIdentity) -> bool {
        self.received
            .iter()
            .any(|r| matches!(&r.origin, Origin::Channel { publisher: p, .. } if p == publisher))
    }

    async fn send(&mut self, to: Destination, envelope: &HumanChatV2, now: u64) {
        let row = Box::pin(self.client.send(to.clone(), envelope, now))
            .await
            .expect("the facade commits it");
        self.model.sent(row, &to, envelope.clone(), wall_ms());
    }

    /// This side's item for `text` in conversation `key`.
    fn item(&self, key: &ConversationKey, text: &str) -> MessageItem {
        self.model
            .messages(key)
            .into_iter()
            .find(|item| item.source == text)
            .unwrap_or_else(|| panic!("no item for {text:.40} in {key:?}"))
    }
}

/// An envelope with a fresh application id; `compressible` makes its raw
/// form larger than the payload limit, so only the fit fallback
/// (`;ce=br`) can send it.
fn envelope(serial: u32, from: &str, compressible: bool) -> HumanChatV2 {
    let mut text = format!("message {serial} from **{from}**");
    if compressible {
        let line = format!("\n- {from} repeats this line so brotli has something to fold");
        while text.len() <= MAX_PAYLOAD_BYTES + 4096 {
            text.push_str(&line);
        }
    }
    HumanChatV2 {
        v: 2,
        kind: MessageKind::Text,
        app_message_id: format!(
            "{:032x}",
            u128::from(serial) | u128::from(from == "B") << 64
        ),
        text,
        reply_to: None,
        sent_at_ms: Some(wall_ms()),
        from_endpoint: Some(human()),
    }
}

/// Two daemons from the shipped desktop example, each with a static
/// route to the other, both serving.
async fn two_daemons() -> (
    Home,
    Daemon,
    TransportIdentity,
    Home,
    Daemon,
    TransportIdentity,
) {
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
    (a, a_daemon, a_peer, b, b_daemon, b_peer)
}

/// Step both sides until `done` holds, failing with both daemons' logs
/// when `PATIENCE` runs out. `between` runs each turn, for a sender that
/// must keep trying.
async fn pump(
    sides: &mut [Side; 2],
    clock: Instant,
    daemons: &[&Daemon; 2],
    what: &str,
    mut between: impl AsyncFnMut(&mut [Side; 2], u64),
    done: impl Fn(&[Side; 2]) -> bool,
) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let now = u64::try_from(clock.elapsed().as_millis()).expect("ms");
        for side in sides.iter_mut() {
            side.step(now).await;
        }
        if done(sides) {
            return;
        }
        between(sides, now).await;
        assert!(
            Instant::now() < deadline,
            "{what} did not happen in time\nA:\n{}\nB:\n{}",
            daemons[0].log(),
            daemons[1].log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn human_chat_crosses_two_daemons_direct_and_broadcast_plain_and_compressed() {
    let (a_home, mut a_daemon, a_peer, b_home, mut b_daemon, b_peer) = two_daemons().await;
    let clock = Instant::now();
    let mut sides = [
        Side::new(&a_home, a_peer.clone(), 0),
        Side::new(&b_home, b_peer.clone(), 0),
    ];
    let names = ["A", "B"];

    pump(
        &mut sides,
        clock,
        &[&a_daemon, &b_daemon],
        "both sessions ready",
        async |_, _| {},
        |s| s.iter().all(Side::is_ready),
    )
    .await;

    // Direct, both directions, plain and compressed: the facade retries
    // on its own until the daemons have found each other.
    let mut sent: BTreeMap<String, (usize, HumanChatV2, Kind)> = BTreeMap::new();
    let mut serial = 0_u32;
    for (from, to) in [(0, 1), (1, 0)] {
        for compressible in [false, true] {
            serial += 1;
            let message = envelope(serial, names[from], compressible);
            let destination = Destination::Direct {
                peer: sides[to].peer.clone(),
                endpoint: Some(human()),
            };
            sides[from].send(destination, &message, 0).await;
            sent.insert(
                message.app_message_id.clone(),
                (from, message, Kind::Direct),
            );
        }
    }
    let directs: Vec<(usize, String)> = sent
        .iter()
        .map(|(id, (from, _, _))| (*from, id.clone()))
        .collect();
    pump(
        &mut sides,
        clock,
        &[&a_daemon, &b_daemon],
        "every direct message accepted and received",
        async |_, _| {},
        |s| {
            directs.iter().all(|(from, id)| {
                s[1 - from].got(id).is_some()
                    && matches!(
                        s[*from].outbound.get(id),
                        Some(OutboundStatus::Accepted { .. })
                    )
            })
        },
    )
    .await;

    // Broadcast: a plain probe from each side until the mesh carries one
    // each way -- a publish before the mesh formed is accepted locally
    // and reaches nobody -- then one compressed broadcast each way, sent
    // once, which must arrive. A probe that arrived by the time one had
    // crossed each way is held to every assertion a sent message is; one
    // published before the mesh formed may be lost, and one that arrives
    // later is checked only by the store and payload sweeps.
    let probes = std::cell::Cell::new(serial);
    let mut last_probe = [None::<u64>; 2];
    let mut probe_log: Vec<(usize, HumanChatV2)> = Vec::new();
    pump(
        &mut sides,
        clock,
        &[&a_daemon, &b_daemon],
        "a plain broadcast each way",
        async |s, now| {
            for from in 0..2 {
                if s[1 - from].has_broadcast_from(&s[from].peer.clone()) {
                    continue;
                }
                if last_probe[from].is_some_and(|at| now < at + 500) {
                    continue;
                }
                last_probe[from] = Some(now);
                probes.set(probes.get() + 1);
                let message = envelope(probes.get(), names[from], false);
                s[from]
                    .send(Destination::Broadcast(general()), &message, now)
                    .await;
                probe_log.push((from, message));
            }
        },
        |s| s[1].has_broadcast_from(&s[0].peer) && s[0].has_broadcast_from(&s[1].peer),
    )
    .await;
    for (from, message) in probe_log {
        if sides[1 - from].got(&message.app_message_id).is_some() {
            sent.insert(
                message.app_message_id.clone(),
                (from, message, Kind::Broadcast),
            );
        }
    }
    for (from, name) in names.iter().enumerate() {
        assert!(
            sent.values().any(|(f, m, k)| *f == from
                && *k == Kind::Broadcast
                && m.text.len() <= MAX_PAYLOAD_BYTES),
            "a plain broadcast from {name} is held to the assertions below"
        );
    }
    serial = probes.get();
    for from in 0..2 {
        serial += 1;
        let message = envelope(serial, names[from], true);
        let now = u64::try_from(clock.elapsed().as_millis()).expect("ms");
        sides[from]
            .send(Destination::Broadcast(general()), &message, now)
            .await;
        sent.insert(
            message.app_message_id.clone(),
            (from, message, Kind::Broadcast),
        );
    }
    let broadcasts: Vec<(usize, String)> = sent
        .iter()
        .filter(|(_, (_, _, kind))| *kind == Kind::Broadcast)
        .map(|(id, (from, _, _))| (*from, id.clone()))
        .collect();
    pump(
        &mut sides,
        clock,
        &[&a_daemon, &b_daemon],
        "every broadcast held to the assertions received",
        async |_, _| {},
        |s| {
            broadcasts
                .iter()
                .all(|(from, id)| s[1 - from].got(id).is_some())
        },
    )
    .await;

    // What each receiver was handed is what its sender composed, from
    // the authenticated sender, and is unread in its store; every row a
    // sender made is transport-terminal, so no pending copy remains.
    for (id, (from, message, kind)) in &sent {
        let receiver = &sides[1 - from];
        let got = receiver.got(id).expect("received");
        assert_eq!(&got.envelope, message, "{id} arrived as composed");
        match (kind, &got.origin) {
            (Kind::Direct, Origin::Direct { peer, endpoint }) => {
                assert_eq!(peer, &sides[*from].peer);
                assert_eq!(endpoint, &human());
            }
            (Kind::Broadcast, Origin::Channel { channel, publisher }) => {
                assert_eq!(channel, &general());
                assert_eq!(publisher, &sides[*from].peer);
            }
            other => panic!("{id}: {other:?}"),
        }
        let expected_terminal = match kind {
            Kind::Direct => matches!(
                sides[*from].outbound.get(id),
                Some(OutboundStatus::Accepted { endpoint }) if endpoint == &human()
            ),
            Kind::Broadcast => sides[*from].outbound.get(id) == Some(&OutboundStatus::Published),
        };
        assert!(
            expected_terminal,
            "{id}: {:?}",
            sides[*from].outbound.get(id)
        );
    }
    for (side, name) in sides.iter_mut().zip(names) {
        let unread: BTreeSet<_> = side
            .client
            .store_mut()
            .unread_inbound()
            .expect("unread rows")
            .into_iter()
            .map(|row| row.row_id)
            .collect();
        for got in &side.received {
            assert!(
                unread.contains(&got.row),
                "{name}: {:?} not unread",
                got.row
            );
        }
        let pending = side
            .client
            .store_mut()
            .pending_outbound_page(None, PageLimits::default())
            .expect("pending rows");
        assert!(pending.items.is_empty(), "{name}: {:?}", pending.items);
    }

    assert_captured_payloads_validate(&sides, names);
    assert_the_views_show_it(&mut sides, &sent);

    for side in &mut sides {
        side.client.close().await;
    }
    assert!(a_daemon.terminate().await.success(), "{}", a_daemon.log());
    assert!(b_daemon.terminate().await.success(), "{}", b_daemon.log());
}

/// The flip's evidence (exit gate (b)): every payload a daemon handed
/// a client, as it was handed, decodes and validates against the
/// envelope schema -- and the four shapes each side must have seen are
/// all among them, the compressed ones genuinely over the limit raw.
fn assert_captured_payloads_validate(sides: &[Side; 2], names: [&str; 2]) {
    let schema = schema_validator("human-chat/envelope.schema.json");
    let mut control = serde_json::to_value(envelope(0, "A", false)).expect("json");
    assert!(schema.is_valid(&control), "positive control");
    control["v"] = serde_json::json!(3);
    assert!(!schema.is_valid(&control), "the validator refuses a v3");
    for (side, name) in sides.iter().zip(names) {
        let mut shapes = BTreeSet::new();
        for captured in side.tap.lock().expect("the tap").iter() {
            let media_type = captured.media_type.as_deref().expect("a media type");
            let info = parse_media_type(media_type).expect("a HumanChatV2 media type");
            let text = decode_envelope_bytes(&captured.bytes, info.encoding).expect("decodes");
            let value: serde_json::Value = serde_json::from_str(&text).expect("json");
            let errors: Vec<String> = schema.iter_errors(&value).map(|e| e.to_string()).collect();
            assert!(errors.is_empty(), "{name}: {media_type}: {errors:?}");
            HumanChatV2::parse(&text).expect("the envelope parses");
            if info.encoding == ContentEncoding::Brotli {
                assert!(captured.bytes.len() <= MAX_PAYLOAD_BYTES);
                assert!(
                    text.len() > MAX_PAYLOAD_BYTES,
                    "{name}: compressed only to fit"
                );
            }
            shapes.insert((captured.kind, info.encoding == ContentEncoding::Brotli));
        }
        for wanted in [
            (Kind::Direct, false),
            (Kind::Direct, true),
            (Kind::Broadcast, false),
            (Kind::Broadcast, true),
        ] {
            assert!(
                shapes.contains(&wanted),
                "{name} saw no {wanted:?}: {shapes:?}"
            );
        }
    }
}

/// The plan's ui-model leg (§17 (6)): what each side's model shows for
/// what crossed -- the receiver's item unread, authored by the
/// authenticated sender, the sender's labelled by how far the transport
/// took it -- and a focused view's `MarkRead`, applied to the store,
/// leaves no unread copy behind. An unfocused view raises none: the
/// control that the read below came from focus.
fn assert_the_views_show_it(
    sides: &mut [Side; 2],
    sent: &BTreeMap<String, (usize, HumanChatV2, Kind)>,
) {
    for (id, (from, message, kind)) in sent {
        let (sender, receiver) = (&sides[*from], &sides[1 - from]);
        let conversation = |with: &Side| match kind {
            Kind::Direct => ConversationKey::Direct {
                peer: with.peer.clone(),
                endpoint: Some(human()),
            },
            Kind::Broadcast => ConversationKey::Channel(general()),
        };
        let inbound = receiver.item(&conversation(sender), &message.text);
        assert_eq!(inbound.direction, Direction::Inbound, "{id}");
        assert_eq!(
            inbound.status,
            ItemStatus::Inbound(Retention::Unread),
            "{id}"
        );
        assert_eq!(inbound.label, LabelKey::Unread, "{id}");
        assert_eq!(inbound.author.as_ref(), Some(&sender.peer), "{id}");
        let outbound = sender.item(&conversation(receiver), &message.text);
        assert_eq!(outbound.direction, Direction::Outbound, "{id}");
        let label = match kind {
            Kind::Direct => LabelKey::AcceptedByRemoteTransport,
            Kind::Broadcast => LabelKey::PublishedLocally,
        };
        assert_eq!(outbound.label, label, "{id}");
    }

    for side in sides.iter_mut() {
        let conversations: Vec<ConversationKey> = side
            .model
            .conversations()
            .into_iter()
            .map(|c| c.key)
            .collect();
        assert!(!conversations.is_empty());
        for key in &conversations {
            assert!(
                side.model.conversation_viewed(key, false).is_empty(),
                "unfocused, nothing is read"
            );
            for intent in side.model.conversation_viewed(key, true) {
                let Intent::MarkRead(row) = intent else {
                    panic!("viewing raises only MarkRead: {intent:?}");
                };
                side.client
                    .store_mut()
                    .mark_read(row, wall_ms())
                    .expect("marked read");
                side.model.read(row);
            }
        }
        assert!(
            side.client
                .store_mut()
                .unread_inbound()
                .expect("unread rows")
                .is_empty(),
            "read through the view, nothing stays unread"
        );
        for summary in side.model.conversations() {
            assert_eq!(summary.unread, 0, "{:?}", summary.key);
            for item in side.model.messages(&summary.key) {
                if item.direction == Direction::Inbound {
                    assert_eq!(item.label, LabelKey::ReadNotKept, "{:?}", item.key);
                }
            }
        }
    }
}
