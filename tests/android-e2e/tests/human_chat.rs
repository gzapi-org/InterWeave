// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `HumanChatV2` between the Android side and a desktop daemon (plan §20
//! gate (c)'s human-client clause), on `paths.rs`'s topology: D, a
//! desktop daemon, reserves on the test relay; R, a host stand-in for the
//! Android runtime, reaches D only through the circuit; C, a second
//! stand-in given D's own address, is the direct path and the control in
//! the same run. Each side is the human client's own transport facade --
//! the stand-ins' over the embedded runtime's in-process binding, D's
//! over IPC -- with its own store and `ui-model`, so a message goes
//! facade -> binding -> runtime -> libp2p -> runtime -> binding -> facade
//! -> store -> model. Direct, plain and compressed (`;ce=br`, an envelope
//! over the 48 KiB limit raw), both ways on both paths; every payload a
//! runtime handed a client, captured as handed, validates against
//! `human-chat/envelope.schema.json`; and each side's route indicator,
//! read through its `ui-model`, says the path to the peer now -- the
//! model keeps the latest path notice, so this reads the current path;
//! that a route's BEGIN is told on the path it took is `paths.rs`'s
//! (`ROUTE_ESTABLISHED`, previous none).
//!
//! What makes the circuit R's only route is `paths.rs`'s module note.
//!
//! What this does NOT prove, by name. The device: the Android target
//! build, its lifecycle, its network callbacks and SELinux-confined app
//! data are the stand-in's limits (`src/lib.rs`), so nothing here is
//! evidence the phone does any of it. The relayed run is #245's harness
//! with #245's limits: one host, loopback, a bare-Swarm relay carrying the
//! production relay-server field rather than a daemon acting as the
//! relay, no NAT, and a host whose interface addresses are private -- on
//! one carrying a public address D could learn a direct address for R,
//! and the relayed reading would not hold (`paths.rs`). The rendering:
//! the stand-in has no Slint window, so the route indicator is read at
//! the `ui-model`. Not exercised here, from `human-client-ui.md`: §13's
//! accessibility-tree labels for the route and connectivity controls and
//! the consistent rendering of a `HumanChatV2` fixture on both clients;
//! and §11's screen-reader-friendly controls and keyboard navigation.
//! Broadcast is not crossed here; desktop-e2e's `human_chat.rs` crosses
//! it between daemons.

#![cfg(target_os = "linux")]
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use interweave_android_e2e_tests::{
    Desktop, Device as _, HostStandIn, PATIENCE, Relay, free_port, human, relayed_example_of,
    schema_validator,
};
use interweave_human_chat_protocol::{
    ContentEncoding, HumanChatV2, MessageKind, decode_envelope_bytes, parse_media_type,
};
use interweave_human_store::{HumanStore, StoreOptions};
use interweave_human_transport_client::{
    ClientConfig, ClientEvent, Destination, Origin, OutboundStatus, Received, SessionState,
    TransportClient,
};
use interweave_human_ui_model::{ConversationKey, UiModel};
use interweave_local_client_api::{
    AdminBinding, DataSessionBinding, DataSessionPort, LocalDataSession, SessionEvent,
    SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointDirectoryV1, EndpointId,
    MAX_PAYLOAD_BYTES, MessageId, Payload, PeerPath, TransportError, TransportIdentity,
};

/// Where a stand-in listens: an `embedded-android` profile listens on a
/// wildcard only.
const WILDCARD: &str = "/ip4/0.0.0.0/tcp/0";

/// A payload exactly as a runtime handed it to the client, before the
/// facade decodes it: (media type, bytes).
type Tap = Arc<Mutex<Vec<(Option<String>, Vec<u8>)>>>;

/// A binding recording every direct payload its sessions hand over. It
/// changes nothing it passes on: the facade sees what the runtime wrote.
#[derive(Clone)]
struct Recording<B> {
    inner: B,
    tap: Tap,
}

impl<B: DataSessionBinding> DataSessionBinding for Recording<B> {
    type Session = RecordingSession<B::Session>;

    fn open(
        &self,
        request: SessionRequest,
    ) -> impl Future<Output = Result<Self::Session, TransportError>> + Send {
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

struct RecordingSession<S> {
    inner: S,
    tap: Tap,
}

impl<S: DataSessionPort> DataSessionPort for RecordingSession<S> {
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

    fn events(
        &self,
        max: usize,
    ) -> impl Future<Output = Result<Vec<SessionEvent>, TransportError>> + Send {
        // The inner future and the tap, taken before the await: no borrow
        // of a session that need not be `Sync` is held across it.
        let reading = self.inner.events(max);
        let tap = Arc::clone(&self.tap);
        async move {
            let events = reading.await?;
            let mut tap = tap.lock().expect("the tap");
            for event in &events {
                if let SessionEvent::Direct(m) = event {
                    tap.push((
                        m.payload.media_type().map(|t| t.as_str().to_owned()),
                        m.payload.bytes().to_vec(),
                    ));
                }
            }
            drop(tap);
            Ok(events)
        }
    }

    fn ready(&self) -> impl Future<Output = Result<(), TransportError>> + Send {
        self.inner.ready()
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

fn wall_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock")
            .as_millis(),
    )
    .expect("a Unix time in ms")
}

/// One person's client over binding `B`: the facade, its store, what it
/// has been handed, each of its rows' latest status, and the model its
/// views would show.
struct Side<B: DataSessionBinding + AdminBinding> {
    name: &'static str,
    client: TransportClient<Recording<B>, B>,
    tap: Tap,
    received: Vec<Received>,
    outbound: BTreeMap<String, OutboundStatus>,
    model: UiModel,
    _store_dir: tempfile::TempDir,
}

impl<B: DataSessionBinding + AdminBinding + Clone> Side<B> {
    fn new(name: &'static str, binding: B) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        // Owner-only at creation, whatever the umask (ADR-0028 A 2026-10-08).
        let store_dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("a store directory");
        let store = HumanStore::open(
            &store_dir.path().join("state/human.sqlite"),
            StoreOptions::default(),
        )
        .expect("a store");
        let tap = Tap::default();
        let client = TransportClient::new(
            Recording {
                inner: binding.clone(),
                tap: Arc::clone(&tap),
            },
            binding,
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
            name,
            client,
            tap,
            received: Vec::new(),
            outbound: BTreeMap::new(),
            model: UiModel::new(),
            _store_dir: store_dir,
        }
    }

    /// One turn of the caller's loop: tick, drain, read the events, and
    /// feed the model as a composition root does.
    async fn step(&mut self, now: u64) {
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

    fn accepted(&self, app_message_id: &str) -> bool {
        matches!(
            self.outbound.get(app_message_id),
            Some(OutboundStatus::Accepted { endpoint }) if endpoint == &human()
        )
    }

    async fn send(&mut self, to: &TransportIdentity, envelope: &HumanChatV2) {
        let destination = Destination::Direct {
            peer: to.clone(),
            endpoint: Some(human()),
        };
        let row = Box::pin(self.client.send(destination.clone(), envelope, 0))
            .await
            .expect("the facade commits it");
        self.model
            .sent(row, &destination, envelope.clone(), wall_ms());
    }

    /// The route indicator this side would show on its direct
    /// conversation with `peer`: `None` while no path is known.
    fn path_to(&self, peer: &TransportIdentity) -> Option<PeerPath> {
        self.model
            .conversations()
            .into_iter()
            .find(|c| matches!(&c.key, ConversationKey::Direct { peer: p, .. } if p == peer))
            .and_then(|c| self.model.path(&c.key))
    }
}

/// An envelope with a fresh application id; `compressible` makes its raw
/// form larger than the payload limit, so only the fit fallback
/// (`;ce=br`) can send it.
fn envelope(serial: u8, from: &str, compressible: bool) -> HumanChatV2 {
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
        app_message_id: format!("{serial:032x}"),
        text,
        reply_to: None,
        sent_at_ms: Some(wall_ms()),
        from_endpoint: Some(human()),
    }
}

/// The three sides, stepped together.
struct Sides<D, A>
where
    D: DataSessionBinding + AdminBinding,
    A: DataSessionBinding + AdminBinding,
{
    d: Side<D>,
    r: Side<A>,
    c: Side<A>,
}

impl<D, A> Sides<D, A>
where
    D: DataSessionBinding + AdminBinding + Clone,
    A: DataSessionBinding + AdminBinding + Clone,
{
    /// Step every side until `done` holds, failing with D's log when
    /// `PATIENCE` runs out.
    async fn until(
        &mut self,
        clock: Instant,
        log: impl Fn() -> String,
        what: &str,
        done: impl Fn(&Self) -> bool,
    ) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let now = u64::try_from(clock.elapsed().as_millis()).expect("ms");
            self.d.step(now).await;
            self.r.step(now).await;
            self.c.step(now).await;
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{what} did not happen in time\nD:\n{}",
                log()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn human_chat_crosses_between_android_and_desktop_relayed_and_direct_plain_and_compressed() {
    let (mut d, mut r, mut c) = (Desktop::new(), HostStandIn::new(), HostStandIn::new());
    let (d_peer, r_peer, c_peer) = (d.peer.clone(), r.peer(), c.peer());
    let relay = Relay::start(&[&d_peer, &r_peer, &c_peer]).await;
    let d_listen = format!(
        "/ip4/127.0.0.1/tcp/{}",
        free_port(std::net::Ipv4Addr::LOCALHOST)
    );
    let direct_to_d = format!("{d_listen}/p2p/{}", d_peer.as_str());
    d.start(&relayed_example_of(
        "human-desktop.yaml",
        &[&r_peer, &c_peer],
        &d_listen,
        &relay,
        None,
    ))
    .await;
    // D's reservation first: until it holds, the relay has nowhere to
    // carry R's circuit.
    let deadline = Instant::now() + PATIENCE;
    while !relay.seen().await.reservations.contains(&pid(&d_peer)) {
        assert!(
            Instant::now() < deadline,
            "D never reserved on the relay:\n{}",
            d.log()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    r.start(&relayed_example_of(
        "human-android.yaml",
        &[&d_peer],
        WILDCARD,
        &relay,
        Some(&relay.circuit_to(&d_peer)),
    ));
    c.start(&relayed_example_of(
        "human-android.yaml",
        &[&d_peer],
        WILDCARD,
        &relay,
        Some(&direct_to_d),
    ));

    let clock = Instant::now();
    let mut sides = Sides {
        d: Side::new("D", d.binding()),
        r: Side::new("R", r.binding()),
        c: Side::new("C", c.binding()),
    };
    sides
        .until(
            clock,
            || d.log(),
            "every session ready",
            |s| s.d.is_ready() && s.r.is_ready() && s.c.is_ready(),
        )
        .await;

    // Direct, plain and compressed, both ways on both paths: R <-> D over
    // the circuit, C <-> D over D's address. The facade retries on its own
    // until the route exists.
    let mut sent: Vec<(&str, &str, HumanChatV2)> = Vec::new();
    let mut serial = 0_u8;
    for compressible in [false, true] {
        for (from, to) in [("R", "D"), ("D", "R"), ("C", "D"), ("D", "C")] {
            serial += 1;
            let message = envelope(serial, from, compressible);
            let peer = match to {
                "D" => &d_peer,
                "R" => &r_peer,
                _ => &c_peer,
            };
            match from {
                "D" => sides.d.send(peer, &message).await,
                "R" => sides.r.send(peer, &message).await,
                _ => sides.c.send(peer, &message).await,
            }
            sent.push((from, to, message));
        }
    }
    let crossed = |s: &Sides<_, _>, from: &str, to: &str, id: &str| {
        let got = match to {
            "D" => s.d.got(id).is_some(),
            "R" => s.r.got(id).is_some(),
            _ => s.c.got(id).is_some(),
        };
        let accepted = match from {
            "D" => s.d.accepted(id),
            "R" => s.r.accepted(id),
            _ => s.c.accepted(id),
        };
        got && accepted
    };
    sides
        .until(
            clock,
            || d.log(),
            "every message accepted and received",
            |s| {
                sent.iter()
                    .all(|(from, to, m)| crossed(s, from, to, &m.app_message_id))
            },
        )
        .await;

    // What each receiver was handed is what its sender composed, from the
    // authenticated sender, at the human endpoint.
    for (from, to, message) in &sent {
        let (got, sender) = match (*to, *from) {
            ("D", "R") => (sides.d.got(&message.app_message_id), &r_peer),
            ("D", _) => (sides.d.got(&message.app_message_id), &c_peer),
            ("R", _) => (sides.r.got(&message.app_message_id), &d_peer),
            _ => (sides.c.got(&message.app_message_id), &d_peer),
        };
        let got = got.expect("received");
        assert_eq!(&got.envelope, message, "{from} -> {to} arrived as composed");
        assert!(
            matches!(&got.origin, Origin::Direct { peer, endpoint } if peer == sender && endpoint == &human()),
            "{from} -> {to}: {:?}",
            got.origin
        );
    }

    // Each route indicator, read through the side's model, says the path
    // to the peer now: relayed between R and D, direct between C and D.
    sides
        .until(
            clock,
            || d.log(),
            "every route indicator set",
            |s| {
                s.r.path_to(&d_peer).is_some()
                    && s.d.path_to(&r_peer).is_some()
                    && s.c.path_to(&d_peer).is_some()
                    && s.d.path_to(&c_peer).is_some()
            },
        )
        .await;
    assert_eq!(sides.r.path_to(&d_peer), Some(PeerPath::Relayed), "R of D");
    assert_eq!(sides.d.path_to(&r_peer), Some(PeerPath::Relayed), "D of R");
    assert_eq!(
        sides.c.path_to(&d_peer),
        Some(PeerPath::Direct),
        "C of D, the control"
    );
    assert_eq!(
        sides.d.path_to(&c_peer),
        Some(PeerPath::Direct),
        "D of C, the control"
    );
    let seen = relay.seen().await;
    assert!(
        seen.circuits.contains(&(pid(&r_peer), pid(&d_peer))),
        "the relay carried R's circuit to D: {seen:?}"
    );

    assert_captured_payloads_validate(&sides);

    Box::pin(sides.d.client.close()).await;
    Box::pin(sides.r.client.close()).await;
    Box::pin(sides.c.client.close()).await;
    drop(sides);
    r.stop();
    c.stop();
    d.stop().await;
}

/// Gate (c)'s envelope clause: every payload a runtime handed a client,
/// as it was handed, decodes and validates against the envelope schema,
/// both ways -- and each side saw both shapes, the compressed one
/// genuinely over the limit raw.
fn assert_captured_payloads_validate<D, A>(sides: &Sides<D, A>)
where
    D: DataSessionBinding + AdminBinding,
    A: DataSessionBinding + AdminBinding,
{
    let schema = schema_validator("human-chat/envelope.schema.json");
    let mut control = serde_json::to_value(envelope(0, "R", false)).expect("json");
    assert!(schema.is_valid(&control), "positive control");
    control["v"] = serde_json::json!(3);
    assert!(!schema.is_valid(&control), "the validator refuses a v3");
    for (name, tap) in [
        (sides.d.name, &sides.d.tap),
        (sides.r.name, &sides.r.tap),
        (sides.c.name, &sides.c.tap),
    ] {
        let mut shapes = BTreeSet::new();
        for (media_type, bytes) in tap.lock().expect("the tap").iter() {
            let media_type = media_type.as_deref().expect("a media type");
            let info = parse_media_type(media_type).expect("a HumanChatV2 media type");
            let text = decode_envelope_bytes(bytes, info.encoding).expect("decodes");
            let value: serde_json::Value = serde_json::from_str(&text).expect("json");
            let errors: Vec<String> = schema.iter_errors(&value).map(|e| e.to_string()).collect();
            assert!(errors.is_empty(), "{name}: {media_type}: {errors:?}");
            HumanChatV2::parse(&text).expect("the envelope parses");
            let compressed = info.encoding == ContentEncoding::Brotli;
            if compressed {
                assert!(bytes.len() <= MAX_PAYLOAD_BYTES);
                assert!(
                    text.len() > MAX_PAYLOAD_BYTES,
                    "{name}: compressed only to fit"
                );
            }
            shapes.insert(compressed);
        }
        assert_eq!(
            shapes,
            BTreeSet::from([false, true]),
            "{name} was handed a plain and a compressed envelope"
        );
    }
}

fn pid(peer: &TransportIdentity) -> libp2p::PeerId {
    peer.as_str().parse().expect("a libp2p identity")
}
