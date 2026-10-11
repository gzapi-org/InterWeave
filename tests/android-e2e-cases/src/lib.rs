// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Android half of `tests/android-e2e`'s cases (plan §20 gate (c)):
//! one body, two runners (architect-cto's DECISION of 2026-10-10). The
//! host stand-in runs a case over the embedded runtime's in-process
//! binding, which proves the body on the host first; the app's
//! instrumentation runs the same body in the app's own process, over the
//! binding of the runtime the app's service started, so a failure on the
//! phone is a platform fact and not a case bug. The host never holds a
//! binding to the phone: it names a case, passes its arguments as JSON,
//! and reads the result JSON back -- [`run`] is that whole seam.
//!
//! What a case here does NOT do is the desktop side, the relay and the
//! lifecycle: those are the host's (`tests/android-e2e`), which pairs
//! each case with its own half -- D replying to what the Android side
//! sends, and reading the path D was told.
//!
//! TEST-ONLY. Its normal dependencies reach no libp2p, test harness,
//! daemon or transport runtime, so it can be built for the phone
//! (`the_crate_reaches_no_libp2p_harness_daemon_or_runtime` below). That
//! the release APK's graph names it nowhere is DECISION 01a12770's
//! invariant, checked by the packaging batch's check, which does not
//! exist yet.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Duration;

use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort as _, DataCapability, DataSessionBinding,
    DataSessionPort, LocalSessionEvent, ROUTE_ESTABLISHED, SessionEvent, SessionRequest,
};
use interweave_profile_config::{ProfilePaths, TrustBoundary, resolve_private_dir_within};
use interweave_transport_api::{
    DirectDestination, EndpointId, MediaType, MessageId, Payload, PeerPath, TransportIdentity,
};
use serde_json::{Map, Value};

mod human_chat;

pub use human_chat::{
    AppClient, Handed, HumanChatArgs, Recording, RecordingSession, Tap, chat_text,
    desktop_message_id, parse_payloads,
};

/// The result JSON's keys. The instrumentation copies each to the status
/// key `interweave.<key>` (the seam agreed with rust-ui-dev, 01a12771).
pub mod keys {
    /// The case's name, as asked.
    pub const CASE: &str = "case";
    /// [`PASS`] or [`FAIL`].
    pub const RESULT: &str = "result";
    /// Why a case failed; absent on a pass.
    pub const DETAIL: &str = "detail";
    /// The profile's `PeerId` (the `identity` case).
    pub const PEER: &str = "peer";
    /// The path a case observed to the desktop (`paths`).
    pub const PATH: &str = "path";
    /// Where the payloads the runtime handed the app's client were
    /// written, for the host to read and validate (`human_chat`; the
    /// file's shape is [`crate::parse_payloads`]'s).
    pub const PAYLOADS: &str = "payloads";
    /// The envelopes this side sent, as its facade committed them
    /// (`human_chat`): a JSON array.
    pub const SENT: &str = "sent";
    /// The runtime root the embedded runtime's private directories lie
    /// under (`trust_boundary`).
    pub const ROOT: &str = "runtime_root";
    /// The private directories found, each `<path> <octal mode>`
    /// (`trust_boundary`).
    pub const PRIVATE_DIRS: &str = "private_dirs";
    /// Why a directory under the platform's `files/` was refused
    /// (`trust_boundary`).
    pub const REFUSED: &str = "refused";
    /// [`RESULT`] of a case that held.
    pub const PASS: &str = "pass";
    /// [`RESULT`] of a case that did not, or could not run.
    pub const FAIL: &str = "fail";
}

/// The case names [`run`] answers.
pub mod cases {
    /// The profile's `PeerId`; needs no runtime.
    pub const IDENTITY: &str = "identity";
    /// Write the profile's `config.yaml` (argument `config`) through the
    /// runner's own provisioning, for the next start to read; needs no
    /// runtime.
    pub const PROVISION: &str = "provision";
    /// One direct message to the desktop and its reply, and the path this
    /// side was told the route began on. Arguments: [`super::PathsArgs`].
    pub const PATHS: &str = "paths";
    /// Plan §20 gate (d), in the app's process: the runtime root sits
    /// directly under the app data directory, it and every private
    /// directory of the profile (argument `profile`) are owner-only, and a
    /// private directory under the platform's `files/` -- which the
    /// ancestor walk alone accepts, the control -- is refused by the
    /// runtime root.
    pub const TRUST_BOUNDARY: &str = "trust_boundary";
    /// Plan §20 gate (g)'s Android half: set a peer (argument `peer`)
    /// trusted through the admin port, which the runtime records on its
    /// audit target. Whether the record reached the platform's log under
    /// the profile's stricter level is the host's to read, with
    /// `Device::log`.
    pub const AUDIT: &str = "audit";

    /// `HumanChatV2` with the desktop on the app's own client, both ways,
    /// plain and `;ce=br`. Arguments: [`super::HumanChatArgs`].
    pub const HUMAN_CHAT: &str = "human_chat";

    /// The cases a runner must have the runtime ALONE serving for: the
    /// app's runtime with the app's launch, identity and paths, and no
    /// store and no facade, so the `human` lease is free for the case
    /// (01a128a6/01a128a8). On the host stand-in that is an `EmbeddedHost`
    /// started so. On a phone it is to be a runtime-alone start in the
    /// app's service host, which the instrumentation's PR (rust-ui-dev's
    /// j66) adds; until then no phone runner exists. The others run
    /// before it starts, as [`PROVISION`] must.
    pub const NEED_A_RUNTIME: &[&str] = &[PATHS, TRUST_BOUNDARY, AUDIT];

    /// The cases a runner must have the app's service serving for in
    /// full -- runtime, store, facade and hub -- handing the case
    /// [`super::CaseCtx::client`].
    pub const NEED_THE_CLIENT: &[&str] = &[HUMAN_CHAT];
}

/// How long a case waits for a route, a message or a notice when its
/// arguments name no deadline: the harness's own patience.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(30);

/// What a runner hands a case.
pub struct CaseCtx<B> {
    /// The profile's `PeerId`.
    pub peer: TransportIdentity,
    /// The app's data directory: the trust boundary the platform
    /// supplies (ADR-0028), under which the runtime keeps its own.
    pub app_data_dir: PathBuf,
    /// The binding of the runtime now serving, or `None` when none is up:
    /// a case that needs one fails, saying so, rather than starting a
    /// runtime of its own -- the app's service holds the profile's lock,
    /// and a second runtime is not what the app ships.
    pub binding: Option<B>,
    /// The runner's own provisioning: writes `config.yaml` where the
    /// runtime's next start reads it, under the profile's private
    /// directories, as the app's first start does. `None` where the
    /// runner cannot provision.
    pub provision: Option<Provision>,
    /// The app's own client, for a case of [`cases::NEED_THE_CLIENT`]:
    /// `None` where the app's service is not serving in full.
    pub client: Option<Box<dyn AppClient>>,
}

/// How a runner writes the profile's configuration ([`CaseCtx::provision`]).
pub type Provision = Box<dyn FnOnce(&str) -> Result<(), String> + Send>;

/// Run `case` with `args_json`, and answer the result JSON: [`keys::CASE`],
/// [`keys::RESULT`], [`keys::DETAIL`] on a failure, and what the case
/// reports. A panic inside a case is that case's failure, caught here,
/// since the instrumentation calls this across JNI, where an unwind must
/// not cross.
pub fn run<B: DataSessionBinding + AdminBinding>(
    case: &str,
    args_json: &str,
    ctx: CaseCtx<B>,
) -> String {
    let outcome = catch_unwind(AssertUnwindSafe(|| dispatch(case, args_json, ctx)));
    let mut out = match outcome {
        Ok(Ok(fields)) => {
            let mut fields = fields;
            fields.insert(keys::RESULT.to_owned(), keys::PASS.into());
            fields
        }
        Ok(Err(detail)) => failed(&detail),
        Err(panic) => failed(&panic_text(panic.as_ref())),
    };
    out.insert(keys::CASE.to_owned(), case.into());
    Value::Object(out).to_string()
}

fn failed(detail: &str) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert(keys::RESULT.to_owned(), keys::FAIL.into());
    out.insert(keys::DETAIL.to_owned(), detail.into());
    out
}

fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "the case panicked".to_owned())
}

fn dispatch<B: DataSessionBinding + AdminBinding>(
    case: &str,
    args_json: &str,
    ctx: CaseCtx<B>,
) -> Result<Map<String, Value>, String> {
    let args: Value =
        serde_json::from_str(args_json).map_err(|e| format!("arguments are not JSON: {e}"))?;
    let mut out = Map::new();
    match case {
        cases::IDENTITY => {
            out.insert(keys::PEER.to_owned(), ctx.peer.as_str().into());
        }
        cases::PROVISION => {
            let config = args
                .get("config")
                .and_then(Value::as_str)
                .ok_or("argument \"config\" is missing")?;
            let provision = ctx
                .provision
                .ok_or("this runner cannot provision a profile")?;
            provision(config)?;
        }
        cases::PATHS => {
            let args = PathsArgs::from_json(&args)?;
            let binding = ctx
                .binding
                .ok_or_else(|| "no runtime is serving: start the app's service first".to_owned())?;
            let path = block_on(paths(&binding, &args))??;
            out.insert(keys::PATH.to_owned(), path.label().into());
        }
        cases::TRUST_BOUNDARY => {
            let profile = args
                .get("profile")
                .and_then(Value::as_str)
                .ok_or("argument \"profile\" is missing")?;
            if ctx.binding.is_none() {
                return Err("no runtime is serving: start the app's service first".to_owned());
            }
            out.extend(trust_boundary(&ctx.app_data_dir, profile)?);
        }
        cases::AUDIT => {
            let peer = args
                .get("peer")
                .and_then(Value::as_str)
                .and_then(|p| TransportIdentity::parse(p).ok())
                .ok_or("argument \"peer\" is not a PeerId")?;
            let binding = ctx
                .binding
                .ok_or_else(|| "no runtime is serving: start the app's service first".to_owned())?;
            block_on(audit(&binding, peer))??;
        }
        cases::HUMAN_CHAT => {
            let args = HumanChatArgs::from_json(&args)?;
            let mut client = ctx.client.ok_or_else(|| {
                "the app's client is not serving: start the app's service first".to_owned()
            })?;
            out.extend(human_chat::human_chat(
                client.as_mut(),
                &args,
                &ctx.app_data_dir,
            )?);
        }
        other => return Err(format!("no case is named {other:?}")),
    }
    Ok(out)
}

/// A current-thread runtime for one case. The bindings' futures talk to a
/// runtime driven elsewhere -- the embedded host's own executor -- so
/// this one needs only timers, and it is never the host's.
fn block_on<T>(f: impl Future<Output = T>) -> Result<T, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|e| format!("no runtime for the case: {e}"))?;
    Ok(rt.block_on(f))
}

/// The arguments of [`cases::PATHS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathsArgs {
    /// The desktop peer this side exchanges with.
    pub desktop: TransportIdentity,
    /// The path this side must be told its route to the desktop began on.
    pub path: PeerPath,
    /// Which exchange this is: it names both messages and their ids, so
    /// the host's half knows what to wait for and what to answer.
    pub serial: u8,
    /// How long each wait may take.
    pub deadline: Duration,
}

impl PathsArgs {
    /// The JSON a host passes: `desktop`, `path` (`relayed` or `direct`),
    /// `serial` (below 128), and `deadline_ms` (optional).
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("desktop".to_owned(), self.desktop.as_str().into());
        out.insert("path".to_owned(), self.path.label().into());
        out.insert("serial".to_owned(), self.serial.into());
        out.insert(
            "deadline_ms".to_owned(),
            u64::try_from(self.deadline.as_millis())
                .unwrap_or(u64::MAX)
                .into(),
        );
        Value::Object(out)
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
        let serial = field("serial")?
            .as_u64()
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| *n < 0x80)
            .ok_or("argument \"serial\" is not below 128")?;
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
            serial,
            deadline,
        })
    }
}

/// Gate (d) in the running app: what `resolve_embedded` resolves for
/// `profile` under the app data directory, judged on disk.
fn trust_boundary(app_data_dir: &Path, profile: &str) -> Result<Map<String, Value>, String> {
    // Where the directory resolves, not how it is spelled: the platform
    // hands over `/data/user/0/<package>`, a symlink to `/data/data` on
    // most devices, and the boundary below is canonical.
    let canonical = std::fs::canonicalize(app_data_dir)
        .map_err(|e| format!("the app data directory {}: {e}", app_data_dir.display()))?;
    let app_data_dir = canonical.as_path();
    let boundary = TrustBoundary::new(app_data_dir)
        .map_err(|e| format!("the app data directory as a boundary: {e}"))?;
    let walk_alone = boundary.clone();
    let paths = ProfilePaths::resolve_embedded(profile, boundary)
        .map_err(|e| format!("the profile's paths: {e}"))?;
    let root = paths
        .boundary()
        .runtime_root()
        .ok_or("the embedded boundary names no runtime root")?
        .to_path_buf();
    if root.parent() != Some(app_data_dir) {
        return Err(format!(
            "the runtime root {} is not directly under {}",
            root.display(),
            app_data_dir.display()
        ));
    }
    let mut found = vec![];
    for dir in [
        root.as_path(),
        paths.config_dir(),
        paths.state_dir(),
        paths.identity_dir(),
        paths.cache_dir(),
    ] {
        if !dir.exists() {
            continue;
        }
        if dir != root && !dir.starts_with(&root) {
            return Err(format!(
                "{} is outside the runtime root {}",
                dir.display(),
                root.display()
            ));
        }
        let mode = std::fs::metadata(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .permissions()
            .mode()
            & 0o7777;
        if mode != 0o700 {
            return Err(format!("{} is {mode:o}, not 700", dir.display()));
        }
        found.push(format!("{} {mode:o}", dir.display()));
    }
    // The runtime is serving, so at least its config and state are made.
    for dir in [paths.config_dir(), paths.state_dir()] {
        if !dir.exists() {
            return Err(format!(
                "{} does not exist under a serving runtime",
                dir.display()
            ));
        }
    }
    let refused = refused_under_files(app_data_dir, &walk_alone, paths.boundary())?;
    let mut out = Map::new();
    out.insert(keys::ROOT.to_owned(), root.display().to_string().into());
    out.insert(keys::PRIVATE_DIRS.to_owned(), found.into());
    out.insert(keys::REFUSED.to_owned(), refused.into());
    Ok(out)
}

/// A private directory made under the platform's `files/`: the walk
/// alone accepts it -- the control, since `files/` passes ADR-0028's
/// rule on a device -- and the embedded boundary refuses it. The probe is
/// removed either way. On a host stand-in, `files/` is made as Android
/// makes it, `0771`, where it is missing.
fn refused_under_files(
    app_data_dir: &Path,
    walk_alone: &TrustBoundary,
    embedded: &TrustBoundary,
) -> Result<String, String> {
    let files = app_data_dir.join("files");
    if !files.exists() {
        std::fs::create_dir(&files).map_err(|e| format!("files/: {e}"))?;
        std::fs::set_permissions(&files, std::fs::Permissions::from_mode(0o771))
            .map_err(|e| format!("files/: {e}"))?;
    }
    let probe = files.join("interweave-gate-d-probe");
    // A run killed between the probe's making and its removal leaves it
    // in a device's persistent files/; the next run takes it back first.
    match std::fs::remove_dir(&probe) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "a stale probe under files/ could not be removed: {e}"
            ));
        }
    }
    std::fs::create_dir(&probe).map_err(|e| format!("the probe under files/: {e}"))?;
    let outcome = (|| {
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("the probe: {e}"))?;
        resolve_private_dir_within(&probe, walk_alone).map_err(|e| {
            format!(
                "the control failed: the walk alone refused {}: {e}",
                probe.display()
            )
        })?;
        match resolve_private_dir_within(&probe, embedded) {
            Ok(_) => Err(format!(
                "{} under files/ was accepted by the embedded boundary",
                probe.display()
            )),
            Err(e) => Ok(e.to_string()),
        }
    })();
    let _ = std::fs::remove_dir(&probe);
    let refused = outcome?;
    if !refused.contains("outside the runtime root") {
        return Err(format!("refused, but not by the runtime root: {refused}"));
    }
    Ok(refused)
}

/// Gate (g)'s Android half: one trust change through the admin port.
async fn audit(binding: &impl AdminBinding, peer: TransportIdentity) -> Result<(), String> {
    let port = binding
        .admin(BTreeSet::from([AdminCapability::Trust]))
        .await
        .map_err(|_| "the admin port with the trust capability was refused".to_owned())?;
    port.set_trust(peer, true)
        .await
        .map_err(|_| "the trust change was refused".to_owned())
}

/// What the Android side sends in exchange `serial`, and its id.
#[must_use]
pub fn to_desktop(serial: u8) -> (MessageId, String) {
    (
        MessageId::from_bytes([serial; 16]),
        format!("android {serial} to desktop"),
    )
}

/// What the desktop answers in exchange `serial`, and its id: the high
/// bit keeps it apart from every id the Android side sends.
#[must_use]
pub fn to_android(serial: u8) -> (MessageId, String) {
    (
        MessageId::from_bytes([serial | 0x80; 16]),
        format!("desktop {serial} to android"),
    )
}

/// The Android half of a path case: send to the desktop until a route
/// exists, take its answer, and read the path this side was told the
/// route began on. Which path that must be is the case's to assert, and
/// it is asserted here, on the side that was told it.
async fn paths(binding: &impl DataSessionBinding, args: &PathsArgs) -> Result<PeerPath, String> {
    // The refusal is not printed: it can carry a trusted peer's identity
    // (`send_until_routed`'s note), and what failing here means is said.
    let session = binding
        .open(lease_request())
        .await
        .map_err(|_| "the human endpoint's lease was refused".to_owned())?;
    let (id, text) = to_desktop(args.serial);
    send_until_routed(&session, &args.desktop, id, &text, args.deadline).await?;
    let mut told = Told::default();
    let (_, answer) = to_android(args.serial);
    told.take_message(&session, &args.desktop, &answer, args.deadline)
        .await?;
    let (_, (previous, current)) = told
        .path_notice(&session, &args.desktop, ROUTE_ESTABLISHED, 0, args.deadline)
        .await?;
    if previous.is_some() || current != args.path {
        return Err(format!(
            "the route to the desktop began as {previous:?} -> {current:?}, not None -> {:?}",
            args.path
        ));
    }
    Ok(current)
}

/// The `human` endpoint, which the profiles of both sides lease.
///
/// # Panics
/// Never: the literal is a valid endpoint id.
#[must_use]
pub fn human() -> EndpointId {
    #[allow(clippy::expect_used, reason = "a literal checked by every case")]
    EndpointId::parse("human").expect("endpoint")
}

/// A human client's session on the `human` endpoint, with events and
/// commands -- the request `interweave-test-support`'s `e2e` harness
/// makes, restated because that harness never builds for Android.
///
/// # Panics
/// Never: the request is a valid one.
#[must_use]
pub fn lease_request() -> SessionRequest {
    #[allow(clippy::expect_used, reason = "a fixed request checked by every case")]
    SessionRequest::new(
        "human-client",
        Some(human()),
        [DataCapability::Events, DataCapability::Commands],
    )
    .expect("a request")
}

fn payload(text: &str) -> Result<Payload, String> {
    let media = MediaType::parse("text/plain").map_err(|e| format!("a media type: {e}"))?;
    Payload::at_ceiling(Some(media), text.as_bytes().to_vec())
        .map_err(|e| format!("a payload: {e}"))
}

/// Send `text` as `id` from `from` to `to` until a route exists, within
/// `deadline`.
///
/// # Errors
/// No route by the deadline.
pub async fn send_until_routed(
    from: &impl DataSessionPort,
    to: &TransportIdentity,
    id: MessageId,
    text: &str,
    deadline: Duration,
) -> Result<(), String> {
    let until = tokio::time::Instant::now() + deadline;
    loop {
        let sent = from
            .send_direct(
                DirectDestination {
                    peer: to.clone(),
                    endpoint: Some(human()),
                },
                id,
                payload(text)?,
            )
            .await;
        if sent.is_ok() {
            return Ok(());
        }
        // The refusal itself is not printed: this helper is generic over
        // every binding, the in-memory fake's among them, whose refusal
        // carries a trusted peer's identity, and a static analysis reads
        // printing it as logging that identity in clear.
        if tokio::time::Instant::now() >= until {
            return Err(format!("no route to {} within the deadline", to.as_str()));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// What one session has been told so far, kept across reads -- shared by
/// both halves, so the desktop's reading and the Android side's are the
/// same reading.
#[derive(Default)]
pub struct Told {
    /// Path notices as (peer, previous, current, reason class), in order.
    pub paths: Vec<(TransportIdentity, Option<PeerPath>, PeerPath, String)>,
    /// Disconnects, by peer, each with how many path notices preceded it.
    pub gone: Vec<(TransportIdentity, usize)>,
}

impl Told {
    /// Read `session` once, keeping what it said; whether a direct
    /// message `text` from `source` was among it.
    ///
    /// # Errors
    /// The session refused the read.
    pub async fn read(
        &mut self,
        session: &impl DataSessionPort,
        source: &TransportIdentity,
        text: &str,
    ) -> Result<bool, String> {
        let mut found = false;
        let events = session
            .events(64)
            .await
            .map_err(|_| "the session refused a read".to_owned())?;
        for event in events {
            match event {
                SessionEvent::Direct(m)
                    if &m.source_peer == source && m.payload.bytes() == text.as_bytes() =>
                {
                    found = true;
                }
                SessionEvent::Local(LocalSessionEvent::PeerPathChanged {
                    peer,
                    previous,
                    current,
                    reason_class,
                    ..
                }) => self.paths.push((peer, previous, current, reason_class)),
                SessionEvent::Local(LocalSessionEvent::PeerDisconnected { peer, .. }) => {
                    self.gone.push((peer, self.paths.len()));
                }
                _ => {}
            }
        }
        Ok(found)
    }

    /// Read until `text` from `source` arrives, within `deadline`.
    ///
    /// # Errors
    /// It did not arrive, or a read was refused.
    pub async fn take_message(
        &mut self,
        session: &impl DataSessionPort,
        source: &TransportIdentity,
        text: &str,
        deadline: Duration,
    ) -> Result<(), String> {
        let until = tokio::time::Instant::now() + deadline;
        while !self.read(session, source, text).await? {
            if tokio::time::Instant::now() >= until {
                return Err(format!("{text:?} from {} never arrived", source.as_str()));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }

    /// Read until a path notice about `peer` with reason `class` has been
    /// told at or after position `from`, within `deadline`; its position
    /// and the notice.
    ///
    /// # Errors
    /// None was told, or a read was refused.
    pub async fn path_notice(
        &mut self,
        session: &impl DataSessionPort,
        peer: &TransportIdentity,
        class: &str,
        from: usize,
        deadline: Duration,
    ) -> Result<(usize, (Option<PeerPath>, PeerPath)), String> {
        let until = tokio::time::Instant::now() + deadline;
        loop {
            if let Some((at, (_, previous, current, _))) = self
                .paths
                .iter()
                .enumerate()
                .skip(from)
                .find(|(_, (p, _, _, c))| p == peer && c == class)
            {
                return Ok((at, (*previous, *current)));
            }
            if tokio::time::Instant::now() >= until {
                let mut told = String::new();
                for (p, previous, current, c) in &self.paths {
                    let _ = write!(told, "[{} {previous:?}->{current:?} {c}] ", p.as_str());
                }
                return Err(format!("no {class} notice for {}: {told}", peer.as_str()));
            }
            self.read(session, peer, "").await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use interweave_local_client_api::MAX_EVENT_QUEUE;
    use interweave_local_client_fake::{FakeConfig, FakeEndpoint, FakeNetwork, FakeNode};

    fn peer() -> TransportIdentity {
        TransportIdentity::parse("12D3KooWD3eckifWpRn9wQpMG9R9hX3sD158z7EqHWmweQAJU5SA")
            .expect("a PeerId")
    }

    fn other_peer() -> TransportIdentity {
        TransportIdentity::parse("12D3KooWQYhTNQdmr3ArTeUHRYzFg94BKyTkoWBDWez9kSCVe2Xo")
            .expect("a PeerId")
    }

    fn config(peer: TransportIdentity) -> FakeConfig {
        FakeConfig {
            peer,
            endpoints: vec![FakeEndpoint::open(human(), false)],
            default_endpoint: Some(human()),
            queue_bound: MAX_EVENT_QUEUE,
        }
    }

    /// What the fake tells the Android side about its route to the
    /// desktop, once the route exists.
    #[derive(Clone, Copy)]
    enum Notice {
        /// A first connection on this path: `route_established`, no
        /// previous path.
        Connected(PeerPath),
        /// A `route_established` that nonetheless names a previous path.
        WithPrevious(PeerPath, PeerPath),
    }

    fn paths_on_the_fake(want: PeerPath, told: PeerPath) -> Map<String, Value> {
        paths_told(want, Notice::Connected(told))
    }

    /// The `paths` case on the fake pair -- its third runner: the Android
    /// side runs the case on a thread of its own, as the instrumentation
    /// does, while this test is the desktop's half, answering and then
    /// reporting the route as `told` from the Android side's view.
    fn paths_told(want: PeerPath, told: Notice) -> Map<String, Value> {
        let (android, desktop) = FakeNetwork::pair(config(peer()), config(other_peer()));
        // The pair starts connected directly, and a first route would be
        // announced on that path; with none, it waits for `connected`.
        android.disconnected(&other_peer());
        let args = PathsArgs {
            desktop: other_peer(),
            path: want,
            serial: 3,
            deadline: Duration::from_secs(5),
        };
        let case = {
            let android = android.clone();
            let args = args.to_json().to_string();
            std::thread::spawn(move || {
                run(
                    cases::PATHS,
                    &args,
                    CaseCtx {
                        peer: peer(),
                        app_data_dir: PathBuf::from("/nonexistent"),
                        binding: Some(android),
                        provision: None,
                        client: None,
                    },
                )
            })
        };
        block_on(desktop_half(&android, &desktop, told)).expect("a runtime");
        result(&case.join().expect("the case thread"))
    }

    async fn desktop_half(android: &FakeNode, desktop: &FakeNode, told: Notice) {
        let session = desktop.open(lease_request()).await.expect("D leases");
        let (_, text) = to_desktop(3);
        Told::default()
            .take_message(&session, &peer(), &text, Duration::from_secs(5))
            .await
            .expect("the Android side's message");
        // The Android side has sent, so its session holds the route the
        // notice is owed on.
        match told {
            Notice::Connected(path) => android.connected(&other_peer(), path),
            Notice::WithPrevious(previous, current) => {
                android.path_changed(&other_peer(), previous, current, ROUTE_ESTABLISHED, 0);
            }
        }
        let (id, answer) = to_android(3);
        send_until_routed(&session, &peer(), id, &answer, Duration::from_secs(5))
            .await
            .expect("D answers");
    }

    #[test]
    fn paths_passes_on_the_path_it_was_told_and_fails_on_another() {
        for path in [PeerPath::Relayed, PeerPath::Direct] {
            let out = paths_on_the_fake(path, path);
            assert_eq!(out[keys::RESULT], keys::PASS, "{path:?}: {out:?}");
            assert_eq!(out[keys::PATH], path.label());
        }
        let out = paths_on_the_fake(PeerPath::Relayed, PeerPath::Direct);
        assert_eq!(out[keys::RESULT], keys::FAIL, "{out:?}");
        let detail = out[keys::DETAIL].as_str().expect("a detail");
        assert!(
            detail.contains("Direct") && detail.contains("Relayed"),
            "{detail}"
        );
    }

    #[test]
    fn paths_fails_when_its_route_began_with_a_previous_path_even_the_right_one() {
        let out = paths_told(
            PeerPath::Relayed,
            Notice::WithPrevious(PeerPath::Direct, PeerPath::Relayed),
        );
        assert_eq!(out[keys::RESULT], keys::FAIL, "{out:?}");
        let detail = out[keys::DETAIL].as_str().expect("a detail");
        assert!(detail.contains("Some(Direct)"), "{detail}");
    }

    /// The packages this crate must never reach through a normal
    /// dependency: they do not build for the phone, or would put a second
    /// runtime or the test harness into the instrumentation.
    const NEVER: &[&str] = &[
        "interweave-test-support",
        "interweave-transport-daemon",
        "interweave-transport-libp2p",
        "interweave-transport-runtime",
        "interweave-transport-composition",
        "interweave-transport-embedded",
    ];

    #[test]
    fn the_crate_reaches_no_libp2p_harness_daemon_or_runtime() {
        // The graph as the phone's target resolves it -- the build the
        // claim is about -- which also keeps cargo from needing crates no
        // Android build uses (an iOS-only one is not in CI's cache).
        let out = std::process::Command::new(env!("CARGO"))
            .args([
                "metadata",
                "--format-version",
                "1",
                "--filter-platform",
                "aarch64-linux-android",
                "--manifest-path",
            ])
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .output()
            .expect("cargo metadata");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let meta: Value = serde_json::from_slice(&out.stdout).expect("metadata JSON");
        let name_of = |id: &str| {
            meta["packages"]
                .as_array()
                .expect("packages")
                .iter()
                .find(|p| p["id"] == id)
                .and_then(|p| p["name"].as_str())
                .expect("a package's name")
                .to_owned()
        };
        let nodes = meta["resolve"]["nodes"].as_array().expect("nodes");
        let me = nodes
            .iter()
            .find(|n| name_of(n["id"].as_str().expect("id")) == env!("CARGO_PKG_NAME"))
            .expect("this crate's node");
        // The normal-dependency closure: a dev or build edge never ships.
        let mut seen = std::collections::BTreeSet::new();
        let mut todo = vec![me["id"].as_str().expect("id").to_owned()];
        while let Some(id) = todo.pop() {
            let node = nodes
                .iter()
                .find(|n| n["id"] == id.as_str())
                .expect("a node");
            for dep in node["deps"].as_array().expect("deps") {
                let normal = dep["dep_kinds"]
                    .as_array()
                    .expect("dep_kinds")
                    .iter()
                    .any(|k| k["kind"].is_null());
                let pkg = dep["pkg"].as_str().expect("pkg").to_owned();
                if normal && seen.insert(name_of(&pkg)) {
                    todo.push(pkg);
                }
            }
        }
        // The walk is live: what the crate does depend on is in it.
        for wanted in [
            "interweave-local-client-api",
            "interweave-transport-api",
            "tokio",
        ] {
            assert!(seen.contains(wanted), "{wanted} missing from {seen:?}");
        }
        let forbidden: Vec<&String> = seen
            .iter()
            .filter(|n| n.starts_with("libp2p") || NEVER.contains(&n.as_str()))
            .collect();
        assert!(forbidden.is_empty(), "reached: {forbidden:?}");
    }

    /// An app data directory as the platform supplies it, owner-only, with
    /// the profile's config and state directories made by the profile
    /// crate itself, as a serving runtime leaves them.
    fn app_dir_as_a_runtime_leaves_it() -> (tempfile::TempDir, PathBuf) {
        let scratch = tempfile::tempdir().expect("scratch");
        let app = scratch.path().join("app");
        std::fs::create_dir(&app).expect("app");
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o700)).expect("0700");
        let paths = ProfilePaths::resolve_embedded(
            "human-android",
            TrustBoundary::new(&app).expect("boundary"),
        )
        .expect("paths");
        for dir in [paths.config_dir(), paths.state_dir()] {
            interweave_profile_config::create_private_dir_within(dir, paths.boundary())
                .expect("a private dir");
        }
        (scratch, app)
    }

    #[test]
    fn trust_boundary_passes_on_the_layout_a_runtime_leaves_and_reports_the_refusal() {
        let (_scratch, app) = app_dir_as_a_runtime_leaves_it();
        let out = trust_boundary(&app, "human-android").expect("gate (d) holds");
        // Where the root resolves: a host's temp directory may itself sit
        // behind a symlink, as /var/run does.
        let canonical = std::fs::canonicalize(&app).expect("canonical");
        assert_eq!(
            out[keys::ROOT],
            canonical.join("interweave").display().to_string()
        );
        let dirs = out[keys::PRIVATE_DIRS].as_array().expect("dirs");
        assert!(dirs.len() >= 3, "the root, config and state: {dirs:?}");
        assert!(
            dirs.iter()
                .all(|d| d.as_str().is_some_and(|d| d.ends_with(" 700"))),
            "{dirs:?}"
        );
        let refused = out[keys::REFUSED].as_str().expect("refused");
        assert!(refused.contains("outside the runtime root"), "{refused}");
        assert!(
            !app.join("files").join("interweave-gate-d-probe").exists(),
            "the probe is removed"
        );
    }

    #[test]
    fn trust_boundary_judges_where_a_symlinked_app_data_dir_resolves() {
        let (scratch, app) = app_dir_as_a_runtime_leaves_it();
        let link = scratch.path().join("data-user-0-link");
        std::os::unix::fs::symlink(&app, &link).expect("a symlink, as /data/user/0 is");
        let out = trust_boundary(&link, "human-android").expect("gate (d) holds through the link");
        let canonical = std::fs::canonicalize(&app).expect("canonical");
        assert_eq!(
            out[keys::ROOT],
            canonical.join("interweave").display().to_string()
        );
    }

    #[test]
    fn a_stale_probe_left_by_a_killed_run_is_taken_back() {
        let (_scratch, app) = app_dir_as_a_runtime_leaves_it();
        let files = app.join("files");
        std::fs::create_dir(&files).expect("files/");
        std::fs::set_permissions(&files, std::fs::Permissions::from_mode(0o771)).expect("0771");
        std::fs::create_dir(files.join("interweave-gate-d-probe")).expect("a stale probe");
        trust_boundary(&app, "human-android").expect("gate (d) holds after a killed run");
        assert!(
            !files.join("interweave-gate-d-probe").exists(),
            "the probe is removed"
        );
    }

    #[test]
    fn trust_boundary_fails_on_a_private_dir_that_is_not_owner_only() {
        let (_scratch, app) = app_dir_as_a_runtime_leaves_it();
        let paths = ProfilePaths::resolve_embedded(
            "human-android",
            TrustBoundary::new(&app).expect("boundary"),
        )
        .expect("paths");
        std::fs::set_permissions(paths.state_dir(), std::fs::Permissions::from_mode(0o750))
            .expect("chmod");
        let err = trust_boundary(&app, "human-android").expect_err("a 750 state dir");
        assert!(err.contains("is 750, not 700"), "{err}");
    }

    #[test]
    fn trust_boundary_fails_when_the_runtime_has_made_nothing() {
        let scratch = tempfile::tempdir().expect("scratch");
        let app = scratch.path().join("app");
        std::fs::create_dir(&app).expect("app");
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o700)).expect("0700");
        let err = trust_boundary(&app, "human-android").expect_err("nothing made");
        assert!(
            err.contains("does not exist under a serving runtime"),
            "{err}"
        );
    }

    #[test]
    fn audit_sets_the_peer_trusted_through_the_admin_port() {
        let (android, _desktop) = FakeNetwork::pair(config(peer()), config(other_peer()));
        let stranger =
            TransportIdentity::parse("12D3KooWJWoaqZhDaoEFshF7Rh1bpY9ohihFhzcW6d69Lr2NASuq")
                .expect("a PeerId");
        let args = serde_json::json!({ "peer": stranger.as_str() }).to_string();
        let out = result(&run(
            cases::AUDIT,
            &args,
            CaseCtx {
                peer: peer(),
                app_data_dir: PathBuf::from("/nonexistent"),
                binding: Some(android.clone()),
                provision: None,
                client: None,
            },
        ));
        assert_eq!(out[keys::RESULT], keys::PASS, "{out:?}");
        let admin = block_on(android.admin(BTreeSet::from([AdminCapability::Status])))
            .expect("rt")
            .expect("admin");
        let rows = block_on(admin.peers()).expect("rt").expect("rows");
        assert!(
            format!("{rows:?}").contains(stranger.as_str()),
            "the stranger is trusted now: {rows:?}"
        );
    }

    #[test]
    fn paths_arguments_round_trip_and_name_what_is_wrong() {
        let args = PathsArgs {
            desktop: peer(),
            path: PeerPath::Relayed,
            serial: 5,
            deadline: Duration::from_millis(1500),
        };
        assert_eq!(PathsArgs::from_json(&args.to_json()), Ok(args.clone()));
        for (field, bad, says) in [
            ("path", Value::from("both"), "\"path\""),
            ("serial", Value::from(128), "\"serial\""),
            ("desktop", Value::from("not a peer"), "\"desktop\""),
            ("deadline_ms", Value::from(-1), "\"deadline_ms\""),
        ] {
            let mut json = args.to_json();
            json[field] = bad;
            let err = PathsArgs::from_json(&json).expect_err(field);
            assert!(err.contains(says), "{field}: {err}");
        }
        let mut json = args.to_json();
        json.as_object_mut().unwrap().remove("deadline_ms");
        assert_eq!(
            PathsArgs::from_json(&json).map(|a| a.deadline),
            Ok(DEFAULT_DEADLINE)
        );
    }

    #[test]
    fn the_two_messages_of_an_exchange_never_share_an_id() {
        for serial in 0..0x80 {
            assert_ne!(to_desktop(serial).0, to_android(serial).0, "{serial}");
            assert_ne!(to_desktop(serial).1, to_android(serial).1, "{serial}");
        }
    }

    fn result(json: &str) -> Map<String, Value> {
        match serde_json::from_str(json).expect("result JSON") {
            Value::Object(map) => map,
            other => panic!("not an object: {other}"),
        }
    }

    #[test]
    fn provision_hands_the_runner_exactly_the_config_and_reports_its_refusal() {
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let ctx = |answer: Result<(), String>| {
            let written = std::sync::Arc::clone(&written);
            CaseCtx::<FakeNode> {
                peer: peer(),
                app_data_dir: PathBuf::from("/nonexistent"),
                binding: None,
                provision: Some(Box::new(move |config: &str| {
                    written.lock().expect("lock").push(config.to_owned());
                    answer
                })),
                client: None,
            }
        };
        let config = "profile:\n  name: \"x\"\n";
        let args = serde_json::json!({ "config": config }).to_string();
        let out = result(&run(cases::PROVISION, &args, ctx(Ok(()))));
        assert_eq!(out[keys::RESULT], keys::PASS, "{out:?}");
        let out = result(&run(
            cases::PROVISION,
            &args,
            ctx(Err("disk full".to_owned())),
        ));
        assert_eq!(out[keys::RESULT], keys::FAIL);
        assert_eq!(out[keys::DETAIL], "disk full");
        assert_eq!(*written.lock().expect("lock"), [config, config]);
    }

    #[test]
    fn identity_answers_the_peer_without_a_runtime() {
        let out = result(&run::<FakeNode>(
            cases::IDENTITY,
            "{}",
            CaseCtx {
                peer: peer(),
                app_data_dir: PathBuf::from("/nonexistent"),
                binding: None,
                provision: None,
                client: None,
            },
        ));
        assert_eq!(out[keys::RESULT], keys::PASS);
        assert_eq!(out[keys::CASE], cases::IDENTITY);
        assert_eq!(out[keys::PEER], peer().as_str());
        assert!(!out.contains_key(keys::DETAIL));
    }

    /// A binding whose `open` panics: what a case body does when one of
    /// its own checks fails by panicking, as the bodies moved here from
    /// `tests/android-e2e` may.
    struct Panics;

    impl DataSessionBinding for Panics {
        type Session = interweave_local_client_fake::FakeSession;

        #[allow(
            clippy::panic,
            clippy::unused_async_trait_impl,
            reason = "the panic is the input under test, raised where the trait's future runs"
        )]
        async fn open(
            &self,
            _: SessionRequest,
        ) -> Result<Self::Session, interweave_transport_api::TransportError> {
            panic!("a check inside the case failed")
        }
    }

    impl AdminBinding for Panics {
        type Admin = interweave_local_client_fake::FakeAdmin;

        #[allow(
            clippy::panic,
            clippy::unused_async_trait_impl,
            reason = "the panic is the input under test, raised where the trait's future runs"
        )]
        async fn admin(
            &self,
            _: BTreeSet<AdminCapability>,
        ) -> Result<Self::Admin, interweave_transport_api::TransportError> {
            panic!("a check inside the case failed")
        }
    }

    #[test]
    fn a_panic_inside_a_case_is_its_failure_and_does_not_unwind_out_of_run() {
        let args = PathsArgs {
            desktop: peer(),
            path: PeerPath::Direct,
            serial: 1,
            deadline: DEFAULT_DEADLINE,
        }
        .to_json()
        .to_string();
        // `run` returning at all is the property: an unwind would end
        // this test here, as it would abort the app across JNI.
        let out = result(&run(
            cases::PATHS,
            &args,
            CaseCtx {
                peer: peer(),
                app_data_dir: PathBuf::from("/nonexistent"),
                binding: Some(Panics),
                provision: None,
                client: None,
            },
        ));
        assert_eq!(out[keys::RESULT], keys::FAIL);
        assert_eq!(out[keys::DETAIL], "a check inside the case failed");
    }

    #[test]
    fn a_case_that_cannot_run_fails_saying_why_and_never_unwinds() {
        let ctx = || CaseCtx::<FakeNode> {
            peer: peer(),
            app_data_dir: PathBuf::from("/nonexistent"),
            binding: None,
            provision: None,
            client: None,
        };
        let args = PathsArgs {
            desktop: peer(),
            path: PeerPath::Direct,
            serial: 1,
            deadline: DEFAULT_DEADLINE,
        }
        .to_json()
        .to_string();
        let chat = HumanChatArgs {
            desktop: peer(),
            path: PeerPath::Direct,
            side: "C".to_owned(),
            sends: [3, 7],
            answers: [4, 8],
            deadline: DEFAULT_DEADLINE,
        }
        .to_json()
        .to_string();
        for (case, args, says) in [
            ("nonsense", "{}", "no case is named"),
            (cases::PATHS, "not json", "not JSON"),
            (cases::PATHS, "{}", "\"desktop\" is missing"),
            (cases::PATHS, args.as_str(), "no runtime is serving"),
            (cases::PROVISION, "{}", "\"config\" is missing"),
            (cases::PROVISION, r#"{"config":"x"}"#, "cannot provision"),
            (cases::HUMAN_CHAT, "{}", "\"desktop\" is missing"),
            (
                cases::HUMAN_CHAT,
                chat.as_str(),
                "the app's client is not serving",
            ),
        ] {
            let out = result(&run(case, args, ctx()));
            assert_eq!(out[keys::RESULT], keys::FAIL, "{case} {args}");
            assert_eq!(out[keys::CASE], case);
            let detail = out[keys::DETAIL].as_str().expect("a detail");
            assert!(detail.contains(says), "{case} {args}: {detail}");
        }
    }
}
