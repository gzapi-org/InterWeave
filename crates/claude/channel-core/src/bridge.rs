// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The bridge's state between the session and the channel: the lease it
//! holds, the channels it joined, and the reply tokens it minted -- and the
//! two things done with them: an inbound message becomes a
//! [`ChannelNotification`] carrying a fresh token, and a `reply` resolves a
//! token back to the exact route it was minted for.
//!
//! Pure: the clock and the token's entropy are the caller's
//! (`apps/claude-channel`), so every rule here is tested by value.

use std::collections::BTreeSet;

use interweave_local_client_api::{
    Generation, LocalSessionEvent, ReceivedBroadcast, ReceivedDirect, SessionEvent,
};
use interweave_transport_api::{ChannelId, EndpointId, TransportError, base64url};

use crate::content::{Undecodable, channel_content};
use crate::meta::{ChannelMeta, MetaError, MetaKey};
use crate::received_at::rfc3339_utc;
use crate::reply_token::{
    DEFAULT_MAX_TOKENS, DEFAULT_TTL_MS, DuplicateToken, ReplyResolution, ReplyRoute,
    ReplyTokenTable,
};

/// Bytes of entropy behind a reply token: 128 bits, so a token cannot be
/// guessed (CHANNEL-EVENT.md §Reply token).
pub const REPLY_TOKEN_ENTROPY_BYTES: usize = 16;

/// One `notifications/claude/channel`: the payload as `content`, the
/// routing facts as `meta`, never mixed (CHANNEL-EVENT.md §Sanitization).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelNotification {
    /// The notification's `content`.
    pub content: String,
    /// The notification's `meta`.
    pub meta: ChannelMeta,
}

/// Why an inbound message produced no notification. Each is a
/// bridge-local error the caller logs; the message is dropped, never
/// forwarded in part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertError {
    /// A `;ce=br` payload that did not decode within the cap.
    Content(Undecodable),
    /// A `meta` value the bridge's bound refused.
    Meta(MetaError),
    /// A receipt time RFC3339 cannot write.
    ReceivedAt {
        /// The receipt time, Unix-epoch milliseconds.
        ms: u64,
    },
    /// The entropy named a token already live: the caller's generator is
    /// broken, and the token is not reused.
    Token(DuplicateToken),
}

impl core::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Content(e) => e.fmt(f),
            Self::Meta(e) => e.fmt(f),
            Self::ReceivedAt { ms } => write!(f, "received_at {ms} ms is not writable as RFC3339"),
            Self::Token(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for ConvertError {}

impl From<MetaError> for ConvertError {
    fn from(e: MetaError) -> Self {
        Self::Meta(e)
    }
}

/// The bridge's view of its session.
#[derive(Debug)]
pub struct BridgeState {
    lease: Option<(EndpointId, Generation)>,
    joined: BTreeSet<ChannelId>,
    tokens: ReplyTokenTable,
}

impl Default for BridgeState {
    fn default() -> Self {
        Self::new()
    }
}

impl BridgeState {
    /// A bridge with no lease, no joins and no tokens; its tokens live
    /// 30 minutes, at most 2048 at once (CHANNEL-EVENT.md §Reply token).
    #[must_use]
    pub fn new() -> Self {
        Self {
            lease: None,
            joined: BTreeSet::new(),
            tokens: ReplyTokenTable::new(DEFAULT_TTL_MS, DEFAULT_MAX_TOKENS),
        }
    }

    /// The session's open granted `endpoint` at `epoch`: direct messages
    /// arrive for it and replies leave from it.
    pub fn leased(&mut self, endpoint: EndpointId, epoch: Generation) {
        self.lease = Some((endpoint, epoch));
    }

    /// The lease ended -- a revocation, or the session closed. Every direct
    /// token minted under it now resolves `StaleLease`; a reacquired lease
    /// has a new epoch and does not revive them (LIFECYCLE.md).
    pub fn lease_lost(&mut self) {
        self.lease = None;
    }

    /// The endpoint and epoch held, if any.
    #[must_use]
    pub fn lease(&self) -> Option<(&EndpointId, &Generation)> {
        self.lease
            .as_ref()
            .map(|(endpoint, epoch)| (endpoint, epoch))
    }

    /// `channel` was joined through this bridge.
    pub fn joined(&mut self, channel: ChannelId) {
        self.joined.insert(channel);
    }

    /// `channel` was left; a broadcast token naming it now resolves
    /// `ChannelNotJoined` (CHANNEL-EVENT.md §Reply token).
    pub fn left(&mut self, channel: &ChannelId) {
        self.joined.remove(channel);
    }

    /// The channels joined through this bridge, which `status` reports as
    /// `joined_channels`.
    pub fn joined_channels(&self) -> impl Iterator<Item = &ChannelId> {
        self.joined.iter()
    }

    /// A session notice the bridge's state follows: a revoked lease is
    /// lost. Whether it was one.
    pub fn observe(&mut self, notice: &LocalSessionEvent) -> bool {
        match notice {
            LocalSessionEvent::EndpointLeaseChanged { revoked_epoch, .. }
                if self
                    .lease
                    .as_ref()
                    .is_some_and(|(_, held)| held == revoked_epoch) =>
            {
                self.lease_lost();
                true
            }
            _ => false,
        }
    }

    /// `event` as a channel notification, with a reply token minted from
    /// `entropy`; `Ok(None)` for a session notice, which is the bridge's
    /// state and `status`, not the conversation's.
    ///
    /// A direct message mints its token only while the lease it arrived
    /// on is held, and the token names that epoch; without one, `meta`
    /// carries no `reply_token` and a reply is the explicit `send`.
    ///
    /// # Errors
    /// [`ConvertError`], with nothing minted: the content is converted
    /// before the token, so a dropped message leaves no route behind.
    pub fn notification(
        &mut self,
        event: &SessionEvent,
        entropy: [u8; REPLY_TOKEN_ENTROPY_BYTES],
        now_ms: u64,
    ) -> Result<Option<ChannelNotification>, ConvertError> {
        let lease = self.lease.clone();
        self.notification_under(event, lease.as_ref(), entropy, now_ms)
    }

    /// [`BridgeState::notification`] for a message taken from the session
    /// under `lease` -- pull mode mints its token when the host TAKES it,
    /// with the lease it ARRIVED under, so a message from before a
    /// reconnect gets a token stale by its epoch, as one pushed then
    /// would have (architect-cto's ruling, relay seq 18784).
    ///
    /// # Errors
    /// As [`BridgeState::notification`].
    pub fn notification_under(
        &mut self,
        event: &SessionEvent,
        lease: Option<&(EndpointId, Generation)>,
        entropy: [u8; REPLY_TOKEN_ENTROPY_BYTES],
        now_ms: u64,
    ) -> Result<Option<ChannelNotification>, ConvertError> {
        let (mut meta, content, route) = match event {
            SessionEvent::Direct(message) => Self::direct(message, lease)?,
            SessionEvent::Broadcast(message) => broadcast(message)?,
            SessionEvent::Local(_) => return Ok(None),
        };
        if let Some(route) = route {
            let token = base64url::encode(&entropy);
            meta.set(MetaKey::ReplyToken, token.clone())?;
            self.tokens
                .mint(token, route, now_ms)
                .map_err(ConvertError::Token)?;
        }
        Ok(Some(ChannelNotification { content, meta }))
    }

    /// The endpoint and epoch held, as a value to keep beside a message
    /// taken now and minted later.
    #[must_use]
    pub fn lease_held(&self) -> Option<(EndpointId, Generation)> {
        self.lease.clone()
    }

    /// A direct message's meta, content and reply route under `lease`.
    fn direct(
        message: &ReceivedDirect,
        lease: Option<&(EndpointId, Generation)>,
    ) -> Result<(ChannelMeta, String, Option<ReplyRoute>), ConvertError> {
        let mut meta = ChannelMeta::new();
        meta.set(MetaKey::DeliveryMode, "direct")?;
        meta.set(MetaKey::SourcePeer, message.source_peer.as_str())?;
        meta.set(MetaKey::SourceEndpoint, message.source_endpoint.as_str())?;
        meta.set(
            MetaKey::DestinationEndpoint,
            message.destination_endpoint.as_str(),
        )?;
        let content = common(
            &mut meta,
            &message.message_id.to_string(),
            message.received_at_ms,
            &message.payload,
        )?;
        let route = lease
            .filter(|(endpoint, _)| *endpoint == message.destination_endpoint)
            .map(|(endpoint, epoch)| ReplyRoute::Direct {
                remote_peer: message.source_peer.clone(),
                remote_endpoint: message.source_endpoint.clone(),
                local_endpoint: endpoint.clone(),
                local_lease_epoch: epoch.clone(),
            });
        Ok((meta, content, route))
    }

    /// Where a `reply` with `token` goes: the exact route it was minted
    /// for, never an adjacent one (TOOL-SURFACE.md §Reply semantics).
    ///
    /// # Errors
    /// `InvalidArgument` for an unknown, expired or stale-lease token, and
    /// `ChannelNotJoined` for a broadcast token whose channel was left.
    pub fn reply_route(&mut self, token: &str, now_ms: u64) -> Result<ReplyRoute, TransportError> {
        let epoch = self.lease.as_ref().map(|(_, epoch)| epoch);
        let joined = &self.joined;
        match self
            .tokens
            .resolve(token, epoch, &|channel| joined.contains(channel), now_ms)
        {
            ReplyResolution::Route(route) => Ok(route),
            other => Err(other.as_error().unwrap_or(TransportError::Internal)),
        }
    }

    /// Live reply tokens, after expiring those past their TTL.
    pub fn live_tokens(&mut self, now_ms: u64) -> usize {
        self.tokens.expire(now_ms);
        self.tokens.len()
    }
}

fn broadcast(
    message: &ReceivedBroadcast,
) -> Result<(ChannelMeta, String, Option<ReplyRoute>), ConvertError> {
    let mut meta = ChannelMeta::new();
    meta.set(MetaKey::DeliveryMode, "broadcast")?;
    meta.set(MetaKey::SourcePeer, message.source_peer.as_str())?;
    meta.set(MetaKey::Channel, message.channel.as_str())?;
    let content = common(
        &mut meta,
        &message.message_id.to_string(),
        message.received_at_ms,
        &message.payload,
    )?;
    let route = ReplyRoute::Broadcast {
        channel: message.channel.clone(),
    };
    Ok((meta, content, Some(route)))
}

/// The keys both modes carry, and the content.
fn common(
    meta: &mut ChannelMeta,
    message_id: &str,
    received_at_ms: u64,
    payload: &interweave_transport_api::Payload,
) -> Result<String, ConvertError> {
    let content = channel_content(payload).map_err(ConvertError::Content)?;
    meta.set(MetaKey::MessageId, message_id)?;
    let received_at =
        rfc3339_utc(received_at_ms).ok_or(ConvertError::ReceivedAt { ms: received_at_ms })?;
    meta.set(MetaKey::ReceivedAt, received_at)?;
    meta.set(MetaKey::PayloadEncoding, content.encoding.as_str())?;
    if let Some(content_type) = content.content_type {
        meta.set(MetaKey::ContentType, content_type)?;
    }
    Ok(content.text)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]
    use super::*;
    use interweave_transport_api::{
        MAX_PAYLOAD_BYTES, MediaType, MessageId, Payload, TransportIdentity,
    };

    const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const NOW: u64 = 1_791_227_222_497;

    fn endpoint(s: &str) -> EndpointId {
        EndpointId::parse(s).expect("endpoint")
    }
    fn channel(s: &str) -> ChannelId {
        ChannelId::parse(s).expect("channel")
    }
    fn epoch(s: &str) -> Generation {
        Generation::parse(s).expect("epoch")
    }
    fn payload(bytes: &[u8]) -> Payload {
        Payload::new(
            Some(MediaType::parse("text/plain").expect("type")),
            bytes.to_vec(),
            MAX_PAYLOAD_BYTES,
        )
        .expect("payload")
    }
    fn direct(to: &str) -> SessionEvent {
        SessionEvent::Direct(ReceivedDirect {
            source_peer: TransportIdentity::parse(PEER).expect("peer"),
            source_endpoint: endpoint("human"),
            destination_endpoint: endpoint(to),
            message_id: MessageId::parse_hex("000102030405060708090a0b0c0d0e0f").expect("id"),
            payload: payload(b"hello"),
            received_at_ms: NOW,
        })
    }
    fn broadcast(on: &str) -> SessionEvent {
        SessionEvent::Broadcast(ReceivedBroadcast {
            source_peer: TransportIdentity::parse(PEER).expect("peer"),
            channel: channel(on),
            message_id: MessageId::parse_hex("0f0e0d0c0b0a09080706050403020100").expect("id"),
            payload: payload(b"hi all"),
            received_at_ms: NOW,
        })
    }
    fn leased() -> BridgeState {
        let mut bridge = BridgeState::new();
        bridge.leased(endpoint("claude"), epoch("AAAAAAAAAAAAAAAAAAAAAQ"));
        bridge
    }

    /// The incoming-direct row of §19's required tests at the conversion:
    /// both endpoints, the peer, the id, the time, the encoding, the type
    /// and a token, in the table's order, and no `source`.
    #[test]
    fn a_direct_message_carries_both_endpoints_and_a_token_in_table_order() {
        let mut bridge = leased();
        let n = bridge
            .notification(&direct("claude"), [7; 16], NOW)
            .expect("converted")
            .expect("a notification");
        assert_eq!(n.content, "hello");
        let entries: Vec<_> = n.meta.entries().collect();
        assert_eq!(
            entries,
            [
                ("delivery_mode", "direct"),
                ("source_peer", PEER),
                ("source_endpoint", "human"),
                ("destination_endpoint", "claude"),
                ("message_id", "000102030405060708090a0b0c0d0e0f"),
                ("received_at", "2026-10-05T19:07:02.497Z"),
                ("reply_token", "BwcHBwcHBwcHBwcHBwcHBw"),
                ("payload_encoding", "utf8"),
                ("content_type", "text/plain"),
            ]
        );
    }

    #[test]
    fn a_broadcast_carries_its_channel_and_no_endpoint() {
        let mut bridge = BridgeState::new();
        let n = bridge
            .notification(&broadcast("general"), [1; 16], NOW)
            .expect("converted")
            .expect("a notification");
        assert_eq!(n.meta.get(MetaKey::DeliveryMode), Some("broadcast"));
        assert_eq!(n.meta.get(MetaKey::Channel), Some("general"));
        assert_eq!(n.meta.get(MetaKey::SourceEndpoint), None);
        assert_eq!(n.meta.get(MetaKey::DestinationEndpoint), None);
        assert!(n.meta.get(MetaKey::ReplyToken).is_some());
    }

    /// A direct token routes back exactly: the remote endpoint it came
    /// from, the local endpoint and epoch it arrived on.
    #[test]
    fn a_direct_token_resolves_to_its_exact_route() {
        let mut bridge = leased();
        let n = bridge
            .notification(&direct("claude"), [2; 16], NOW)
            .expect("ok")
            .expect("some");
        let token = n.meta.get(MetaKey::ReplyToken).expect("token").to_owned();
        assert_eq!(
            bridge.reply_route(&token, NOW + 1),
            Ok(ReplyRoute::Direct {
                remote_peer: TransportIdentity::parse(PEER).expect("peer"),
                remote_endpoint: endpoint("human"),
                local_endpoint: endpoint("claude"),
                local_lease_epoch: epoch("AAAAAAAAAAAAAAAAAAAAAQ"),
            })
        );
    }

    /// A lost lease makes every direct token stale, and a reacquired one
    /// -- a new epoch -- does not revive them; the control resolves first.
    #[test]
    fn a_stale_lease_token_fails_and_is_not_revived_by_a_new_lease() {
        let mut bridge = leased();
        let n = bridge
            .notification(&direct("claude"), [3; 16], NOW)
            .expect("ok")
            .expect("some");
        let token = n.meta.get(MetaKey::ReplyToken).expect("token").to_owned();
        assert!(bridge.reply_route(&token, NOW).is_ok(), "the control");
        assert!(bridge.observe(&LocalSessionEvent::EndpointLeaseChanged {
            endpoint: endpoint("claude"),
            revoked_epoch: epoch("AAAAAAAAAAAAAAAAAAAAAQ"),
        }));
        assert_eq!(
            bridge.reply_route(&token, NOW),
            Err(TransportError::InvalidArgument)
        );
        bridge.leased(endpoint("claude"), epoch("AAAAAAAAAAAAAAAAAAAAAg"));
        assert_eq!(
            bridge.reply_route(&token, NOW),
            Err(TransportError::InvalidArgument),
            "a new epoch does not revive it"
        );
    }

    /// A revocation of another epoch is not this lease's.
    #[test]
    fn a_revocation_of_another_epoch_leaves_the_lease() {
        let mut bridge = leased();
        assert!(!bridge.observe(&LocalSessionEvent::EndpointLeaseChanged {
            endpoint: endpoint("claude"),
            revoked_epoch: epoch("BBBBBBBBBBBBBBBBBBBBBB"),
        }));
        assert!(bridge.lease().is_some());
    }

    /// A broadcast token routes while the channel is joined and fails
    /// `ChannelNotJoined` once it is left: it never rejoins.
    #[test]
    fn a_broadcast_token_needs_the_channel_joined() {
        let mut bridge = BridgeState::new();
        bridge.joined(channel("general"));
        let n = bridge
            .notification(&broadcast("general"), [4; 16], NOW)
            .expect("ok")
            .expect("some");
        let token = n.meta.get(MetaKey::ReplyToken).expect("token").to_owned();
        assert_eq!(
            bridge.reply_route(&token, NOW),
            Ok(ReplyRoute::Broadcast {
                channel: channel("general")
            })
        );
        bridge.left(&channel("general"));
        assert_eq!(
            bridge.reply_route(&token, NOW),
            Err(TransportError::ChannelNotJoined)
        );
        assert_eq!(bridge.joined_channels().count(), 0, "nothing rejoined");
    }

    /// Without the lease the message arrived on there is no route to
    /// mint: no `reply_token`, and nothing in the table.
    #[test]
    fn a_direct_message_without_its_lease_mints_no_token() {
        let mut bridge = BridgeState::new();
        let n = bridge
            .notification(&direct("claude"), [5; 16], NOW)
            .expect("ok")
            .expect("some");
        assert_eq!(n.meta.get(MetaKey::ReplyToken), None);
        assert_eq!(bridge.live_tokens(NOW), 0);
        let mut other = leased();
        let n = other
            .notification(&direct("claude.secondary"), [5; 16], NOW)
            .expect("ok")
            .expect("some");
        assert_eq!(
            n.meta.get(MetaKey::ReplyToken),
            None,
            "nor for another endpoint than the one leased"
        );
    }

    /// An undecodable payload is dropped and leaves no token behind.
    #[test]
    fn a_dropped_message_mints_nothing() {
        let mut bridge = leased();
        let SessionEvent::Direct(mut message) = direct("claude") else {
            panic!("direct");
        };
        message.payload = Payload::new(
            Some(
                MediaType::parse("application/vnd.interweave-human-chat+json;v=2;ce=br")
                    .expect("type"),
            ),
            vec![0xff; 32],
            MAX_PAYLOAD_BYTES,
        )
        .expect("payload");
        assert!(matches!(
            bridge.notification(&SessionEvent::Direct(message), [6; 16], NOW),
            Err(ConvertError::Content(_))
        ));
        assert_eq!(bridge.live_tokens(NOW), 0);
    }

    /// The same entropy twice is a broken generator: refused, the first
    /// token's route kept.
    #[test]
    fn repeated_entropy_is_refused_and_the_first_route_kept() {
        let mut bridge = leased();
        bridge.joined(channel("general"));
        let first = bridge
            .notification(&broadcast("general"), [8; 16], NOW)
            .expect("ok")
            .expect("some");
        assert!(matches!(
            bridge.notification(&direct("claude"), [8; 16], NOW),
            Err(ConvertError::Token(_))
        ));
        let token = first.meta.get(MetaKey::ReplyToken).expect("token");
        assert!(matches!(
            bridge.reply_route(token, NOW),
            Ok(ReplyRoute::Broadcast { .. })
        ));
    }

    #[test]
    fn a_session_notice_is_no_notification() {
        let mut bridge = BridgeState::new();
        let notice = SessionEvent::Local(LocalSessionEvent::PeerDisconnected {
            peer: TransportIdentity::parse(PEER).expect("peer"),
            reason_class: "policy".into(),
        });
        assert_eq!(bridge.notification(&notice, [9; 16], NOW), Ok(None));
    }

    /// The contract's bounds, by value: 30 minutes, 2048 tokens
    /// (CHANNEL-EVENT.md §Reply token). The bridge's table is full at
    /// 2048, and the 2049th mint evicts the oldest.
    #[test]
    fn the_token_bounds_are_the_contracts() {
        assert_eq!(crate::reply_token::DEFAULT_TTL_MS, 30 * 60 * 1000);
        assert_eq!(crate::reply_token::DEFAULT_MAX_TOKENS, 2048);
        let mut bridge = BridgeState::new();
        bridge.joined(channel("general"));
        let mut first = None;
        for i in 0..2049_u32 {
            let mut entropy = [0_u8; 16];
            entropy[..4].copy_from_slice(&i.to_be_bytes());
            let n = bridge
                .notification(&broadcast("general"), entropy, NOW)
                .expect("ok")
                .expect("some");
            if i == 0 {
                first = n.meta.get(MetaKey::ReplyToken).map(str::to_owned);
            }
            if i == 2047 {
                assert_eq!(bridge.live_tokens(NOW), 2048, "the control: full");
            }
        }
        assert_eq!(bridge.live_tokens(NOW), 2048, "bounded");
        assert_eq!(
            bridge.reply_route(&first.expect("token"), NOW),
            Err(TransportError::InvalidArgument),
            "the oldest was evicted"
        );
    }

    /// Tokens expire after the TTL (30 minutes) and resolve as unknown.
    #[test]
    fn a_token_expires() {
        let mut bridge = BridgeState::new();
        bridge.joined(channel("general"));
        let n = bridge
            .notification(&broadcast("general"), [10; 16], NOW)
            .expect("ok")
            .expect("some");
        let token = n.meta.get(MetaKey::ReplyToken).expect("token").to_owned();
        let ttl = crate::reply_token::DEFAULT_TTL_MS;
        assert!(
            bridge.reply_route(&token, NOW + ttl - 1).is_ok(),
            "the control"
        );
        assert_eq!(
            bridge.reply_route(&token, NOW + ttl),
            Err(TransportError::InvalidArgument)
        );
    }
}
