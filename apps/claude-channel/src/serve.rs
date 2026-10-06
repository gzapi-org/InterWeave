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
//! `commands` only (`the_session_asks_events_and_commands_only`).
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
//! this loop alone, a line each, so they never interleave.

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
    MessageId, Payload, TransportError, TransportIdentity,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::time::Instant;

use crate::mcp::{
    INVALID_PARAMS, Incoming, METHOD_NOT_FOUND, error_line, notification_line, parse_line,
    protocol_answer, tool_result_line,
};

/// The client kind the bridge presents: a hygiene label, never authority
/// (ADR-0037).
pub const CLIENT_KIND: &str = "claude-channel";

/// Events taken from the session per wake. Bounded so one burst cannot
/// hold the loop away from the host's lines for long.
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
        health: None,
    };
    let mut lines = input.lines();
    loop {
        let connected = bridge.session.is_some();
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                for out in bridge.host_line(&line).await {
                    write_line(&mut output, &out).await?;
                }
            }
            woke = ready(bridge.session.as_ref()), if connected => {
                let out = if woke.is_ok() {
                    bridge.pump().await
                } else {
                    bridge.lost();
                    Vec::new()
                };
                for line in out {
                    write_line(&mut output, &line).await?;
                }
            }
            () = tokio::time::sleep_until(bridge.reconnect_at), if !connected => {
                bridge.open().await;
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

impl<B: DataSessionBinding> Bridge<B> {
    /// Open a session: a fresh claim, then the bridge's joins taken
    /// again. A join refused now is dropped from the bridge's joins, not
    /// retried forever.
    async fn open(&mut self) {
        let request = SessionRequest::new(
            CLIENT_KIND,
            Some(self.config.endpoint.clone()),
            [DataCapability::Events, DataCapability::Commands],
        );
        let opened = match request {
            Ok(request) => self.binding.open(request).await,
            Err(_) => Err(TransportError::InvalidArgument),
        };
        match opened {
            Ok(session) => {
                self.local_peer = Some(session.session().local_peer().clone());
                if let Some(lease) = session.session().endpoint_lease() {
                    self.state
                        .leased(lease.endpoint.clone(), lease.epoch.clone());
                }
                let joined: Vec<ChannelId> = self.state.joined_channels().cloned().collect();
                for channel in joined {
                    if let Err(e) = session.join(channel.clone()).await {
                        self.state.left(&channel);
                        self.rejoin_refused.insert(channel, e);
                    }
                }
                self.session = Some(session);
                self.last_open_error = None;
                self.attempt = 0;
            }
            Err(e) => {
                self.last_open_error = Some(e);
                self.schedule_reconnect();
            }
        }
    }

    /// The session ended: its lease with it, and a fresh one is sought.
    fn lost(&mut self) {
        self.session = None;
        self.state.lease_lost();
        self.health = None;
        self.schedule_reconnect();
    }

    fn schedule_reconnect(&mut self) {
        self.reconnect_at = Instant::now() + reconnect_delay(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
    }

    /// Take what waits and turn it into notifications.
    async fn pump(&mut self) -> Vec<String> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        let Ok(events) = session.events(EVENT_BATCH).await else {
            self.lost();
            return Vec::new();
        };
        let mut out = Vec::new();
        for event in &events {
            if let SessionEvent::Local(notice) = event {
                self.notice(notice);
                continue;
            }
            let entropy = (self.env.entropy)();
            match self.state.notification(event, entropy, (self.env.now_ms)()) {
                Ok(Some(n)) => match notification_line(&n) {
                    Ok(line) => out.push(line),
                    Err(e) => eprintln!("claude-channel: a notification did not serialize: {e}"),
                },
                Ok(None) => {}
                // Bridge-local: dropped and said, never forwarded in part
                // (CHANNEL-EVENT.md §Content).
                Err(e) => eprintln!("claude-channel: an inbound message was dropped: {e}"),
            }
        }
        out
    }

    fn notice(&mut self, notice: &LocalSessionEvent) {
        self.state.observe(notice);
        if let LocalSessionEvent::ServerState {
            health,
            connectivity,
        } = notice
        {
            self.health = Some((*health, connectivity.clone()));
        }
    }

    /// The answer to one line from the host, if it is owed one.
    async fn host_line(&mut self, line: &str) -> Vec<String> {
        match parse_line(line) {
            Incoming::Request { id, method, params } => {
                if let Some(answer) = protocol_answer(&id, &method) {
                    return vec![answer];
                }
                vec![self.tool_call(&id, params).await]
            }
            Incoming::Notification { .. } | Incoming::Malformed => Vec::new(),
        }
    }

    async fn tool_call(&mut self, id: &Value, params: Option<Value>) -> String {
        let params = params.unwrap_or(Value::Null);
        let Some(tool) = params
            .get("name")
            .and_then(Value::as_str)
            .and_then(ToolName::parse)
        else {
            return error_line(id, METHOD_NOT_FOUND, "no such tool");
        };
        let call = match parse_call(tool, params.get("arguments").cloned()) {
            Ok(call) => call,
            Err(e) => return error_line(id, INVALID_PARAMS, &e.to_string()),
        };
        match self.run(call).await {
            Ok(text) => tool_result_line(id, &text, false),
            Err(e) => {
                if e == TransportError::BackendUnavailable {
                    self.lost();
                }
                tool_result_line(id, &error_text(e), true)
            }
        }
    }

    async fn run(&mut self, call: ToolCall) -> Result<String, TransportError> {
        match call {
            ToolCall::Identity => Ok(self.identity()),
            ToolCall::Status => Ok(self.status()),
            ToolCall::Leave(channel) => {
                if let Some(session) = self.session.as_ref() {
                    session.leave(channel.clone()).await?;
                }
                self.state.left(&channel);
                self.rejoin_refused.remove(&channel);
                Ok(format!("left {}", channel.as_str()))
            }
            ToolCall::Join(channel) => {
                let joined = self.connected()?.join(channel.clone()).await;
                self.rejoin_refused.remove(&channel);
                joined?;
                self.state.joined(channel.clone());
                Ok(format!("joined {}", channel.as_str()))
            }
            ToolCall::Broadcast { channel, payload } => self.broadcast(channel, payload).await,
            ToolCall::Send {
                destination,
                payload,
            } => self.send(destination, payload).await,
            ToolCall::Reply {
                reply_token,
                payload,
            } => {
                let now = (self.env.now_ms)();
                match self.state.reply_route(&reply_token, now)? {
                    ReplyRoute::Direct {
                        remote_peer,
                        remote_endpoint,
                        ..
                    } => {
                        let destination = DirectDestination {
                            peer: remote_peer,
                            endpoint: Some(remote_endpoint),
                        };
                        self.send(destination, payload).await
                    }
                    ReplyRoute::Broadcast { channel } => self.broadcast(channel, payload).await,
                }
            }
        }
    }

    fn connected(&self) -> Result<&B::Session, TransportError> {
        self.session
            .as_ref()
            .ok_or(TransportError::BackendUnavailable)
    }

    async fn broadcast(
        &mut self,
        channel: ChannelId,
        payload: Payload,
    ) -> Result<String, TransportError> {
        let message = BroadcastMessageV1 {
            message_id: MessageId::from_bytes((self.env.entropy)()),
            sent_at_ms: (self.env.now_ms)(),
            payload,
        };
        self.connected()?.broadcast(channel, message).await?;
        Ok(BROADCAST_ACCEPTED.to_owned())
    }

    async fn send(
        &mut self,
        destination: DirectDestination,
        payload: Payload,
    ) -> Result<String, TransportError> {
        let message_id = MessageId::from_bytes((self.env.entropy)());
        let accepted = self
            .connected()?
            .send_direct(destination, message_id, payload)
            .await?;
        Ok(direct_accepted(&accepted))
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
}
