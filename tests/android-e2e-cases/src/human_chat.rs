// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `HumanChatV2`'s Android half (plan §20 gate (c)'s human-client
//! clause), run on the app's own client: the facade, store and hub the
//! app's service composed, driven the way the app's window drives them --
//! a model side fed the hub's updates, with a scripted surface typing a
//! draft and pressing Send where a person would. A runner hands it
//! [`AppClient`], and the case drives that client rather than one of its
//! own, since on the phone the store and the `human` lease are the
//! service's (the seam
//! agreed with p2p-network-dev-02, 01a128a6/01a128a8).
//!
//! What the case does: two messages to the desktop, one plain and one
//! only `;ce=br` can carry, each accepted at the desktop's `human`
//! endpoint; the desktop's two answers shown, from the desktop; the
//! route indicator naming the path the host expects; and every payload
//! the runtime handed the client, as handed, written for the host to
//! validate against the envelope schema -- the schema is the host's, so
//! this crate builds without it.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use interweave_human_app_core::{Command, ModelSide, Opener, Update};
use interweave_human_chat_protocol::HumanChatV2;
use interweave_human_client_api::OutboundStatus;
use interweave_human_ui_model::{ConversationKey, Direction, ItemStatus, UiModel, ViewEvent};
use interweave_local_client_api::{
    DataSessionBinding, DataSessionPort, LocalDataSession, SessionEvent, SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointDirectoryV1, EndpointId,
    MAX_PAYLOAD_BYTES, MessageId, Payload, PeerPath, TransportError, TransportIdentity,
};
use serde_json::{Map, Value};

use crate::{DEFAULT_DEADLINE, human, keys};

/// The app's own human client, as a runner hands it to a case that
/// needs it ([`crate::cases::NEED_THE_CLIENT`]): the app's hub on the
/// phone and on the host alike, so the case drives what the window
/// drives.
pub trait AppClient: Send {
    /// Hand the facade `command`, as the window does; false while no
    /// facade runs.
    fn command(&mut self, command: Command) -> bool;

    /// What the facade sent the view since the last call, waiting at
    /// most `wait` for the first of it.
    ///
    /// # Errors
    /// The view was detached, the store could not be listed for it, or
    /// the service stopped: nothing more will come.
    fn updates(&mut self, wait: Duration) -> Result<Vec<Update>, String>;

    /// Every payload the runtime handed the facade, as handed.
    fn handed(&self) -> Vec<Handed>;
}

/// A payload as a runtime handed it to a client, before the facade
/// decoded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handed {
    /// Its media type.
    pub media_type: Option<String>,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// Where a [`Recording`] keeps what it saw.
pub type Tap = Arc<Mutex<Vec<Handed>>>;

/// A binding recording every direct payload its sessions hand over. It
/// changes nothing it passes on: the facade sees what the runtime wrote.
#[derive(Clone)]
pub struct Recording<B> {
    inner: B,
    tap: Tap,
}

impl<B> Recording<B> {
    /// `inner`, recording into `tap`.
    pub const fn new(inner: B, tap: Tap) -> Self {
        Self { inner, tap }
    }
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

/// A [`Recording`]'s session.
pub struct RecordingSession<S> {
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
            let mut tap = tap.lock().unwrap_or_else(PoisonError::into_inner);
            for event in &events {
                if let SessionEvent::Direct(m) = event {
                    tap.push(Handed {
                        media_type: m.payload.media_type().map(|t| t.as_str().to_owned()),
                        bytes: m.payload.bytes().to_vec(),
                    });
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

/// The text side `from` sends in exchange `serial`; `compressible` makes
/// it larger than the payload limit raw, so only the fit fallback
/// (`;ce=br`) can carry it.
#[must_use]
pub fn chat_text(serial: u8, from: &str, compressible: bool) -> String {
    let mut text = format!("message {serial} from **{from}**");
    if compressible {
        let line = format!("\n- {from} repeats this line so brotli has something to fold");
        while text.len() <= MAX_PAYLOAD_BYTES + 4096 {
            text.push_str(&line);
        }
    }
    text
}

/// The application id the desktop gives its answer `serial`: fixed, so
/// the Android side knows what to look for.
#[must_use]
pub fn desktop_message_id(serial: u8) -> String {
    format!("{serial:032x}")
}

/// The arguments of [`crate::cases::HUMAN_CHAT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanChatArgs {
    /// The desktop peer this side exchanges with.
    pub desktop: TransportIdentity,
    /// The path the route indicator must name once both ways crossed.
    pub path: PeerPath,
    /// This side's name in the texts it sends (`R`, `C`).
    pub side: String,
    /// The serials of the two messages this side sends: plain, then
    /// compressible.
    pub sends: [u8; 2],
    /// The serials of the desktop's two answers: plain, then compressible.
    pub answers: [u8; 2],
    /// How long the whole exchange may take.
    pub deadline: Duration,
}

impl HumanChatArgs {
    /// The JSON a host passes.
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "desktop": self.desktop.as_str(),
            "path": self.path.label(),
            "side": self.side,
            "sends": self.sends,
            "answers": self.answers,
            "deadline_ms": u64::try_from(self.deadline.as_millis()).unwrap_or(u64::MAX),
        })
    }

    /// Parse what [`to_json`](Self::to_json) wrote.
    ///
    /// # Errors
    /// A missing or malformed field, by name.
    pub fn from_json(args: &Value) -> Result<Self, String> {
        let field = |name: &str| {
            args.get(name)
                .ok_or_else(|| format!("argument {name:?} is missing"))
        };
        let desktop = field("desktop")?
            .as_str()
            .and_then(|s| TransportIdentity::parse(s).ok())
            .ok_or("argument \"desktop\" is not a PeerId")?;
        let path = match field("path")?.as_str() {
            Some("relayed") => PeerPath::Relayed,
            Some("direct") => PeerPath::Direct,
            _ => return Err("argument \"path\" is neither \"relayed\" nor \"direct\"".to_owned()),
        };
        let side = field("side")?
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("argument \"side\" is not a name")?
            .to_owned();
        let pair = |name: &str| -> Result<[u8; 2], String> {
            let bad = || format!("argument {name:?} is not two serials");
            let list = field(name)?.as_array().ok_or_else(bad)?;
            let serial = |v: &Value| v.as_u64().and_then(|n| u8::try_from(n).ok());
            match list.as_slice() {
                [a, b] => Ok([serial(a).ok_or_else(bad)?, serial(b).ok_or_else(bad)?]),
                _ => Err(bad()),
            }
        };
        let deadline = match args.get("deadline_ms") {
            None => DEFAULT_DEADLINE,
            Some(v) => Duration::from_millis(
                v.as_u64()
                    .ok_or("argument \"deadline_ms\" is not a count")?,
            ),
        };
        Ok(Self {
            desktop,
            path,
            side,
            sends: pair("sends")?,
            answers: pair("answers")?,
            deadline,
        })
    }
}

/// The surface a person would be: types a draft, then presses Send, one
/// event per take, as `Surface::take_events` asks (a draft edit ends a
/// take).
#[derive(Default)]
struct Script {
    queue: VecDeque<Step>,
}

enum Step {
    Type(ConversationKey, String),
    PressSend(ConversationKey),
}

impl interweave_human_app_core::Surface for Script {
    fn render(&mut self, _model: &UiModel) {}

    fn take_events(&mut self, model: &UiModel) -> Vec<ViewEvent> {
        match self.queue.pop_front() {
            None => Vec::new(),
            Some(Step::Type(key, draft)) => vec![ViewEvent::DraftChanged { key, draft }],
            // Resolved against the model at take time, as a view's press
            // is: the composer's draft is what goes.
            Some(Step::PressSend(key)) => model
                .send_draft(&key)
                .map(ViewEvent::Intent)
                .into_iter()
                .collect(),
        }
    }
}

struct NoLinks;

impl Opener for NoLinks {
    fn open(&mut self, _destination: &str) {}
}

/// What the case has seen of the facade, beyond the model.
#[derive(Default)]
struct Seen {
    /// The store's listing arrived: the hub sends a view no update
    /// before it, so nothing typed earlier could be followed.
    listed: bool,
    /// Each send the facade committed, as committed.
    sent: Vec<HumanChatV2>,
}

/// One step of the window's loop: apply what came, run a turn, hand the
/// facade what the turn asked.
fn step(
    client: &mut dyn AppClient,
    side: &mut ModelSide<Script, NoLinks>,
    seen: &mut Seen,
) -> Result<(), String> {
    for update in client.updates(Duration::from_millis(100))? {
        match &update {
            Update::Listed(_) => seen.listed = true,
            Update::Sent { envelope, .. } => seen.sent.push(envelope.clone()),
            _ => {}
        }
        side.apply(update);
    }
    for command in side.turn() {
        if !client.command(command) {
            return Err("the app's facade is not running".to_owned());
        }
    }
    Ok(())
}

/// Step until `done` holds of the model, or fail naming `what`.
fn step_until(
    client: &mut dyn AppClient,
    side: &mut ModelSide<Script, NoLinks>,
    seen: &mut Seen,
    until: Instant,
    what: &str,
    done: impl Fn(&UiModel, &Seen) -> bool,
) -> Result<(), String> {
    loop {
        step(client, side, seen)?;
        if done(side.model(), seen) {
            return Ok(());
        }
        if Instant::now() >= until {
            return Err(format!("{what} did not happen within the deadline"));
        }
    }
}

/// The Android half: see the module note. Answers the result fields.
pub(crate) fn human_chat(
    client: &mut dyn AppClient,
    args: &HumanChatArgs,
    app_data_dir: &Path,
) -> Result<Map<String, Value>, String> {
    let until = Instant::now() + args.deadline;
    let key = ConversationKey::Direct {
        peer: args.desktop.clone(),
        endpoint: Some(human()),
    };
    let mut side = ModelSide::new(Script::default(), NoLinks);
    let mut seen = Seen::default();
    step_until(
        client,
        &mut side,
        &mut seen,
        until,
        "the store's listing for the view",
        |_, seen| seen.listed,
    )?;

    // Each draft typed and sent as a person would, one at a time: the
    // window offers one send per conversation in flight. A refusal with
    // no row (no session yet) leaves the draft in the composer, and Send
    // is pressed again.
    for (serial, compressible) in [(args.sends[0], false), (args.sends[1], true)] {
        let text = chat_text(serial, &args.side, compressible);
        let queue = &mut side.surface_mut().queue;
        queue.push_back(Step::Type(key.clone(), text.clone()));
        queue.push_back(Step::PressSend(key.clone()));
        let mut last_press = Instant::now();
        loop {
            step(client, &mut side, &mut seen)?;
            if seen.sent.iter().any(|e| e.text == text) {
                break;
            }
            let composer = side.model().composer(&key);
            if composer.refused.is_some()
                && composer.draft == text
                && last_press.elapsed() > Duration::from_millis(500)
                && side.surface_mut().queue.is_empty()
            {
                side.surface_mut()
                    .queue
                    .push_back(Step::PressSend(key.clone()));
                last_press = Instant::now();
            }
            if Instant::now() >= until {
                return Err(format!(
                    "the send of message {serial} was never committed: {:?}",
                    composer.refused
                ));
            }
        }
    }

    let answers: Vec<String> = [(args.answers[0], false), (args.answers[1], true)]
        .into_iter()
        .map(|(serial, compressible)| chat_text(serial, "D", compressible))
        .collect();
    let outbound: Vec<String> = seen.sent.iter().map(|e| e.text.clone()).collect();
    step_until(
        client,
        &mut side,
        &mut seen,
        until,
        "both messages accepted at the desktop and both answers shown",
        |model, _| {
            let items = model.messages(&key);
            let accepted = outbound.iter().all(|text| {
                items.iter().any(|i| {
                    i.direction == Direction::Outbound
                        && &i.source == text
                        && matches!(
                            &i.status,
                            ItemStatus::Outbound(OutboundStatus::Accepted { endpoint })
                                if endpoint == &human()
                        )
                })
            });
            let shown = answers.iter().all(|text| {
                items.iter().any(|i| {
                    i.direction == Direction::Inbound
                        && &i.source == text
                        && i.author.as_ref() == Some(&args.desktop)
                })
            });
            accepted && shown
        },
    )?;
    step_until(
        client,
        &mut side,
        &mut seen,
        until,
        "the route indicator",
        |model, _| model.path(&key).is_some(),
    )?;
    let path = side.model().path(&key);
    if path != Some(args.path) {
        return Err(format!(
            "the route indicator names {path:?}, not {:?}",
            args.path
        ));
    }

    let payloads = write_payloads(app_data_dir, &args.side, &client.handed())?;
    let mut out = Map::new();
    out.insert(keys::PATH.to_owned(), args.path.label().into());
    out.insert(
        keys::PAYLOADS.to_owned(),
        payloads.to_string_lossy().into_owned().into(),
    );
    out.insert(
        keys::SENT.to_owned(),
        Value::Array(
            seen.sent
                .iter()
                .map(|e| serde_json::to_value(e).map_err(|e| format!("an envelope: {e}")))
                .collect::<Result<_, _>>()?,
        ),
    );
    Ok(out)
}

/// Write `handed` under the app data directory, in a directory of its
/// own made owner-only, one line per payload (see [`parse_payloads`]),
/// and answer the file's path for the host to read.
fn write_payloads(app_data_dir: &Path, side: &str, handed: &[Handed]) -> Result<PathBuf, String> {
    let dir = app_data_dir.join("e2e");
    std::fs::create_dir_all(&dir).map_err(|e| format!("the payload directory: {e}"))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("the payload directory's mode: {e}"))?;
    let file = dir.join(format!("payloads-{side}.txt"));
    let mut text = String::new();
    for h in handed {
        let _ = write!(text, "{}\t", h.media_type.as_deref().unwrap_or(""));
        for b in &h.bytes {
            let _ = write!(text, "{b:02x}");
        }
        text.push('\n');
    }
    std::fs::write(&file, text).map_err(|e| format!("the payload file: {e}"))?;
    Ok(file)
}

/// The payloads a [`crate::cases::HUMAN_CHAT`] run wrote: one line each,
/// the media type (empty for none), a tab, and the bytes in lowercase
/// hex.
///
/// # Errors
/// A line that is not that shape, by number.
pub fn parse_payloads(text: &str) -> Result<Vec<Handed>, String> {
    text.lines()
        .enumerate()
        .map(|(n, line)| {
            let bad = || format!("payload line {} is malformed", n + 1);
            let (media_type, hex) = line.split_once('\t').ok_or_else(bad)?;
            if hex.len() % 2 != 0 {
                return Err(bad());
            }
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| {
                    u8::from_str_radix(hex.get(i..i + 2).ok_or_else(bad)?, 16).map_err(|_| bad())
                })
                .collect::<Result<_, _>>()?;
            Ok(Handed {
                media_type: (!media_type.is_empty()).then(|| media_type.to_owned()),
                bytes,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_round_trip_through_their_file() {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("a scratch root");
        let handed = vec![
            Handed {
                media_type: Some("application/vnd.interweave.human-chat+json;v=2".to_owned()),
                bytes: b"{\"v\":2}".to_vec(),
            },
            Handed {
                media_type: None,
                bytes: vec![0x00, 0xff, 0x10],
            },
            Handed {
                media_type: Some("text/plain".to_owned()),
                bytes: Vec::new(),
            },
        ];
        let file = write_payloads(dir.path(), "R", &handed).expect("written");
        let mode = std::fs::metadata(file.parent().expect("a parent"))
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "the payload directory is private");
        let text = std::fs::read_to_string(&file).expect("read");
        assert_eq!(parse_payloads(&text).expect("parsed"), handed);
        assert!(parse_payloads("no tab here").is_err());
        assert!(parse_payloads("text/plain\tabc").is_err(), "odd hex");
        assert!(parse_payloads("text/plain\tzz").is_err(), "not hex");
    }

    #[test]
    fn the_arguments_round_trip_and_name_what_is_wrong() {
        let args = HumanChatArgs {
            desktop: TransportIdentity::parse(
                "12D3KooWGzjuH8Y6m1wHn4J8yHnD7tTQbR3H3wGQW8nP4kPZB2xm",
            )
            .expect("a PeerId"),
            path: PeerPath::Relayed,
            side: "R".to_owned(),
            sends: [1, 5],
            answers: [2, 6],
            deadline: Duration::from_secs(7),
        };
        assert_eq!(HumanChatArgs::from_json(&args.to_json()), Ok(args.clone()));
        let mut broken = args.to_json();
        broken["sends"] = serde_json::json!([1]);
        assert_eq!(
            HumanChatArgs::from_json(&broken),
            Err("argument \"sends\" is not two serials".to_owned())
        );
        let mut broken = args.to_json();
        broken["side"] = serde_json::json!("");
        assert!(HumanChatArgs::from_json(&broken).is_err());
    }

    #[test]
    fn a_compressible_text_is_over_the_limit_and_a_plain_one_is_not() {
        assert!(chat_text(1, "R", true).len() > MAX_PAYLOAD_BYTES);
        assert!(chat_text(1, "R", false).len() < 64);
        assert_ne!(chat_text(1, "R", false), chat_text(1, "C", false));
    }
}
