// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The agent-facing tool surface (`plugin/TOOL-SURFACE.md`): seven tools in
//! either delivery mode, `receive` beside them in pull mode, their inputs
//! as closed shapes, and the exact wording of their results.
//!
//! **Nothing administrative.** [`Delivery::tools`] is the whole surface a
//! session sees; trust, endpoints, keys, configuration, shutdown and raw
//! network operations are not tools (§What is not a Claude tool). Every
//! input refuses an unknown field, so no argument -- a `source_endpoint`
//! least of all -- reaches the bridge beside the ones named: the source
//! of every direct send is the session's lease (§Bridge endpoint).
//!
//! **Results say what happened, never more.** A broadcast is "accepted for
//! local publish", not delivered; a direct send is accepted by the remote
//! transport at an endpoint, not processed by whoever is there
//! (§Tool results).

use interweave_transport_api::{
    ChannelId, DirectDestination, EndpointId, MAX_PAYLOAD_BYTES, MediaType, Payload,
    TransportError, TransportIdentity,
};
use serde::Deserialize;

/// A tool the bridge offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolName {
    /// Publish to a channel this bridge joined.
    Broadcast,
    /// Send to a trusted peer's endpoint, or its default.
    Send,
    /// Follow the exact route of an inbound message.
    Reply,
    /// Take a join on a channel.
    Join,
    /// Release a join.
    Leave,
    /// This profile's `PeerId` and this bridge's endpoint.
    Identity,
    /// Health, the lease, the joins.
    Status,
    /// Take what waits in the pull queue (pull mode only).
    Receive,
}

/// How inbound messages reach the host -- configuration, never detected
/// (`--delivery`, required): a host does not declare whether it takes
/// Claude Code's channel push, so one that does is named so by whoever
/// starts the bridge (architect-cto's ruling, relay seq 18691).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Each message pushed as `notifications/claude/channel`; the
    /// `claude/channel` capability advertised, no `receive` tool.
    Push,
    /// Each message queued for `receive`; no capability, no push.
    Pull,
}

impl Delivery {
    /// The `--delivery` value naming each mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Pull => "pull",
        }
    }

    /// The mode `value` names, if any.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Push, Self::Pull]
            .into_iter()
            .find(|mode| mode.as_str() == value)
    }

    /// The tools a session in this mode sees, in TOOL-SURFACE.md's order:
    /// `receive` only in pull mode, so a session never has both ways of
    /// taking a message.
    #[must_use]
    pub const fn tools(self) -> &'static [ToolName] {
        match self {
            Self::Push => &ToolName::ALL,
            Self::Pull => &ToolName::PULL,
        }
    }

    /// The tool called `name` in this mode, if it offers one.
    #[must_use]
    pub fn tool(self, name: &str) -> Option<ToolName> {
        self.tools()
            .iter()
            .copied()
            .find(|tool| tool.as_str() == name)
    }
}

impl ToolName {
    /// The seven tools of either mode, TOOL-SURFACE.md's table in its
    /// order.
    pub const ALL: [Self; 7] = [
        Self::Broadcast,
        Self::Send,
        Self::Reply,
        Self::Join,
        Self::Leave,
        Self::Identity,
        Self::Status,
    ];

    /// Pull mode's surface: the seven, and `receive`.
    pub const PULL: [Self; 8] = [
        Self::Broadcast,
        Self::Send,
        Self::Reply,
        Self::Join,
        Self::Leave,
        Self::Identity,
        Self::Status,
        Self::Receive,
    ];

    /// The tool's name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Broadcast => "broadcast",
            Self::Send => "send",
            Self::Reply => "reply",
            Self::Join => "join",
            Self::Leave => "leave",
            Self::Identity => "identity",
            Self::Status => "status",
            Self::Receive => "receive",
        }
    }
}

/// A tool call, its arguments validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    /// `broadcast(channel, content, content_type?)`.
    Broadcast {
        /// The channel.
        channel: ChannelId,
        /// The content and its media type.
        payload: Payload,
    },
    /// `send(peer, endpoint?, content, content_type?)`.
    Send {
        /// The peer and the endpoint, or its default.
        destination: DirectDestination,
        /// The content and its media type.
        payload: Payload,
    },
    /// `reply(reply_token, content, content_type?)`.
    Reply {
        /// The token of the message replied to.
        reply_token: String,
        /// The content and its media type.
        payload: Payload,
    },
    /// `join(channel)`.
    Join(ChannelId),
    /// `leave(channel)`.
    Leave(ChannelId),
    /// `identity()`.
    Identity,
    /// `status()`.
    Status,
    /// `receive(max?)`: at most `max` events, or as many as the queue's
    /// bound; a larger ask is clamped by the bridge, never refused.
    Receive {
        /// The most to take, if named.
        max: Option<u64>,
    },
}

/// Why a call's arguments were refused, as the tool's error text says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInputError(pub String);

impl core::fmt::Display for ToolInputError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for ToolInputError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BroadcastInput {
    channel: String,
    content: String,
    #[serde(default)]
    content_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendInput {
    peer: String,
    #[serde(default)]
    endpoint: Option<String>,
    content: String,
    #[serde(default)]
    content_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyInput {
    reply_token: String,
    content: String,
    #[serde(default)]
    content_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelInput {
    channel: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiveInput {
    #[serde(default)]
    max: Option<u64>,
}

/// `arguments` for `tool` as a [`ToolCall`]; absent arguments are an
/// empty object.
///
/// # Errors
/// [`ToolInputError`] for an unknown field or a missing one (each named),
/// a value of the wrong type (named by the tool only: serde reports no
/// field for it), an identifier or media type its grammar refuses, or
/// content past the transport's payload ceiling (each named).
pub fn parse_call(
    tool: ToolName,
    arguments: Option<serde_json::Value>,
) -> Result<ToolCall, ToolInputError> {
    let arguments =
        arguments.unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::default()));
    let shape = |e: serde_json::Error| ToolInputError(format!("{}: {e}", tool.as_str()));
    Ok(match tool {
        ToolName::Broadcast => {
            let input: BroadcastInput = serde_json::from_value(arguments).map_err(shape)?;
            ToolCall::Broadcast {
                channel: channel(&input.channel)?,
                payload: payload(input.content, input.content_type)?,
            }
        }
        ToolName::Send => {
            let input: SendInput = serde_json::from_value(arguments).map_err(shape)?;
            let peer = TransportIdentity::parse(input.peer)
                .map_err(|e| ToolInputError(format!("peer: {e}")))?;
            let endpoint = input
                .endpoint
                .map(|e| EndpointId::parse(e).map_err(|e| ToolInputError(format!("endpoint: {e}"))))
                .transpose()?;
            ToolCall::Send {
                destination: DirectDestination { peer, endpoint },
                payload: payload(input.content, input.content_type)?,
            }
        }
        ToolName::Reply => {
            let input: ReplyInput = serde_json::from_value(arguments).map_err(shape)?;
            ToolCall::Reply {
                reply_token: input.reply_token,
                payload: payload(input.content, input.content_type)?,
            }
        }
        ToolName::Join => {
            let input: ChannelInput = serde_json::from_value(arguments).map_err(shape)?;
            ToolCall::Join(channel(&input.channel)?)
        }
        ToolName::Leave => {
            let input: ChannelInput = serde_json::from_value(arguments).map_err(shape)?;
            ToolCall::Leave(channel(&input.channel)?)
        }
        ToolName::Identity => {
            let NoInput {} = serde_json::from_value(arguments).map_err(shape)?;
            ToolCall::Identity
        }
        ToolName::Status => {
            let NoInput {} = serde_json::from_value(arguments).map_err(shape)?;
            ToolCall::Status
        }
        ToolName::Receive => {
            let input: ReceiveInput = serde_json::from_value(arguments).map_err(shape)?;
            ToolCall::Receive { max: input.max }
        }
    })
}

fn channel(value: &str) -> Result<ChannelId, ToolInputError> {
    ChannelId::parse(value).map_err(|e| ToolInputError(format!("channel: {e}")))
}

/// The content as UTF-8 bytes under the transport's ceiling; the
/// daemon's effective limit, which may be lower, is the daemon's to apply.
fn payload(content: String, content_type: Option<String>) -> Result<Payload, ToolInputError> {
    let media_type = content_type
        .map(|m| MediaType::parse(m).map_err(|e| ToolInputError(format!("content_type: {e}"))))
        .transpose()?;
    Payload::new(media_type, content.into_bytes(), MAX_PAYLOAD_BYTES)
        .map_err(|e| ToolInputError(format!("content: {e}")))
}

/// A broadcast's result: local acceptance, never delivery.
pub const BROADCAST_ACCEPTED: &str = "accepted for local publish";

/// A direct send's or reply's result: the remote transport's bounded queue
/// accepted it at `endpoint`, which is not anyone having processed it.
#[must_use]
pub fn direct_accepted(endpoint: &EndpointId) -> String {
    format!(
        "remote transport accepted at endpoint {}",
        endpoint.as_str()
    )
}

/// A refused network tool's error text: the error's name, explicit, and
/// for the coarse remote one nothing it does not know
/// (`RemoteEndpointUnavailable` never says whether the endpoint exists).
#[must_use]
pub fn error_text(error: TransportError) -> String {
    let detail = match error {
        TransportError::RemoteEndpointUnavailable => {
            "the remote transport did not accept it at an endpoint"
        }
        TransportError::UnauthorizedPeer => "the peer is not trusted by this profile",
        TransportError::ChannelNotJoined => "this bridge has not joined that channel",
        TransportError::EndpointNotRegistered => "this bridge holds no endpoint lease",
        TransportError::EndpointInUse => "the configured endpoint is leased by another client",
        TransportError::EndpointUnknown
        | TransportError::EndpointDisabled
        | TransportError::EndpointClientKindDenied => {
            "the configured endpoint cannot be leased by this bridge"
        }
        TransportError::Overloaded => "the transport is overloaded; nothing was sent",
        TransportError::BackendUnavailable => "the transport daemon is unavailable",
        TransportError::InvalidArgument => {
            "an argument is invalid, or the reply token is unknown, expired or from an earlier lease"
        }
        _ => "the transport refused it",
    };
    format!("{error:?}: {detail}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use serde_json::json;

    const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    /// TOOL-SURFACE.md's table, exactly: seven tools and no other.
    #[test]
    fn the_surface_is_the_seven_tools() {
        assert_eq!(
            ToolName::ALL.map(ToolName::as_str),
            [
                "broadcast",
                "send",
                "reply",
                "join",
                "leave",
                "identity",
                "status"
            ]
        );
        for tool in ToolName::ALL {
            assert_eq!(Delivery::Push.tool(tool.as_str()), Some(tool));
        }
        assert_eq!(Delivery::Pull.tool("shutdown"), None);
    }

    /// Each mode's surface: push is the seven, pull is the seven and
    /// `receive` -- so a push session never sees `receive`, and a pull
    /// session sees it once.
    #[test]
    fn receive_is_on_the_pull_surface_only() {
        assert_eq!(Delivery::Push.tools(), &ToolName::ALL[..]);
        assert_eq!(Delivery::Push.tool("receive"), None);
        assert_eq!(Delivery::Pull.tool("receive"), Some(ToolName::Receive));
        assert_eq!(
            Delivery::Pull.tools()[..7],
            ToolName::ALL[..],
            "the seven, in order, first"
        );
        assert_eq!(Delivery::Pull.tools().len(), 8);
        assert_eq!(Delivery::parse("push"), Some(Delivery::Push));
        assert_eq!(Delivery::parse("pull"), Some(Delivery::Pull));
        assert_eq!(Delivery::parse("auto"), None);
    }

    /// `receive`'s one argument is an optional count; nothing else.
    #[test]
    fn receive_takes_an_optional_count() {
        assert_eq!(
            parse_call(ToolName::Receive, None),
            Ok(ToolCall::Receive { max: None })
        );
        assert_eq!(
            parse_call(ToolName::Receive, Some(json!({"max": 5}))),
            Ok(ToolCall::Receive { max: Some(5) })
        );
        for bad in [json!({"max": -1}), json!({"max": "5"}), json!({"limit": 5})] {
            assert!(
                parse_call(ToolName::Receive, Some(bad.clone())).is_err(),
                "{bad}"
            );
        }
    }

    /// No call takes a source endpoint, or any field beside its own: the
    /// lease is the source of every direct send.
    #[test]
    fn a_source_endpoint_argument_is_refused_on_every_tool() {
        let base = |tool| match tool {
            ToolName::Broadcast => json!({"channel": "general", "content": "x"}),
            ToolName::Send => json!({"peer": PEER, "content": "x"}),
            ToolName::Reply => json!({"reply_token": "t", "content": "x"}),
            ToolName::Join | ToolName::Leave => json!({"channel": "general"}),
            ToolName::Identity | ToolName::Status | ToolName::Receive => json!({}),
        };
        for tool in ToolName::PULL {
            let mut arguments = base(tool);
            assert!(
                parse_call(tool, Some(arguments.clone())).is_ok(),
                "the control: {tool:?} without it"
            );
            arguments["source_endpoint"] = json!("claude");
            let refused = parse_call(tool, Some(arguments)).expect_err("refused");
            assert!(refused.0.contains("source_endpoint"), "{tool:?}: {refused}");
        }
    }

    #[test]
    fn send_without_an_endpoint_asks_for_the_remote_default() {
        let call = parse_call(
            ToolName::Send,
            Some(json!({"peer": PEER, "content": "hello"})),
        )
        .expect("ok");
        let ToolCall::Send { destination, .. } = call else {
            unreachable!("a send")
        };
        assert_eq!(destination.endpoint, None);
        let call = parse_call(
            ToolName::Send,
            Some(json!({"peer": PEER, "endpoint": "human", "content": "hello"})),
        )
        .expect("ok");
        let ToolCall::Send { destination, .. } = call else {
            unreachable!("a send")
        };
        assert_eq!(
            destination.endpoint.as_ref().map(EndpointId::as_str),
            Some("human")
        );
    }

    #[test]
    fn grammar_and_ceiling_refusals_name_the_field() {
        for (tool, arguments, field) in [
            (ToolName::Join, json!({"channel": ""}), "channel"),
            (
                ToolName::Send,
                json!({"peer": "nope", "content": "x"}),
                "peer",
            ),
            (
                ToolName::Send,
                json!({"peer": PEER, "endpoint": "", "content": "x"}),
                "endpoint",
            ),
            (
                ToolName::Broadcast,
                json!({"channel": "general", "content": "x", "content_type": "a\u{1}b"}),
                "content_type",
            ),
            (
                ToolName::Reply,
                json!({"reply_token": "t", "content": "x".repeat(MAX_PAYLOAD_BYTES + 1)}),
                "content",
            ),
            (ToolName::Reply, json!({"content": "x"}), "reply_token"),
        ] {
            let refused = parse_call(tool, Some(arguments)).expect_err("refused");
            assert!(refused.0.contains(field), "{tool:?}: {refused}");
        }
        assert!(
            parse_call(
                ToolName::Reply,
                Some(json!({"reply_token": "t", "content": "x".repeat(MAX_PAYLOAD_BYTES)}))
            )
            .is_ok(),
            "the control: at the ceiling"
        );
    }

    #[test]
    fn absent_arguments_are_an_empty_object() {
        assert_eq!(parse_call(ToolName::Status, None), Ok(ToolCall::Status));
        assert!(parse_call(ToolName::Join, None).is_err());
    }

    /// §Tool results' wording, exactly, and never a delivery claim.
    #[test]
    fn result_wording_is_the_contracts() {
        assert_eq!(BROADCAST_ACCEPTED, "accepted for local publish");
        assert_eq!(
            direct_accepted(&EndpointId::parse("human").expect("endpoint")),
            "remote transport accepted at endpoint human"
        );
        for error in [
            TransportError::RemoteEndpointUnavailable,
            TransportError::UnauthorizedPeer,
            TransportError::ChannelNotJoined,
            TransportError::EndpointNotRegistered,
            TransportError::EndpointInUse,
            TransportError::Overloaded,
        ] {
            let text = error_text(error);
            assert!(text.starts_with(&format!("{error:?}: ")), "{text}");
            for claim in ["delivered", "processed", "unknown endpoint", "offline"] {
                assert!(!text.contains(claim), "{text}");
            }
        }
    }
}
