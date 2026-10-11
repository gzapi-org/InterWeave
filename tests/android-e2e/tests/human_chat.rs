// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `HumanChatV2` between the Android side and a desktop daemon (plan §20
//! gate (c)'s human-client clause), on `paths.rs`'s topology: D, a
//! desktop daemon, reserves on the test relay; R reaches D only through
//! the circuit; C, given D's own address, is the direct path and the
//! control in the same run. R's and C's halves are
//! `interweave-android-e2e-cases`' `human_chat` case, run on the app's own
//! client -- the facade, store and hub the app's service composed, driven
//! as its window drives them, a draft typed and Send pressed -- in the
//! Android side's own process; D's half is here, the desktop client's
//! facade over IPC with its own store and `ui-model`. So a message goes
//! model -> facade -> binding -> runtime -> libp2p -> runtime -> binding
//! -> facade -> store -> model. Direct, plain and compressed (`;ce=br`,
//! an envelope over the 48 KiB limit raw), both ways on both paths; every
//! payload a runtime handed a client, captured as handed, validates
//! against `human-chat/envelope.schema.json`; each Android side's route
//! indicator names its path to D when its case ends, and D's names R
//! relayed and C direct -- the model keeps the latest path notice, so
//! this reads the current path; that a route's BEGIN is told on the path
//! it took is `paths.rs`'s (`ROUTE_ESTABLISHED`, previous none).
//!
//! Two runs, as `paths.rs` has them. On the host, R and C are stand-ins
//! for the app's process: the app's own `ServiceHost` (android-platform)
//! over the embedded runtime, started as the app's service starts it. On
//! a device (`#[ignore]`d, run with `--ignored` and the device's
//! environment, `src/adb.rs`) the phone is R in one run and C in the
//! other, a stand-in the other side.
//!
//! What makes the circuit R's only route is `paths.rs`'s module note.
//!
//! What this does NOT prove, by name. On the stand-in, the device: the
//! Android target build, its lifecycle, its network callbacks and
//! SELinux-confined app data are the stand-in's limits (`src/lib.rs`),
//! so a stand-in run is no evidence the phone does any of it. On a
//! device, a radio network (adb's reversed ports, `src/adb.rs`). The
//! relayed run is #245's harness with #245's limits: one host, loopback,
//! a bare-Swarm relay carrying the production relay-server field rather
//! than a daemon acting as the relay, no NAT, and a host whose interface
//! addresses are private -- on one carrying a public address D could
//! learn a direct address for R, and the relayed reading would not hold
//! (`paths.rs`). The rendering: the case drives the model side with a
//! scripted surface, not the Slint window, so the route indicator is read
//! at the `ui-model`. Not exercised here, from `human-client-ui.md`: §13's
//! accessibility-tree labels for the route and connectivity controls and
//! the consistent rendering of a `HumanChatV2` fixture on both clients;
//! and §11's screen-reader-friendly controls and keyboard navigation.
//! Broadcast is not crossed here; desktop-e2e's `human_chat.rs` crosses
//! it between daemons.

#![cfg(target_os = "linux")]
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use interweave_android_e2e_cases::{
    CaseCtx, Handed, HumanChatArgs, Recording, Tap, cases, chat_text, desktop_message_id, keys,
    parse_payloads,
};
use interweave_android_e2e_tests::adb::AdbDevice;
use interweave_android_e2e_tests::{
    CaseRun, Desktop, Device, PATIENCE, PROFILE, Relay, free_port, human, relayed_example_of,
    schema_validator, try_provision,
};
use interweave_human_android_platform::e2e::HubClient;
use interweave_human_android_platform::{ServiceHost, ServiceLaunch};
use interweave_human_chat_protocol::{
    ContentEncoding, HumanChatV2, MessageKind, decode_envelope_bytes, parse_media_type,
};
use interweave_human_store::{HumanStore, StoreOptions};
use interweave_human_transport_client::{
    ClientConfig, ClientEvent, Destination, Origin, OutboundStatus, Received, SessionState,
    TransportClient,
};
use interweave_human_ui_model::{ConversationKey, UiModel};
use interweave_local_client_api::{AdminBinding, DataSessionBinding};
use interweave_profile_identity::{ProfileIdentity, RecoveryPhrase};
use interweave_transport_api::{MAX_PAYLOAD_BYTES, PeerPath, TransportIdentity};
use serde_json::Value;

/// Where an Android side listens: an `embedded-android` profile listens
/// on a wildcard only.
const WILDCARD: &str = "/ip4/0.0.0.0/tcp/0";

/// How long a stop lets exchanges in flight settle, as the app's own.
const GRACE: Duration = Duration::from_secs(1);

/// A stand-in for the app's process as a `NEED_THE_CLIENT` case needs it:
/// the app's own service host -- runtime, store, facade and hub -- under
/// an app data directory of its own, with the payload recorder the
/// instrumentation starts it with too.
struct AppStandIn {
    // Held for its lifetime: the app data directory lives under it.
    _scratch: tempfile::TempDir,
    app_data_dir: PathBuf,
    peer: TransportIdentity,
    // Each start consumes an identity: a restart rebuilds it from its
    // phrase, so the PeerId is the same by construction.
    phrase: RecoveryPhrase,
    service: ServiceHost,
    tap: Tap,
}

impl AppStandIn {
    fn new() -> Self {
        let scratch = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("a scratch root");
        let app_data_dir = scratch.path().join("app");
        std::fs::create_dir(&app_data_dir).expect("the app data directory");
        std::fs::set_permissions(&app_data_dir, std::fs::Permissions::from_mode(0o700))
            .expect("owner-only");
        let identity = ProfileIdentity::generate();
        Self {
            _scratch: scratch,
            app_data_dir,
            peer: identity.transport_identity().expect("a valid identity"),
            phrase: identity.recovery_phrase().expect("a phrase"),
            service: ServiceHost::new(),
            tap: Tap::default(),
        }
    }
}

/// `f` on a thread of its own: the service's start and stop block on the
/// runtime's executor, which may not be done on a thread that drives a
/// runtime -- and this test's async body is one.
fn off_runtime<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| scope.spawn(f).join().expect("the host thread"))
}

impl Device for AppStandIn {
    fn peer(&self) -> TransportIdentity {
        self.peer.clone()
    }

    fn start(&mut self, config: &str) {
        try_provision(&self.app_data_dir, config).expect("provisioned");
        self.restart();
    }

    fn stop(&mut self) {
        off_runtime(|| self.service.stop(GRACE));
    }

    fn kill(&mut self) {
        off_runtime(|| self.service.stop(Duration::ZERO));
    }

    fn restart(&mut self) {
        let launch = ServiceLaunch {
            app_data_dir: self.app_data_dir.clone(),
            profile: PROFILE.to_owned(),
            identity: ProfileIdentity::from_phrase(&self.phrase).expect("the same identity"),
        };
        let tap = Arc::clone(&self.tap);
        off_runtime(|| self.service.start_recording(launch, tap)).expect("the service starts");
    }

    /// The case on a thread of its own, as the instrumentation runs it,
    /// with the hub as its client.
    fn run_case(&self, case: &str, args: &Value) -> CaseRun {
        let ctx = CaseCtx::<interweave_transport_composition::InProcessBinding> {
            peer: self.peer.clone(),
            app_data_dir: self.app_data_dir.clone(),
            binding: None,
            provision: None,
            client: Some(Box::new(HubClient::attach(
                self.service.hub(),
                Arc::clone(&self.tap),
            ))),
        };
        let (name, args) = (case.to_owned(), args.to_string());
        CaseRun::on_thread(case, move || {
            interweave_android_e2e_cases::run(&name, &args, ctx)
        })
    }
}

impl Drop for AppStandIn {
    fn drop(&mut self) {
        self.kill();
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

/// The desktop person's client over binding `B`: the facade, its store,
/// what it has been handed, each of its rows' latest status, and the
/// model its views would show.
struct Side<B: DataSessionBinding + AdminBinding> {
    client: TransportClient<Recording<B>, B>,
    tap: Tap,
    received: Vec<Received>,
    outbound: BTreeMap<String, OutboundStatus>,
    model: UiModel,
    _store_dir: tempfile::TempDir,
}

impl<B: DataSessionBinding + AdminBinding + Clone> Side<B> {
    fn new(binding: B) -> Self {
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
            Recording::new(binding.clone(), Arc::clone(&tap)),
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

    /// What `from` sent with `text`, if it arrived.
    fn got_text(&self, from: &TransportIdentity, text: &str) -> Option<&Received> {
        self.received.iter().find(|r| {
            r.envelope.text == text
                && matches!(&r.origin, Origin::Direct { peer, endpoint } if peer == from && endpoint == &human())
        })
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

    /// Step until `done` holds, failing with `log` when `PATIENCE` runs
    /// out.
    async fn until(
        &mut self,
        clock: Instant,
        log: impl Fn() -> String,
        what: &str,
        done: impl Fn(&Self) -> bool,
    ) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.step(u64::try_from(clock.elapsed().as_millis()).expect("ms"))
                .await;
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{what} did not happen in time\n{}",
                log()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// D's answer `serial`, with the id the Android side looks for.
fn desktop_envelope(serial: u8, compressible: bool) -> HumanChatV2 {
    HumanChatV2 {
        v: 2,
        kind: MessageKind::Text,
        app_message_id: desktop_message_id(serial),
        text: chat_text(serial, "D", compressible),
        reply_to: None,
        sent_at_ms: Some(wall_ms()),
        from_endpoint: Some(human()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn human_chat_crosses_between_android_and_desktop_relayed_and_direct_plain_and_compressed() {
    relayed_and_direct(AppStandIn::new(), AppStandIn::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a device with the app's androidTest build: ANDROID_SERIAL and INTERWEAVE_ANDROID_INSTRUMENTATION (src/adb.rs); one phone, so --test-threads 1"]
async fn on_a_device_the_phone_chats_over_the_relayed_path_with_a_stand_in_as_the_control() {
    relayed_and_direct(AdbDevice::connect(), AppStandIn::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a device with the app's androidTest build: ANDROID_SERIAL and INTERWEAVE_ANDROID_INSTRUMENTATION (src/adb.rs); one phone, so --test-threads 1"]
async fn on_a_device_the_phone_chats_over_the_direct_path_beside_a_relayed_stand_in() {
    relayed_and_direct(AppStandIn::new(), AdbDevice::connect()).await;
}

/// The case over any two [`Device`]s, R and C unstarted with their
/// `PeerId`s known. The exchanges run one Android side at a time.
async fn relayed_and_direct<R: Device, C: Device>(mut r: R, mut c: C) {
    let mut d = Desktop::new();
    let (d_peer, r_peer, c_peer) = (d.peer.clone(), r.peer(), c.peer());
    let relay = Relay::start(&[&d_peer, &r_peer, &c_peer]).await;
    let d_port = free_port(std::net::Ipv4Addr::LOCALHOST);
    let d_listen = format!("/ip4/127.0.0.1/tcp/{d_port}");
    let direct_to_d = format!("{d_listen}/p2p/{}", d_peer.as_str());
    d.start(&relayed_example_of(
        "human-desktop.yaml",
        &[&r_peer, &c_peer],
        &d_listen,
        &relay,
        None,
    ))
    .await;
    // What each Android side dials is this host's loopback: the relay for
    // both, D's own address for C.
    let relay_port = tcp_port(&relay.address);
    r.reach(relay_port);
    c.reach(relay_port);
    c.reach(d_port);
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
    let mut side = Side::new(d.binding());
    side.until(clock, || d.log(), "D's session ready", Side::is_ready)
        .await;

    // R over the circuit, then C over D's address: each sends D a plain
    // and a compressible message from its window, and D answers each
    // with one of either.
    let mut handed: Vec<(&str, Vec<Handed>)> = Vec::new();
    for (name, device, peer, path, sends, answers) in [
        (
            "R",
            &r as &dyn Device,
            &r_peer,
            PeerPath::Relayed,
            [1, 5],
            [2, 6],
        ),
        (
            "C",
            &c as &dyn Device,
            &c_peer,
            PeerPath::Direct,
            [3, 7],
            [4, 8],
        ),
    ] {
        let log = || format!("D:\n{}\n{name}:\n{}", d.log(), device.log());
        let run = device.run_case(
            cases::HUMAN_CHAT,
            &HumanChatArgs {
                desktop: d_peer.clone(),
                path,
                side: name.to_owned(),
                sends,
                answers,
                deadline: PATIENCE,
            }
            .to_json(),
        );
        let texts = [
            chat_text(sends[0], name, false),
            chat_text(sends[1], name, true),
        ];
        // A case that ends first has failed: its own detail says why.
        side.until(clock, &log, &format!("D receiving {name}'s two"), |s| {
            texts.iter().all(|t| s.got_text(peer, t).is_some()) || run.is_finished()
        })
        .await;
        let run = if texts.iter().all(|t| side.got_text(peer, t).is_some()) {
            run
        } else {
            let out = run.passed(log);
            panic!("{name}'s case passed without D receiving its two: {out:?}");
        };
        let replies = [
            desktop_envelope(answers[0], false),
            desktop_envelope(answers[1], true),
        ];
        for reply in &replies {
            side.send(peer, reply).await;
        }
        // D keeps its half going until the Android side has seen what it
        // waits for and answered.
        side.until(clock, &log, &format!("{name}'s case answering"), |s| {
            replies.iter().all(|m| s.accepted(&m.app_message_id)) && run.is_finished()
        })
        .await;
        let out = run.passed(log);

        // What D was handed is what the Android side's facade composed.
        let sent: Vec<HumanChatV2> = out
            .get(keys::SENT)
            .and_then(Value::as_array)
            .expect("sent")
            .iter()
            .map(|v| HumanChatV2::parse(&v.to_string()).expect("an envelope"))
            .collect();
        assert_eq!(sent.len(), 2, "{name} sent two: {sent:?}");
        for envelope in &sent {
            let got = side
                .got_text(peer, &envelope.text)
                .unwrap_or_else(|| panic!("D never got {name}'s {}", envelope.app_message_id));
            assert_eq!(&got.envelope, envelope, "{name} -> D arrived as composed");
        }
        let payloads = out
            .get(keys::PAYLOADS)
            .and_then(Value::as_str)
            .expect("a payload file");
        let text = String::from_utf8(device.pull(payloads)).expect("the payload file is text");
        handed.push((name, parse_payloads(&text).expect("the payload file")));
    }

    // D's route indicator: relayed to R, direct to C.
    side.until(
        clock,
        || d.log(),
        "D's route indicators set",
        |s| s.path_to(&r_peer).is_some() && s.path_to(&c_peer).is_some(),
    )
    .await;
    assert_eq!(side.path_to(&r_peer), Some(PeerPath::Relayed), "D of R");
    assert_eq!(
        side.path_to(&c_peer),
        Some(PeerPath::Direct),
        "D of C, the control"
    );
    let seen = relay.seen().await;
    assert!(
        seen.circuits.contains(&(pid(&r_peer), pid(&d_peer))),
        "the relay carried R's circuit to D: {seen:?}"
    );

    // Each Android side was handed D's two, and D the four from them.
    let d_handed = side.tap.lock().expect("the tap").clone();
    assert_eq!(d_handed.len(), 4, "D was handed R's two and C's two");
    handed.push(("D", d_handed));
    assert_captured_payloads_validate(&handed);

    Box::pin(side.client.close()).await;
    drop(side);
    r.stop();
    c.stop();
    d.stop().await;
}

/// Gate (c)'s envelope clause: every payload a runtime handed a client,
/// as it was handed, decodes and validates against the envelope schema,
/// both ways -- and each side saw both shapes, the compressed one
/// genuinely over the limit raw.
fn assert_captured_payloads_validate(handed: &[(&str, Vec<Handed>)]) {
    let schema = schema_validator("human-chat/envelope.schema.json");
    let mut control = serde_json::to_value(desktop_envelope(0, false)).expect("json");
    assert!(schema.is_valid(&control), "positive control");
    control["v"] = serde_json::json!(3);
    assert!(!schema.is_valid(&control), "the validator refuses a v3");
    for (name, payloads) in handed {
        let mut shapes = BTreeSet::new();
        for payload in payloads {
            let media_type = payload.media_type.as_deref().expect("a media type");
            let info = parse_media_type(media_type).expect("a HumanChatV2 media type");
            let text = decode_envelope_bytes(&payload.bytes, info.encoding).expect("decodes");
            let value: Value = serde_json::from_str(&text).expect("json");
            let errors: Vec<String> = schema.iter_errors(&value).map(|e| e.to_string()).collect();
            assert!(errors.is_empty(), "{name}: {media_type}: {errors:?}");
            HumanChatV2::parse(&text).expect("the envelope parses");
            let compressed = info.encoding == ContentEncoding::Brotli;
            if compressed {
                assert!(payload.bytes.len() <= MAX_PAYLOAD_BYTES);
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

/// The TCP port of `address` (`/ip4/127.0.0.1/tcp/<port>/...`).
fn tcp_port(address: &str) -> u16 {
    address
        .split('/')
        .skip_while(|p| *p != "tcp")
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no TCP port in {address}"))
}

fn pid(peer: &TransportIdentity) -> libp2p::PeerId {
    peer.as_str().parse().expect("a libp2p identity")
}
