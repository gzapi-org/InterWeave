// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A raw `/meshsub/1.1.0` writer, for messages the high-level API
//! cannot construct.
//!
//! # Why this exists
//!
//! `MessageAuthenticity::Author` publishes an UNSIGNED message with a
//! claimed source. `PUBSUB.md` requires the receive path to reject an
//! invalid *signed* source claim before it can poison the duplicate
//! cache, and a missing signature is not that: it could in principle
//! take an earlier rejection path, leaving the signed case untested
//! while the spike reported PASS. Review caught exactly that.
//!
//! `gossipsub::Behaviour::publish` only ever signs correctly, and
//! nothing public accepts a prebuilt `RawMessage`, so the only way to
//! put a present-but-invalid signature on the wire is to write the
//! frames.
//!
//! # The control that makes this trustworthy
//!
//! Hand-rolled protobuf invites its own failure: an encoding the
//! receiver cannot parse is rejected too, and the experiment would
//! then "pass" for a reason that has nothing to do with signatures.
//! So the injector is used TWICE -- once with a correct signature,
//! which the receiver must DELIVER, and once mutated. The first is not
//! decoration; it is what proves the second means anything.

use futures::AsyncWriteExt;
use libp2p::core::upgrade::ReadyUpgrade;
use libp2p::identity::Keypair;
use libp2p::swarm::handler::{
    ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedOutbound,
    SubstreamProtocol,
};
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, Stream, StreamProtocol,
    THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId};
use std::collections::VecDeque;
use std::task::{Context, Poll};

/// The gossipsub protocol this writes. 1.1.0 rather than 1.2.0 because
/// the frame shape is the same and the older one is what every peer
/// supports.
const MESHSUB: StreamProtocol = StreamProtocol::new("/meshsub/1.1.0");

/// `libp2p-pubsub:`, the domain gossipsub signs under.
const SIGNING_PREFIX: &[u8] = b"libp2p-pubsub:";

/// A protobuf length-delimited field: tag byte, varint length, bytes.
fn field(out: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
    out.push(tag);
    varint(out, bytes.len());
    out.extend_from_slice(bytes);
}

/// Protobuf/multistream unsigned varint.
fn varint(out: &mut Vec<u8>, mut n: usize) {
    loop {
        let byte = u8::try_from(n & 0x7F).unwrap_or(0);
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// The gossipsub `Message` protobuf, in the field order gossipsub
/// itself writes: from=1, data=2, seqno=3, topic=4, signature=5, key=6.
///
/// `topic` is a plain string field in this version, so it is always
/// written -- the signature is computed over exactly these bytes with
/// signature and key omitted, which is why the order and presence have
/// to match rather than merely parse.
fn message_bytes(
    from: &[u8],
    data: &[u8],
    seqno: &[u8],
    topic: &str,
    signature: Option<&[u8]>,
) -> Vec<u8> {
    let mut out = Vec::new();
    field(&mut out, 0x0A, from);
    field(&mut out, 0x12, data);
    field(&mut out, 0x1A, seqno);
    field(&mut out, 0x22, topic.as_bytes());
    if let Some(sig) = signature {
        field(&mut out, 0x2A, sig);
    }
    out
}

/// One RPC frame carrying a subscription and one publish.
///
/// The subscription is what makes the receiver treat this connection
/// as a gossipsub peer interested in the topic; the publish is the
/// message under test.
fn rpc_frame(topic: &str, message: &[u8]) -> Vec<u8> {
    let mut sub = Vec::new();
    sub.push(0x08); // subscribe = 1 (bool)
    sub.push(0x01);
    field(&mut sub, 0x12, topic.as_bytes()); // topic_id = 2

    let mut rpc = Vec::new();
    field(&mut rpc, 0x0A, &sub); // subscriptions = 1
    field(&mut rpc, 0x12, message); // publish = 2

    // The stream itself is varint-length-delimited.
    let mut framed = Vec::new();
    varint(&mut framed, rpc.len());
    framed.extend_from_slice(&rpc);
    framed
}

/// Build a gossipsub message signed by `keypair`, optionally over
/// DIFFERENT data than it carries.
///
/// `signed_over` is what the signature actually covers. Passing the
/// same bytes as `data` yields a valid message; passing anything else
/// yields one whose signature is PRESENT, well-formed, and wrong --
/// which is the case `PUBSUB.md` names and the reason this function
/// takes two payloads instead of one.
#[must_use]
pub fn signed_message(
    keypair: &Keypair,
    topic: &str,
    data: &[u8],
    signed_over: &[u8],
    seqno: u64,
) -> Vec<u8> {
    let from = PeerId::from_public_key(&keypair.public()).to_bytes();
    let seq = seqno.to_be_bytes().to_vec();

    let mut to_sign = SIGNING_PREFIX.to_vec();
    to_sign.extend_from_slice(&message_bytes(&from, signed_over, &seq, topic, None));
    let signature = keypair.sign(&to_sign).expect("ed25519 signs");

    message_bytes(&from, data, &seq, topic, Some(&signature))
}

/// Writes one prepared frame to one peer, then closes the stream.
pub struct Injector {
    frame: Vec<u8>,
    pending: VecDeque<PeerId>,
}

impl Injector {
    /// Carry `frame` (from [`rpc_frame`]) to whoever connects.
    #[must_use]
    pub fn new(topic: &str, message: &[u8]) -> Self {
        Self {
            frame: rpc_frame(topic, message),
            pending: VecDeque::new(),
        }
    }
}

impl NetworkBehaviour for Injector {
    type ConnectionHandler = Writer;
    type ToSwarm = ();

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(Writer::idle())
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: libp2p::core::Endpoint,
        _: libp2p::core::transport::PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        self.pending.push_back(peer);
        Ok(Writer::carrying(self.frame.clone()))
    }

    fn on_swarm_event(&mut self, _: FromSwarm<'_>) {}

    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        (): THandlerOutEvent<Self>,
    ) {
    }

    fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<(), THandlerInEvent<Self>>> {
        Poll::Pending
    }
}

/// The handler: asks for one `/meshsub/1.1.0` substream, writes the
/// frame, closes.
pub struct Writer {
    frame: Option<Vec<u8>>,
    asked: bool,
    writing: Option<futures::future::BoxFuture<'static, ()>>,
}

impl Writer {
    const fn idle() -> Self {
        Self {
            frame: None,
            asked: true,
            writing: None,
        }
    }
    const fn carrying(frame: Vec<u8>) -> Self {
        Self {
            frame: Some(frame),
            asked: false,
            writing: None,
        }
    }
}

impl ConnectionHandler for Writer {
    type FromBehaviour = ();
    type ToBehaviour = ();
    type InboundProtocol = ReadyUpgrade<StreamProtocol>;
    type OutboundProtocol = ReadyUpgrade<StreamProtocol>;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, ()> {
        SubstreamProtocol::new(ReadyUpgrade::new(MESHSUB), ())
    }

    fn on_behaviour_event(&mut self, (): ()) {}

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<'_, Self::InboundProtocol, Self::OutboundProtocol>,
    ) {
        if let ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
            protocol: mut stream,
            ..
        }) = event
            && let Some(frame) = self.frame.take()
        {
            self.writing = Some(Box::pin(async move {
                let _ = write_and_close(&mut stream, &frame).await;
            }));
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ConnectionHandlerEvent<Self::OutboundProtocol, (), ()>> {
        if !self.asked {
            self.asked = true;
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(ReadyUpgrade::new(MESHSUB), ()),
            });
        }
        if let Some(writing) = self.writing.as_mut() {
            if std::pin::Pin::new(writing).poll(cx).is_ready() {
                self.writing = None;
            }
        }
        Poll::Pending
    }
}

async fn write_and_close(stream: &mut Stream, frame: &[u8]) -> std::io::Result<()> {
    stream.write_all(frame).await?;
    stream.flush().await?;
    Ok(())
}
