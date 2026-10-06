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
    BROADCAST_ACCEPTED, BridgeState, ReplyRoute, ToolCall, ToolName, direct_accepted, error_text,
    parse_call,
};
use interweave_local_client_api::{
    DataCapability, DataSessionBinding, DataSessionPort, LocalSessionEvent, SessionEvent,
    SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, ConnectivitySummary, DirectDestination, EndpointId, Health,
    MessageId, TransportError, TransportIdentity,
};
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
    rejoin_refused: BTreeMap<ChannelId, TransportError>,
    /// Why the last open failed, for `status`.
    last_open_error: Option<TransportError>,
    attempt: u32,
    reconnect_at: Instant,
    /// When the current session opened: the backoff resets only after one
    /// has stayed up a whole ceiling, so a daemon that accepts and closes
    /// is not retried at the first delay forever.
    opened_at: Instant,
    health: Option<(Health, Option<ConnectivitySummary>)>,
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
            woke = ready(bridge.session.as_ref()), if connected => {
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
async fn drive<S, T, W>(
    session: &S,
    call: impl std::future::Future<Output = T>,
    emit: &mut Emit<'_, W>,
) -> std::io::Result<T>
where
    S: DataSessionPort,
    W: AsyncWrite + Unpin,
{
    tokio::pin!(call);
    let mut draining = true;
    loop {
        tokio::select! {
            biased;
            answer = &mut call => return Ok(answer),
            woke = session.ready(), if draining => {
                match woke {
                    Ok(()) => match session.events(EVENT_BATCH).await {
                        Ok(events) => emit.events(events).await?,
                        Err(_) => draining = false,
                    },
                    Err(_) => draining = false,
                }
            }
        }
    }
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
            ..
        } = self;
        let mut emit = Emit {
            state,
            env,
            health,
            out,
        };
        let mut died = false;
        for channel in joined {
            match drive(&session, session.join(channel.clone()), &mut emit).await? {
                Ok(()) => {}
                // The session died; the daemon refused nothing, so the
                // joins are kept for the next open.
                Err(TransportError::BackendUnavailable) => {
                    died = true;
                    break;
                }
                Err(e) => {
                    emit.state.left(&channel);
                    rejoin_refused.insert(channel, e);
                }
            }
        }
        if died {
            self.state.lease_lost();
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
        let Ok(events) = session.events(EVENT_BATCH).await else {
            self.lost();
            return Ok(());
        };
        let Self {
            state, env, health, ..
        } = self;
        Emit {
            state,
            env,
            health,
            out,
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
            Incoming::Request { id, method, params } => match protocol_answer(&id, &method) {
                Some(answer) => Some(answer),
                None => Some(self.tool_call(&id, params, out).await?),
            },
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
            .and_then(ToolName::parse)
        else {
            // The method exists; the tool named does not: invalid params,
            // as MCP's tools/call answers an unknown tool.
            return Ok(error_line(id, INVALID_PARAMS, "unknown tool"));
        };
        let call = match parse_call(tool, params.get("arguments").cloned()) {
            Ok(call) => call,
            Err(e) => return Ok(error_line(id, INVALID_PARAMS, &e.to_string())),
        };
        Ok(match self.run(call, out).await? {
            Ok(text) => tool_result_line(id, &text, false),
            // A session that ended is noticed where it ends: `ready`
            // resolves once it has, and the loop reconnects from there.
            Err(e) => tool_result_line(id, &error_text(e), true),
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
    ) -> std::io::Result<Result<String, TransportError>> {
        let absent = self.absent();
        let now = (self.env.now_ms)();
        let message_id = MessageId::from_bytes((self.env.entropy)());
        // The route a reply resolves to, decided before any call.
        let call = match call {
            ToolCall::Identity => return Ok(Ok(self.identity())),
            ToolCall::Status => return Ok(Ok(self.status())),
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
                Err(e) => return Ok(Err(e)),
            },
            other => other,
        };
        let Self {
            session,
            state,
            env,
            health,
            rejoin_refused,
            ..
        } = self;
        let Some(session) = session.as_ref() else {
            if let ToolCall::Leave(channel) = &call {
                state.left(channel);
                rejoin_refused.remove(channel);
                return Ok(Ok(format!("left {}", channel.as_str())));
            }
            return Ok(Err(absent));
        };
        let mut emit = Emit {
            state,
            env,
            health,
            out,
        };
        Ok(match call {
            ToolCall::Join(channel) => {
                let joined = drive(session, session.join(channel.clone()), &mut emit).await?;
                rejoin_refused.remove(&channel);
                joined.map(|()| {
                    emit.state.joined(channel.clone());
                    format!("joined {}", channel.as_str())
                })
            }
            ToolCall::Leave(channel) => {
                let left = drive(session, session.leave(channel.clone()), &mut emit).await?;
                emit.state.left(&channel);
                rejoin_refused.remove(&channel);
                left.map(|()| format!("left {}", channel.as_str()))
            }
            ToolCall::Broadcast { channel, payload } => {
                let message = BroadcastMessageV1 {
                    message_id,
                    sent_at_ms: now,
                    payload,
                };
                drive(session, session.broadcast(channel, message), &mut emit)
                    .await?
                    .map(|()| BROADCAST_ACCEPTED.to_owned())
            }
            ToolCall::Send {
                destination,
                payload,
            } => drive(
                session,
                session.send_direct(destination, message_id, payload),
                &mut emit,
            )
            .await?
            .map(|accepted| direct_accepted(&accepted)),
            ToolCall::Identity | ToolCall::Status | ToolCall::Reply { .. } => {
                unreachable!("answered above")
            }
        })
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
            .map(|(channel, error)| json!({"channel": channel.as_str(), "error": format!("{error:?}")}))
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
        json!({
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
        })
        .to_string()
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
