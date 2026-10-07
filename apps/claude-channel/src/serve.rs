// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The bridge's one loop: the host's lines in, the session's events out,
//! and the session kept open across the daemon going away
//! (`plugin/LIFECYCLE.md`, `plugin/CLAUDE-CODE-CHANNEL.md` §Session
//! behavior).
//!
//! **Generic over [`DataSessionBinding`] and nothing else.** The bridge
//! opens data sessions and has no way to reach an admin port: the
//! authority split of `plugin/SECURITY.md` §Administrative separation is
//! in the signature, and every session it opens asks `events` and
//! `commands` only (`tests/fake.rs`,
//! `no_administrative_request_leaves_the_bridge`).
//!
//! **The daemon away is a state, never an exit.** A bridge that exits is
//! not restarted by the host (SPIKE-001 fact 19), so a failed open or a
//! session that ends is retried with bounded exponential backoff, each
//! retry a fresh claim and fresh joins of the channels joined through
//! this bridge; meanwhile `status` answers and network tools are refused
//! with `BackendUnavailable`. Nothing missed is replayed, and a direct
//! reply token from before is stale by its epoch.
//!
//! **One task, so one writer.** Answers and notifications are written by
//! this loop alone, a whole line at a time.
//!
//! **A call in flight does not stop the draining.** Over IPC, a session
//! whose events are not taken stops reading its socket once its event
//! buffer is full, and a call's answer queued behind those events would
//! never arrive (ipc-client's `IpcSession` says so). So every session
//! call runs beside the session's events, which are converted and
//! written as they come -- bounded by stdout's own backpressure, never
//! held aside (`tests/ipc_burst.rs`,
//! `a_call_answered_behind_a_burst_of_events_completes`).

use std::collections::BTreeMap;
use std::time::Duration;

use interweave_claude_channel_core::{
    BROADCAST_ACCEPTED, BridgeState, ChannelMeta, ChannelNotification, Delivery, PullQueue,
    ReplyRoute, ToolCall, direct_accepted, error_text, parse_call,
};
use interweave_local_client_api::Generation;
use interweave_local_client_api::{
    DataCapability, DataSessionBinding, DataSessionPort, LocalSessionEvent, SessionEvent,
    SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, ConnectivitySummary, DirectDestination, EndpointId, Health,
    MessageId, TransportError, TransportIdentity,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::time::Instant;

use crate::mcp::{
    INVALID_PARAMS, Incoming, error_line, notification_line, parse_line, protocol_answer,
    tool_result_line,
};

/// The client kind the bridge presents: a hygiene label, never authority
/// (ADR-0037).
pub const CLIENT_KIND: &str = "claude-channel";

/// Events taken from the session per wake: the loop returns to the
/// host's lines between batches.
pub const EVENT_BATCH: usize = 64;

/// The first reconnect delay.
pub const RECONNECT_FIRST: Duration = Duration::from_millis(250);
/// The longest reconnect delay: the backoff doubles up to here and stays
/// (`the_reconnect_backoff_doubles_to_its_ceiling`).
pub const RECONNECT_CEILING: Duration = Duration::from_secs(30);

/// What the bridge is configured with.
#[derive(Debug, Clone)]
pub struct Config {
    /// The local endpoint it claims; never a substitute (LIFECYCLE.md
    /// §Endpoint conflict).
    pub endpoint: EndpointId,
    /// `status.profile_desired_channels`: the profile document's desired
    /// channels AS CONFIGURED, or the class of the error that kept them
    /// from being read -- unknown, never an empty list (TOOL-SURFACE.md
    /// §Status visibility).
    pub desired_channels: Result<Vec<ChannelId>, String>,
    /// How inbound messages reach the host (`--delivery`, required):
    /// pushed as `notifications/claude/channel`, or queued for `receive`.
    pub delivery: Delivery,
}

/// A session-bound tool's answer while the pull queue is full: the
/// bridge's own state, named, never a stall and never `Overloaded`, which
/// says the transport is busy (architect-cto's ruling, relay seq 18798).
pub const PULL_QUEUE_FULL: &str = "the pull queue is full: call receive first";

/// The same, for a call cancelled in flight as the queue filled: the
/// cancel is advisory, so the daemon may have done it -- a repeated send
/// or broadcast may go twice, a cancelled leave may have left
/// (TOOL-SURFACE.md, A 2026-10-07).
pub const PULL_QUEUE_FULL_CANCELLED: &str =
    "the pull queue is full: call receive first; the call was cancelled in flight, outcome unknown";

/// One message waiting in the pull queue, as the session gave it, with
/// the lease it arrived under: its token is minted when the host takes
/// it, under that lease, so its TTL runs from the reading and a message
/// from before a reconnect stays stale by its epoch (relay seq 18784).
#[derive(Debug)]
struct Pulled {
    event: SessionEvent,
    lease: Option<(EndpointId, Generation)>,
}

/// Pull mode's queue and when it last filled: `paused_since` is when the
/// bridge stopped taking from its session, in the env's milliseconds --
/// not a deadline, since the daemon's liveness clock starts only once the
/// IPC client's own buffer fills behind it (relay seq 18835).
#[derive(Debug)]
struct Pull {
    queue: PullQueue<Pulled>,
    paused_since: Option<u64>,
}

impl Pull {
    fn room(&self) -> usize {
        self.queue.room()
    }

    fn is_full(&self) -> bool {
        self.queue.is_full()
    }

    /// Queue `pulled`, false when the queue was full; the one that fills
    /// it starts the pause.
    fn push(&mut self, pulled: Pulled, now_ms: u64) -> bool {
        if self.queue.push(pulled).is_err() {
            return false;
        }
        if self.queue.is_full() && self.paused_since.is_none() {
            self.paused_since = Some(now_ms);
        }
        true
    }

    /// Take at most `max`; a take that makes room ends the pause.
    fn take(&mut self, max: usize) -> interweave_claude_channel_core::Take<Pulled> {
        let take = self.queue.take(max);
        if !self.queue.is_full() {
            self.paused_since = None;
        }
        take
    }
}

/// Why a channel the bridge held was not re-joined at a reconnect: the
/// daemon refused it, or the pull queue filled while the join was in
/// flight and it was cancelled. Either stands until the next join or
/// leave of that channel.
#[derive(Debug, Clone, Copy)]
enum RejoinRefusal {
    Daemon(TransportError),
    PullQueueFull,
}

impl RejoinRefusal {
    fn label(self) -> String {
        match self {
            Self::Daemon(e) => format!("{e:?}"),
            Self::PullQueueFull => "PullQueueFull".to_owned(),
        }
    }
}

/// `receive`'s answer: the events in the order taken from the session,
/// what remains, and whether the drain is paused (architect-cto's ruling,
/// relay seq 18784). Written by `serde` straight to text, so each `meta`
/// keeps the contract's table order, as the push does.
#[derive(Serialize)]
struct Received<'a> {
    events: Vec<ReceivedEvent<'a>>,
    remaining: usize,
    paused: bool,
}

#[derive(Serialize)]
struct ReceivedEvent<'a> {
    kind: &'static str,
    content: &'a str,
    meta: &'a ChannelMeta,
}

/// The bridge's clock and entropy: the caller's, so a test runs on its
/// own.
pub struct Env {
    /// Unix-epoch milliseconds.
    pub now_ms: Box<dyn Fn() -> u64 + Send>,
    /// 16 bytes from a CSPRNG: a reply token, or a message id.
    pub entropy: Box<dyn FnMut() -> [u8; 16] + Send>,
}

/// The delay before reconnect attempt `attempt` (0 for the first).
#[must_use]
pub fn reconnect_delay(attempt: u32) -> Duration {
    RECONNECT_FIRST
        .saturating_mul(1_u32 << attempt.min(16))
        .min(RECONNECT_CEILING)
}

/// The reconnect attempt after a session that lived `lived`: back to the
/// first delay only when it stayed up a whole ceiling, so a daemon that
/// accepts and closes at once is retried on the growing backoff, not at
/// 250 ms forever.
#[must_use]
pub fn attempt_after(attempt: u32, lived: Duration) -> u32 {
    if lived >= RECONNECT_CEILING {
        0
    } else {
        attempt
    }
}

struct Bridge<B: DataSessionBinding> {
    binding: B,
    config: Config,
    env: Env,
    state: BridgeState,
    session: Option<B::Session>,
    /// The profile's `PeerId`, as the last session's open reported it.
    local_peer: Option<TransportIdentity>,
    /// Re-joins the daemon refused at a reconnect, each until the next
    /// join or leave of that channel (LIFECYCLE.md step 6).
    rejoin_refused: BTreeMap<ChannelId, RejoinRefusal>,
    /// Why the last open failed, for `status`.
    last_open_error: Option<TransportError>,
    attempt: u32,
    reconnect_at: Instant,
    /// When the current session opened: the backoff resets only after one
    /// has stayed up a whole ceiling, so a daemon that accepts and closes
    /// is not retried at the first delay forever.
    opened_at: Instant,
    health: Option<(Health, Option<ConnectivitySummary>)>,
    /// Pull mode's queue, made at the first session with its granted
    /// event queue as the bound, and kept across reconnects so what the
    /// host has not taken survives the daemon going away. Full, it pauses
    /// the session's draining: nothing it took is ever dropped.
    pull: Option<Pull>,
}

/// Serve the host on `input` and `output` until `input` ends.
///
/// # Errors
/// Only an I/O error on `output` or `input`: every session failure is a
/// state the bridge keeps serving through.
pub async fn serve<B, R, W>(
    binding: B,
    config: Config,
    env: Env,
    input: R,
    mut output: W,
) -> std::io::Result<()>
where
    B: DataSessionBinding,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut bridge = Bridge {
        binding,
        config,
        env,
        state: BridgeState::new(),
        session: None,
        local_peer: None,
        rejoin_refused: BTreeMap::new(),
        last_open_error: None,
        attempt: 0,
        reconnect_at: Instant::now(),
        opened_at: Instant::now(),
        health: None,
        pull: None,
    };
    let mut lines = input.lines();
    loop {
        let connected = bridge.session.is_some();
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                if let Some(answer) = bridge.host_line(&line, &mut output).await? {
                    write_line(&mut output, &answer).await?;
                }
            }
            // Paused (pull mode, the queue full): the session is not
            // drained until `receive` makes room; the daemon's own rules
            // decide what it cannot take.
            woke = ready(bridge.session.as_ref()), if connected && !bridge.paused() => {
                if woke.is_ok() {
                    bridge.pump(&mut output).await?;
                } else {
                    bridge.lost();
                }
            }
            () = tokio::time::sleep_until(bridge.reconnect_at), if !connected => {
                bridge.open(&mut output).await?;
            }
        }
    }
}

async fn ready<S: DataSessionPort>(session: Option<&S>) -> Result<(), TransportError> {
    match session {
        Some(session) => session.ready().await,
        None => std::future::pending().await,
    }
}

async fn write_line<W: AsyncWrite + Unpin>(output: &mut W, line: &str) -> std::io::Result<()> {
    output.write_all(line.as_bytes()).await?;
    output.write_all(b"\n").await?;
    output.flush().await
}

/// What turns a session's events into the host's lines: the bridge's
/// state, its clock and entropy, the last health it heard, and the
/// writer. Borrowed apart from the session, so events can be taken and
/// written while a call on that session is in flight.
struct Emit<'a, W> {
    state: &'a mut BridgeState,
    env: &'a mut Env,
    health: &'a mut Option<(Health, Option<ConnectivitySummary>)>,
    out: &'a mut W,
    /// In pull mode, where each message goes instead of a push line.
    pull: Option<&'a mut Pull>,
}

impl<W> Emit<'_, W> {
    /// How many events to take from the session now: a batch, or in pull
    /// mode no more than the queue has room for, so nothing taken is ever
    /// refused by it. Zero is the paused state.
    fn room(&self) -> usize {
        self.pull
            .as_deref()
            .map_or(EVENT_BATCH, |pull| pull.room().min(EVENT_BATCH))
    }

    /// Whether the pull queue is full: the drain paused, and any call in
    /// flight to be cancelled rather than wait behind it.
    fn paused(&self) -> bool {
        self.pull.as_deref().is_some_and(Pull::is_full)
    }
}

impl<W: AsyncWrite + Unpin> Emit<'_, W> {
    async fn events(&mut self, events: Vec<SessionEvent>) -> std::io::Result<()> {
        for event in &events {
            if let SessionEvent::Local(notice) = event {
                self.state.observe(notice);
                if let LocalSessionEvent::ServerState {
                    health,
                    connectivity,
                } = notice
                {
                    *self.health = Some((*health, connectivity.clone()));
                }
                continue;
            }
            // Pull mode: queued as taken, never pushed -- one mode per
            // process; the token is minted when the host takes it.
            if let Some(pull) = self.pull.as_deref_mut() {
                let pulled = Pulled {
                    event: event.clone(),
                    lease: self.state.lease_held(),
                };
                // Never refused: no more is taken than the queue has room
                // for (`Emit::room`).
                if !pull.push(pulled, (self.env.now_ms)()) {
                    eprintln!(
                        "claude-channel: the pull queue refused an event taken past its room"
                    );
                }
                continue;
            }
            let entropy = (self.env.entropy)();
            match self.state.notification(event, entropy, (self.env.now_ms)()) {
                Ok(Some(n)) => match notification_line(&n) {
                    Ok(line) => write_line(self.out, &line).await?,
                    Err(e) => eprintln!("claude-channel: a notification did not serialize: {e}"),
                },
                Ok(None) => {}
                // Bridge-local: dropped and said, never forwarded in part
                // (CHANNEL-EVENT.md §Content).
                Err(e) => eprintln!("claude-channel: an inbound message was dropped: {e}"),
            }
        }
        Ok(())
    }
}

/// `call` on `session`, its events taken and written meanwhile. Once the
/// session reports its end the draining stops and the call is left to
/// answer with it.
///
/// `None` when the pull queue filled while the call was in flight: the
/// call is dropped, which sends LOCAL-IPC's cancel for it, since its
/// answer would otherwise wait behind events the bridge is no longer
/// taking (architect-cto's ruling, relay seq 18835). What the daemon did
/// with it before the cancel is not known here.
async fn drive<S, T, W>(
    session: &S,
    call: impl std::future::Future<Output = T>,
    emit: &mut Emit<'_, W>,
) -> std::io::Result<Option<T>>
where
    S: DataSessionPort,
    W: AsyncWrite + Unpin,
{
    tokio::pin!(call);
    let mut draining = true;
    loop {
        tokio::select! {
            biased;
            answer = &mut call => return Ok(Some(answer)),
            woke = session.ready(), if draining => {
                match woke {
                    Ok(()) => match session.events(emit.room()).await {
                        Ok(events) => {
                            emit.events(events).await?;
                            if emit.paused() {
                                return Ok(None);
                            }
                        }
                        Err(_) => draining = false,
                    },
                    Err(_) => draining = false,
                }
            }
        }
    }
}

/// Whether `session` has ended, asked of the session rather than read
/// from an error code: a binding reports the same end as
/// `BackendUnavailable`, `ShuttingDown`, `Timeout` or `ProtocolViolation`
/// by the way it ended. `events(0)` takes nothing on a live session and
/// answers the end on an ended one (`DataSessionPort::events`).
async fn ended<S: DataSessionPort>(session: &S) -> bool {
    session.events(0).await.is_err()
}

/// An open refused because of the endpoint, not the daemon: reported as
/// itself (TOOL-SURFACE.md §Tool results, LIFECYCLE.md §Endpoint conflict).
const fn endpoint_refusal(error: TransportError) -> bool {
    matches!(
        error,
        TransportError::EndpointInUse
            | TransportError::EndpointUnknown
            | TransportError::EndpointDisabled
            | TransportError::EndpointClientKindDenied
    )
}

impl<B: DataSessionBinding> Bridge<B> {
    /// Open a session: a fresh claim, then the bridge's joins taken
    /// again. A join the daemon refuses leaves the joins as a status row;
    /// a session that ends during the re-joins is a reconnect, the joins
    /// kept.
    async fn open<W: AsyncWrite + Unpin>(&mut self, out: &mut W) -> std::io::Result<()> {
        let request = SessionRequest::new(
            CLIENT_KIND,
            Some(self.config.endpoint.clone()),
            [DataCapability::Events, DataCapability::Commands],
        );
        let opened = match request {
            Ok(request) => self.binding.open(request).await,
            Err(_) => Err(TransportError::InvalidArgument),
        };
        let session = match opened {
            Ok(session) => session,
            Err(e) => {
                self.last_open_error = Some(e);
                self.schedule_reconnect();
                return Ok(());
            }
        };
        self.local_peer = Some(session.session().local_peer().clone());
        if self.config.delivery == Delivery::Pull && self.pull.is_none() {
            self.pull = Some(Pull {
                queue: PullQueue::new(session.session().event_queue()),
                paused_since: None,
            });
        }
        if let Some(lease) = session.session().endpoint_lease() {
            self.state
                .leased(lease.endpoint.clone(), lease.epoch.clone());
        }
        let joined: Vec<ChannelId> = self.state.joined_channels().cloned().collect();
        let Self {
            state,
            env,
            health,
            rejoin_refused,
            pull,
            ..
        } = self;
        let mut emit = Emit {
            state,
            env,
            health,
            out,
            pull: pull.as_mut(),
        };
        let mut died = false;
        for channel in joined {
            // The queue may be full here (an earlier re-join cancelled,
            // then the session ended): `drive` then takes nothing and
            // cancels at once.
            let refusal = match drive(&session, session.join(channel.clone()), &mut emit).await? {
                Some(Ok(())) => continue,
                // A session that ended refused nothing, whatever code its
                // end came with: the joins are kept for the next open
                // (`a_daemon_stopping_during_the_rejoin_keeps_the_join`).
                Some(Err(_)) if ended(&session).await => {
                    died = true;
                    break;
                }
                Some(Err(e)) => RejoinRefusal::Daemon(e),
                None => RejoinRefusal::PullQueueFull,
            };
            emit.state.left(&channel);
            rejoin_refused.insert(channel, refusal);
        }
        if died {
            self.state.lease_lost();
            self.health = None;
            self.last_open_error = Some(TransportError::BackendUnavailable);
            self.schedule_reconnect();
            return Ok(());
        }
        self.session = Some(session);
        self.last_open_error = None;
        self.opened_at = Instant::now();
        Ok(())
    }

    /// The session ended: its lease with it, and a fresh one is sought.
    fn lost(&mut self) {
        self.session = None;
        self.state.lease_lost();
        self.health = None;
        self.attempt = attempt_after(self.attempt, self.opened_at.elapsed());
        self.schedule_reconnect();
    }

    fn schedule_reconnect(&mut self) {
        self.reconnect_at = Instant::now() + reconnect_delay(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
    }

    /// Take what waits and write it as notifications.
    async fn pump<W: AsyncWrite + Unpin>(&mut self, out: &mut W) -> std::io::Result<()> {
        let Some(session) = self.session.as_ref() else {
            return Ok(());
        };
        let room = self
            .pull
            .as_ref()
            .map_or(EVENT_BATCH, |pull| pull.room().min(EVENT_BATCH));
        if room == 0 {
            return Ok(());
        }
        let Ok(events) = session.events(room).await else {
            self.lost();
            return Ok(());
        };
        let Self {
            state,
            env,
            health,
            pull,
            ..
        } = self;
        Emit {
            state,
            env,
            health,
            out,
            pull: pull.as_mut(),
        }
        .events(events)
        .await
    }

    /// The answer to one line from the host, if it is owed one.
    async fn host_line<W: AsyncWrite + Unpin>(
        &mut self,
        line: &str,
        out: &mut W,
    ) -> std::io::Result<Option<String>> {
        Ok(match parse_line(line) {
            Incoming::Request { id, method, params } => {
                match protocol_answer(&id, &method, self.config.delivery) {
                    Some(answer) => Some(answer),
                    None => Some(self.tool_call(&id, params, out).await?),
                }
            }
            Incoming::Notification { .. } | Incoming::Malformed => None,
        })
    }

    async fn tool_call<W: AsyncWrite + Unpin>(
        &mut self,
        id: &Value,
        params: Option<Value>,
        out: &mut W,
    ) -> std::io::Result<String> {
        let params = params.unwrap_or(Value::Null);
        let Some(tool) = params
            .get("name")
            .and_then(Value::as_str)
            .and_then(|name| self.config.delivery.tool(name))
        else {
            // The method exists; the tool named does not: invalid params,
            // as MCP's tools/call answers an unknown tool.
            return Ok(error_line(id, INVALID_PARAMS, "unknown tool"));
        };
        let call = match parse_call(tool, params.get("arguments").cloned()) {
            Ok(call) => call,
            Err(e) => return Ok(error_line(id, INVALID_PARAMS, &e.to_string())),
        };
        // Paused, a tool that needs the session would wait behind events
        // the bridge is not taking: refused at once instead.
        if self.paused()
            && matches!(
                call,
                ToolCall::Send { .. }
                    | ToolCall::Reply { .. }
                    | ToolCall::Broadcast { .. }
                    | ToolCall::Join(_)
                    | ToolCall::Leave(_)
            )
        {
            return Ok(tool_result_line(id, PULL_QUEUE_FULL, true));
        }
        Ok(match self.run(call, out).await? {
            Some(Ok(text)) => tool_result_line(id, &text, false),
            // A session that ended is noticed where it ends: `ready`
            // resolves once it has, and the loop reconnects from there.
            Some(Err(e)) => tool_result_line(id, &error_text(e), true),
            // Cancelled in flight as the queue filled (`drive`).
            None => tool_result_line(id, PULL_QUEUE_FULL_CANCELLED, true),
        })
    }

    /// Why there is no session to call: the endpoint refusal itself when
    /// that was it, the daemon's absence otherwise.
    fn absent(&self) -> TransportError {
        match self.last_open_error {
            Some(e) if endpoint_refusal(e) => e,
            _ => TransportError::BackendUnavailable,
        }
    }

    async fn run<W: AsyncWrite + Unpin>(
        &mut self,
        call: ToolCall,
        out: &mut W,
    ) -> std::io::Result<Option<Result<String, TransportError>>> {
        let absent = self.absent();
        let now = (self.env.now_ms)();
        let message_id = MessageId::from_bytes((self.env.entropy)());
        // The route a reply resolves to, decided before any call.
        let call = match call {
            ToolCall::Identity => return Ok(Some(Ok(self.identity()))),
            ToolCall::Status => return Ok(Some(Ok(self.status()))),
            // From the queue, with or without a session: what was taken
            // before the daemon went away is still the host's.
            ToolCall::Receive { max } => return Ok(Some(Ok(self.receive(max)))),
            ToolCall::Reply {
                reply_token,
                payload,
            } => match self.state.reply_route(&reply_token, now) {
                Ok(ReplyRoute::Direct {
                    remote_peer,
                    remote_endpoint,
                    ..
                }) => ToolCall::Send {
                    destination: DirectDestination {
                        peer: remote_peer,
                        endpoint: Some(remote_endpoint),
                    },
                    payload,
                },
                Ok(ReplyRoute::Broadcast { channel }) => ToolCall::Broadcast { channel, payload },
                Err(e) => return Ok(Some(Err(e))),
            },
            other => other,
        };
        let Self {
            session,
            state,
            env,
            health,
            rejoin_refused,
            pull,
            ..
        } = self;
        let Some(session) = session.as_ref() else {
            if let ToolCall::Leave(channel) = &call {
                state.left(channel);
                rejoin_refused.remove(channel);
                return Ok(Some(Ok(format!("left {}", channel.as_str()))));
            }
            return Ok(Some(Err(absent)));
        };
        let mut emit = Emit {
            state,
            env,
            health,
            out,
            pull: pull.as_mut(),
        };
        Ok(Some(match call {
            ToolCall::Join(channel) => {
                let Some(joined) = drive(session, session.join(channel.clone()), &mut emit).await?
                else {
                    return Ok(None);
                };
                rejoin_refused.remove(&channel);
                joined.map(|()| {
                    emit.state.joined(channel.clone());
                    format!("joined {}", channel.as_str())
                })
            }
            ToolCall::Leave(channel) => {
                // Cancelled, the bridge keeps the join it holds: the host
                // may leave again once it has taken.
                let Some(left) = drive(session, session.leave(channel.clone()), &mut emit).await?
                else {
                    return Ok(None);
                };
                // A live daemon that refused the leave still holds the join,
                // so the bridge keeps it. Taken, or the session ended and
                // took the join with it: left, as the no-session branch
                // answers, and the next open does not re-take it
                // (`a_leave_whose_session_ends_is_left_and_not_retaken`).
                let refused = match left {
                    Ok(()) => None,
                    Err(e) => (!ended(session).await).then_some(e),
                };
                if let Some(e) = refused {
                    Err(e)
                } else {
                    emit.state.left(&channel);
                    rejoin_refused.remove(&channel);
                    Ok(format!("left {}", channel.as_str()))
                }
            }
            ToolCall::Broadcast { channel, payload } => {
                let message = BroadcastMessageV1 {
                    message_id,
                    sent_at_ms: now,
                    payload,
                };
                let Some(sent) =
                    drive(session, session.broadcast(channel, message), &mut emit).await?
                else {
                    return Ok(None);
                };
                sent.map(|()| BROADCAST_ACCEPTED.to_owned())
            }
            ToolCall::Send {
                destination,
                payload,
            } => {
                let Some(sent) = drive(
                    session,
                    session.send_direct(destination, message_id, payload),
                    &mut emit,
                )
                .await?
                else {
                    return Ok(None);
                };
                sent.map(|accepted| direct_accepted(&accepted))
            }
            ToolCall::Identity
            | ToolCall::Status
            | ToolCall::Reply { .. }
            | ToolCall::Receive { .. } => {
                unreachable!("answered above")
            }
        }))
    }

    /// Whether pull mode's queue is full, so the session's draining is
    /// paused.
    fn paused(&self) -> bool {
        self.pull.as_ref().is_some_and(Pull::is_full)
    }

    /// `receive(max)`: at most `max` messages from the queue -- `max`
    /// defaulting to and clamped at its bound, never refused -- each
    /// minted now under the lease it arrived under. None queued yet (no
    /// session has opened) is an empty answer.
    fn receive(&mut self, max: Option<u64>) -> String {
        let take = match self.pull.as_mut() {
            Some(pull) => {
                let bound = pull.queue.bound();
                let max = max.map_or(bound, |m| usize::try_from(m).unwrap_or(usize::MAX));
                pull.take(max)
            }
            None => interweave_claude_channel_core::Take {
                items: Vec::new(),
                remaining: 0,
            },
        };
        let mut taken: Vec<(&'static str, ChannelNotification)> = Vec::new();
        for pulled in take.items {
            let kind = match pulled.event {
                SessionEvent::Broadcast(_) => "broadcast",
                _ => "direct",
            };
            let entropy = (self.env.entropy)();
            match self.state.notification_under(
                &pulled.event,
                pulled.lease.as_ref(),
                entropy,
                (self.env.now_ms)(),
            ) {
                Ok(Some(notification)) => taken.push((kind, notification)),
                Ok(None) => {}
                // Bridge-local, as on the push path: dropped and said,
                // never forwarded in part (CHANNEL-EVENT.md §Content).
                Err(e) => eprintln!("claude-channel: an inbound message was dropped: {e}"),
            }
        }
        let received = Received {
            events: taken
                .iter()
                .map(|(kind, notification)| ReceivedEvent {
                    kind,
                    content: &notification.content,
                    meta: &notification.meta,
                })
                .collect(),
            remaining: take.remaining,
            paused: self.paused(),
        };
        serde_json::to_string(&received).unwrap_or_default()
    }

    fn identity(&self) -> String {
        json!({
            "local_peer_id": self.local_peer.as_ref().map(TransportIdentity::as_str),
            "local_endpoint": self.config.endpoint.as_str(),
        })
        .to_string()
    }

    fn status(&mut self) -> String {
        let now = (self.env.now_ms)();
        let (lease_state, epoch) = match (self.session.is_some(), self.state.lease()) {
            (_, Some((_, epoch))) => ("held", Some(epoch.as_str().to_owned())),
            (true, None) => ("not held", None),
            (false, None) if endpoint_refusal(self.absent()) => ("endpoint refused", None),
            (false, None) => ("daemon unavailable", None),
        };
        let joined: Vec<&str> = self
            .state
            .joined_channels()
            .map(ChannelId::as_str)
            .collect();
        let desired = match &self.config.desired_channels {
            Ok(channels) => json!({
                "as_configured": channels.iter().map(ChannelId::as_str).collect::<Vec<_>>()
            }),
            Err(class) => json!({"unknown": class}),
        };
        let refused: Vec<Value> = self
            .rejoin_refused
            .iter()
            .map(
                |(channel, refusal)| json!({"channel": channel.as_str(), "error": refusal.label()}),
            )
            .collect();
        let (health, connectivity) = match &self.health {
            Some((health, connectivity)) => (
                serde_json::to_value(health).unwrap_or(Value::Null),
                connectivity
                    .as_ref()
                    .and_then(|c| serde_json::to_value(c).ok())
                    .unwrap_or(Value::Null),
            ),
            None => (Value::Null, Value::Null),
        };
        let mut status = json!({
            "local_peer_id": self.local_peer.as_ref().map(TransportIdentity::as_str),
            "local_endpoint": self.config.endpoint.as_str(),
            "endpoint_lease_state": lease_state,
            "endpoint_lease_epoch": epoch,
            "joined_channels": joined,
            "profile_desired_channels": desired,
            "rejoin_refused": refused,
            "transport_health": health,
            "connectivity": connectivity,
            "last_connect_error": self.last_open_error.map(|e| format!("{e:?}")),
            "reply_tokens": self.state.live_tokens(now),
        });
        // Pull mode: what waits for `receive`, whether the drain is
        // paused for it, and since when.
        if self.config.delivery == Delivery::Pull {
            status["pull_queue"] = json!({
                "depth": self.pull.as_ref().map_or(0, |pull| pull.queue.depth()),
                "paused": self.paused(),
                "paused_since": self.pull.as_ref().and_then(|pull| pull.paused_since),
            });
        }
        status.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reconnect_backoff_doubles_to_its_ceiling() {
        assert_eq!(reconnect_delay(0), Duration::from_millis(250));
        assert_eq!(reconnect_delay(1), Duration::from_millis(500));
        assert_eq!(reconnect_delay(6), Duration::from_secs(16));
        assert_eq!(reconnect_delay(7), RECONNECT_CEILING, "32 s capped at 30");
        assert_eq!(reconnect_delay(u32::MAX), RECONNECT_CEILING);
    }

    /// A session that closed at once keeps the backoff growing; one that
    /// lived a whole ceiling resets it.
    #[test]
    fn the_backoff_resets_only_after_a_session_that_lasted() {
        assert_eq!(attempt_after(5, Duration::from_millis(10)), 5);
        assert_eq!(attempt_after(5, Duration::from_millis(29_999)), 5);
        assert_eq!(attempt_after(5, RECONNECT_CEILING), 0);
    }
}
