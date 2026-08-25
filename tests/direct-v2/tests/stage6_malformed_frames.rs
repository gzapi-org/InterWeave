// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A malformed frame is REFUSED on the wire, not dropped.
//!
//! The contract gives a sender a code for a frame this node cannot
//! parse: `too_large` for an over-ceiling payload, `malformed` for
//! anything else. Mapping the decode failure to an I/O error made
//! request-response emit `InboundFailure` and no request at all, so the
//! handler had nothing to answer with and the peer observed a broken
//! exchange instead.
//!
//! # Why this suite builds its own peer
//!
//! No legal `DirectMessageV2` encodes to bytes the decoder rejects —
//! `Payload`, `MediaType` and `EndpointId` all validate on construction,
//! and `send_direct` encodes from those types. A well-behaved
//! `SwarmRuntime` therefore CANNOT produce the input under test, so
//! proving the receiver answers requires a peer that writes raw bytes.
//! It speaks the real protocol id over the real transport stack; only
//! its codec differs.
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    DirectMessageV2, DirectRejectReason, EndpointId, MediaType, MessageId, Payload,
    TransportIdentity,
};
use interweave_transport_libp2p::direct_codec::{DIRECT_PROTOCOL, DirectResponse, decode_response};
use interweave_transport_libp2p::runtime::{DirectEndpoints, SubstrateConfig, SwarmRuntime};
use interweave_transport_runtime::{Generation, TrustSources};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

use async_trait::async_trait;
use futures::{AsyncReadExt as _, AsyncWriteExt as _, StreamExt as _};
use libp2p::request_response::{self, Codec, ProtocolSupport};
use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;
use libp2p::{Multiaddr, StreamProtocol, SwarmBuilder};

/// A codec that sends whatever bytes it is given.
///
/// The receiver's own codec is the thing under test, so this side does
/// no validation at all — that is the entire point of it existing.
#[derive(Clone, Default)]
struct RawCodec;

#[async_trait]
impl Codec for RawCodec {
    type Protocol = StreamProtocol;
    type Request = Vec<u8>;
    type Response = Vec<u8>;

    async fn read_request<T>(&mut self, _: &StreamProtocol, io: &mut T) -> std::io::Result<Vec<u8>>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.take(64 * 1024).read_to_end(&mut buf).await?;
        Ok(buf)
    }

    async fn read_response<T>(&mut self, _: &StreamProtocol, io: &mut T) -> std::io::Result<Vec<u8>>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.take(64 * 1024).read_to_end(&mut buf).await?;
        Ok(buf)
    }

    async fn write_request<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        req: Vec<u8>,
    ) -> std::io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        io.write_all(&req).await?;
        io.close().await
    }

    async fn write_response<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        res: Vec<u8>,
    ) -> std::io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        io.write_all(&res).await?;
        io.close().await
    }
}

fn endpoint(name: &str) -> EndpointId {
    EndpointId::parse(name).expect("valid endpoint id")
}

fn endpoints() -> DirectEndpoints {
    DirectEndpoints {
        endpoints: vec![endpoint("human"), endpoint("claude")],
        default: Some(endpoint("human")),
        queue_bound: 8,
        epoch: Generation::parse("malformed_______").expect("valid generation"),
    }
}

/// A legal frame, which each test then corrupts in one specific way.
fn legal_frame() -> Vec<u8> {
    DirectMessageV2 {
        message_id: MessageId::from_bytes([7; 16]),
        sent_at_ms: 1,
        source_endpoint: endpoint("human"),
        destination_endpoint: Some(endpoint("claude")),
        payload: Payload::at_ceiling(
            Some(MediaType::parse("text/plain").expect("valid media type")),
            b"hello".to_vec(),
        )
        .expect("within the ceiling"),
    }
    .encode()
}

/// Send `bytes` to a real receiver and return what it answered.
///
/// Bounded throughout: a receiver that never answers is a RESULT.
async fn what_the_receiver_answers(bytes: Vec<u8>) -> DirectResponse {
    answers_with_trust(bytes, true, 1).await
}

/// As above, but the receiver may distrust the sender, and the sender
/// may send `repeat` frames so a rate bound can be reached.
async fn answers_with_trust(bytes: Vec<u8>, trusted: bool, repeat: usize) -> DirectResponse {
    let receiver_id = ProfileIdentity::generate();
    let receiver_peer = receiver_id.transport_identity().expect("peer id");

    let hostile_keys = libp2p::identity::Keypair::generate_ed25519();
    let hostile_peer = TransportIdentity::parse(hostile_keys.public().to_peer_id().to_string())
        .expect("a valid peer id");

    let mut receiver = SwarmRuntime::start(
        &receiver_id,
        SubstrateConfig::default(),
        TrustSources::new(
            if trusted {
                PeerTrustPolicy::new([hostile_peer.clone()]).expect("a one-peer allowlist")
            } else {
                PeerTrustPolicy::new(std::iter::empty()).expect("an empty allowlist")
            },
            if trusted {
                InfrastructureSet::default()
            } else {
                // INFRASTRUCTURE-ONLY, not unknown. A peer this profile
                // does not know at all is refused at the CONNECTION gate
                // and never reaches the direct protocol, so it cannot
                // test what the direct protocol answers. Infrastructure
                // trust is the case that connects and still has no
                // data-plane authority (ADR-0036).
                InfrastructureSet::new([hostile_peer]).expect("a one-peer infrastructure set")
            },
        ),
    )
    .expect("the receiver starts");
    receiver
        .configure_direct(endpoints())
        .await
        .expect("endpoints install");
    let address = receiver
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .await
        .expect("listens");

    let mut hostile = SwarmBuilder::with_existing_identity(hostile_keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the receiver uses")
        .with_behaviour(|_| {
            request_response::Behaviour::<RawCodec>::new(
                [(DIRECT_PROTOCOL, ProtocolSupport::Full)],
                request_response::Config::default(),
            )
        })
        .expect("behaviour")
        .build();

    let dial: Multiaddr = address;
    let receiver_peer_id: libp2p::PeerId = receiver_peer
        .as_str()
        .parse()
        .expect("a peer id the neutral type already validated");
    hostile.dial(dial).expect("the dial starts");

    // Drive both sides until the receiver answers, or give up loudly.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut sent = false;
    let mut seen = 0usize;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "no answer within 20s");

        tokio::select! {
            event = hostile.select_next_some() => match event {
                Libp2pSwarmEvent::ConnectionEstablished { .. } if !sent => {
                    for _ in 0..repeat {
                        hostile
                            .behaviour_mut()
                            .send_request(&receiver_peer_id, bytes.clone());
                    }
                    sent = true;
                }
                Libp2pSwarmEvent::Behaviour(request_response::Event::Message {
                    message: request_response::Message::Response { response, .. },
                    ..
                }) => {
                    seen += 1;
                    if seen < repeat {
                        continue;
                    }
                    // AN EMPTY BODY IS "NOTHING WAS ANSWERED", which is
                    // the defect this suite exists for. Named separately
                    // because dropping a `ResponseChannel` closes the
                    // substream cleanly, so it arrives here as a
                    // zero-byte response rather than as a failure — and
                    // a decode error would misdescribe the cause.
                    assert!(
                        !response.is_empty(),
                        "the receiver closed the exchange without answering"
                    );
                    return decode_response(&response)
                        .expect("the receiver answered in the frozen shape");
                }
                Libp2pSwarmEvent::Behaviour(request_response::Event::OutboundFailure {
                    error, ..
                }) => panic!("the exchange failed instead of being answered: {error:?}"),
                _ => {}
            },
            // The receiver's own task runs independently; this just keeps
            // its event channel drained so it never blocks.
            _ = receiver.next_event() => {}
            () = tokio::time::sleep(remaining) => panic!("no answer within 20s"),
        }
    }
}

/// An over-ceiling declared payload is `too_large`, on the wire.
#[tokio::test]
async fn an_over_ceiling_payload_is_answered_too_large() {
    let mut frame = legal_frame();
    let at = frame.len() - 4 - 5;
    frame[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());

    let answer = what_the_receiver_answers(frame).await;
    assert_eq!(
        answer,
        DirectResponse::Rejected {
            message_id: MessageId::from_bytes([7; 16]),
            reason: DirectRejectReason::TooLarge,
        },
        "the sender is told what was wrong, and under its own id"
    );
}

/// Anything else is `malformed`.
#[tokio::test]
async fn a_bad_field_is_answered_malformed() {
    let mut frame = legal_frame();
    // Claim a source endpoint far longer than the bytes that follow.
    frame[MessageId::LEN + 8] = 200;

    let answer = what_the_receiver_answers(frame).await;
    assert_eq!(
        answer,
        DirectResponse::Rejected {
            message_id: MessageId::from_bytes([7; 16]),
            reason: DirectRejectReason::Malformed,
        },
        "and a bad field is malformed, not too_large"
    );
}

// NO TEST HERE FOR AN UNTRUSTED PEER'S MALFORMED FRAME, and the reason
// is worth recording rather than leaving as an absence.
//
// Neither an unknown peer nor an infrastructure-only one can hold an
// INBOUND connection at all: `settle_outcome` admits inbound with
// `manager.authorizes(class)`, which is `authorizes_for(class,
// DialOrigin::Manual)`, and `Manual.is_data_plane()` is true — so
// `ConnectivityInfrastructureOnly` is refused and the socket closes.
// Both were tried here and both produced `ConnectionClosed` before a
// request could be sent, which is the connection layer doing its job.
//
// The reachable case is trust REVOKED after the connection is
// established, where the close is asynchronous. That is a race by
// construction, and `stage6_direct_over_the_wire.rs` already covers it
// the only honest way — by naming both outcomes. What IS deterministic
// is the rate bound below, and it is sufficient evidence the gates run:
// `Overloaded` is a code only `admit_prefix` can produce.

/// Malformed frames are rate limited like any other request.
///
/// Answering costs this node an encode and a send, so a peer that never
/// spends ingress allowance to make it happen can do it without limit.
/// The per-peer burst is 32, so the thirty-third in one instant is over
/// it — and `overloaded` is what it must hear, not `malformed`.
#[tokio::test]
async fn malformed_frames_spend_ingress_allowance() {
    let mut frame = legal_frame();
    frame[MessageId::LEN + 8] = 200;

    let answer = answers_with_trust(frame, true, 40).await;
    assert_eq!(
        answer,
        DirectResponse::Rejected {
            message_id: MessageId::from_bytes([7; 16]),
            reason: DirectRejectReason::Overloaded,
        },
        "past the burst the answer is overloaded, so the bucket was charged"
    );
}
