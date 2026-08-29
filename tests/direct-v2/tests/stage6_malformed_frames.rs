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

use interweave_profile_config::{
    ChannelsConfig, DirectoryConfig, EndpointConfig, EndpointsConfig, ProfileConfig,
    RegistrationPolicy, TrustConfig, TrustPolicyKind,
};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    DirectMessageV2, DirectRejectReason, EndpointId, MediaType, MessageId, Payload, TransportError,
    TransportIdentity,
};
use interweave_transport_libp2p::direct_codec::{DIRECT_PROTOCOL, DirectResponse, decode_response};
use interweave_transport_libp2p::runtime::{DirectEndpoints, SubstrateConfig, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::EndpointTrustPolicy;
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

/// Claim each named endpoint for a session of the same name.
///
/// What Stage 6 did implicitly at `configure_direct`, done explicitly:
/// a session sends AS the endpoint it holds, so these tests name sessions
/// after endpoints and the lease is the only thing that binds the two.
type Leases = std::collections::BTreeMap<String, interweave_local_client_api::EndpointLease>;

async fn claim_all(runtime: &SwarmRuntime, names: &[&str]) -> Leases {
    let mut leases = Leases::new();
    for name in names {
        let lease = runtime
            .claim_endpoint(*name, endpoint(name), "in-process")
            .await
            .expect("the claim reaches the task")
            .expect("the endpoint is configured and free");
        leases.insert((*name).to_owned(), lease);
    }
    leases
}

/// A profile carrying these endpoints, which is now the ONLY way to
/// reach `DirectEndpoints` — the runtime derives its state from the
/// canonical validated configuration rather than from a second model
/// assembled here.
fn profile_with(entries: Vec<EndpointConfig>, default: Option<&str>) -> ProfileConfig {
    ProfileConfig {
        schema_version: 2,
        trust: TrustConfig {
            policy: TrustPolicyKind::default(),
            allowed_peers: std::collections::BTreeSet::new(),
        },
        endpoints: EndpointsConfig {
            registration_policy: RegistrationPolicy::default(),
            default_direct_endpoint: default.map(endpoint),
            directory: DirectoryConfig::default(),
            entries,
        },
        discovery: interweave_profile_config::DiscoveryConfig::default(),
        channels: ChannelsConfig::default(),
    }
}

/// One endpoint entry with default policies.
fn entry(name: &str) -> EndpointConfig {
    EndpointConfig {
        id: endpoint(name),
        enabled: true,
        advertise: false,
        allowed_client_kinds: Vec::new(),
        inbound: EndpointTrustPolicy::default(),
        outbound: EndpointTrustPolicy::default(),
    }
}

fn endpoints() -> DirectEndpoints {
    DirectEndpoints::from_profile(
        &profile_with(vec![entry("human"), entry("claude")], Some("human")),
        8,
    )
    .expect("a valid profile")
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
    answers_with_trust(bytes, true, 1)
        .await
        .into_iter()
        .next()
        .expect("one answer")
}

/// As above, but the receiver may distrust the sender, and the sender
/// may send `repeat` frames so a rate bound can be reached.
async fn answers_with_trust(bytes: Vec<u8>, trusted: bool, repeat: usize) -> Vec<DirectResponse> {
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
    claim_all(&receiver, &["human", "claude"]).await;
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
    let mut collected: Vec<DirectResponse> = Vec::with_capacity(repeat);
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
                    collected.push(
                        decode_response(&response)
                            .expect("the receiver answered in the frozen shape"),
                    );
                    if collected.len() == repeat {
                        return collected;
                    }
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

    // SIXTY-FOUR, and the number is bounded from BOTH sides.
    //
    // Above, because the bucket refills while the test runs: 120/minute
    // is two tokens a second, so a burst of 32 plus a slow machine
    // absorbed forty and this test passed vacuously under parallel load.
    //
    // Below, because every request opens a substream at once and the
    // connection has its own limits: 192 closed it outright with
    // `WriteZero`, which fails the test for a reason that has nothing to
    // do with rate limiting.
    let started = tokio::time::Instant::now();
    let answers = answers_with_trust(frame, true, 64).await;
    let elapsed = started.elapsed();

    // THE TEST STATES ITS OWN PRECONDITION rather than quietly becoming
    // vacuous when the machine is slow. Refill over `elapsed` plus the
    // burst must stay under the number sent, or "some were refused"
    // proves nothing.
    let refilled = 2 * elapsed.as_secs();
    assert!(
        32 + refilled < 64,
        "too slow for this to mean anything: {elapsed:?} refilled ~{refilled} tokens"
    );

    // ANY refusal, not the last answer: responses arrive in whatever
    // order the peer produces them, so "the last one" is not the same
    // question as "some were refused".
    assert!(
        answers.iter().any(|a| matches!(
            a,
            DirectResponse::Rejected {
                reason: DirectRejectReason::Overloaded,
                ..
            }
        )),
        "past the burst the answer is overloaded, so the bucket was charged"
    );
    // ...and some were still answered `malformed`, so the flood was not
    // refused wholesale for some other reason.
    assert!(
        answers.iter().any(|a| matches!(
            a,
            DirectResponse::Rejected {
                reason: DirectRejectReason::Malformed,
                ..
            }
        )),
        "the ones within the burst were answered on their merits"
    );
}

/// A physically oversized frame is answered, and for the RIGHT reason.
///
/// The distinction the earlier tests missed. They overstated a declared
/// length inside a short buffer, so the frame still fitted under
/// `MAX_REQUEST_BYTES` and reached the decoder. A sender that actually
/// transmits one byte past the payload ceiling produces a frame that
/// exceeds the request ceiling, and the bounded reader used to error
/// before any rejection could be built — so the peer got nothing at all
/// for the single failure mode the contract names `too_large` outright.
///
/// The reader now keeps the bytes it already had, and the first sixteen
/// of them are the message id.
#[tokio::test]
async fn a_physically_oversized_frame_is_answered_for_the_right_reason() {
    // TRAILING GARBAGE AFTER A COMPLETE LEGAL FRAME. Its declared
    // payload length is untouched and legal, so this is `malformed` —
    // the frame overran the request ceiling for a reason that has
    // nothing to do with payload size.
    //
    // This test asserted `too_large` when it was written, which was the
    // misclassification rather than evidence of it: telling a sender to
    // shrink a payload that was already within the ceiling.
    let mut frame = legal_frame();
    frame.extend(std::iter::repeat_n(0u8, 64 * 1024));

    let answer = what_the_receiver_answers(frame).await;
    assert_eq!(
        answer,
        DirectResponse::Rejected {
            message_id: MessageId::from_bytes([7; 16]),
            reason: DirectRejectReason::Malformed,
        },
        "answered under its own id, and for the right reason"
    );
}

/// An in-flight exchange gets a bounded grace at shutdown.
///
/// `shutdown` used to break the loop the moment the command arrived,
/// dropping every `PendingDirect` — so a caller whose request had
/// already reached the wire was answered `Stopped`, a shutdown
/// cancelling work it had accepted. `DIRECT.md` asks for the opposite:
/// stop taking new work, let existing exchanges finish briefly, close.
///
/// The peer here accepts the request and HOLDS its response channel,
/// which is the only way to keep an exchange genuinely in flight —
/// dropping the channel would resolve it as a failure and there would be
/// nothing left to grace.
#[tokio::test]
async fn shutdown_grants_an_in_flight_exchange_a_bounded_grace() {
    let silent_keys = libp2p::identity::Keypair::generate_ed25519();
    let silent_peer = TransportIdentity::parse(silent_keys.public().to_peer_id().to_string())
        .expect("a valid peer id");

    let mut silent = SwarmBuilder::with_existing_identity(silent_keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack")
        .with_behaviour(|_| {
            request_response::Behaviour::<RawCodec>::new(
                [(DIRECT_PROTOCOL, ProtocolSupport::Full)],
                request_response::Config::default(),
            )
        })
        .expect("behaviour")
        .build();
    silent
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .expect("listens");

    let address = loop {
        if let Libp2pSwarmEvent::NewListenAddr { address, .. } = silent.select_next_some().await {
            break address;
        }
    };

    // Hold every channel, answer nothing.
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            if let Libp2pSwarmEvent::Behaviour(request_response::Event::Message {
                message: request_response::Message::Request { channel, .. },
                ..
            }) = silent.select_next_some().await
            {
                held.push(channel);
            }
        }
    });

    let sender_id = ProfileIdentity::generate();
    let mut sender = SwarmRuntime::start(
        &sender_id,
        SubstrateConfig::default(),
        TrustSources::new(
            PeerTrustPolicy::new([silent_peer.clone()]).expect("a one-peer allowlist"),
            InfrastructureSet::default(),
        ),
    )
    .expect("the sender starts");
    sender
        .configure_direct(endpoints())
        .await
        .expect("endpoints install");
    let leases = claim_all(&sender, &["human", "claude"]).await;
    sender
        .dial(silent_peer.clone(), address)
        .await
        .expect("command")
        .expect("admitted");

    // THE CONNECTION MUST EXIST FIRST, or `send_direct` refuses as
    // unreachable and returns at once — which looks exactly like the
    // pass this test is trying to detect.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "no connection within 20s");
        match tokio::time::timeout(remaining, sender.next_event()).await {
            Ok(Some(interweave_transport_libp2p::runtime::SwarmEvent::Connected { .. })) => break,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the sender stopped before connecting"),
            Err(_) => panic!("no connection within 20s"),
        }
    }

    // Dispatch and walk away: the peer never answers, so this times out
    // and leaves the exchange in flight.
    let dispatched = tokio::time::timeout(
        Duration::from_millis(500),
        sender.send_direct(&leases["human"], silent_peer, legal_message()),
    )
    .await;
    assert!(dispatched.is_err(), "the silent peer answered nothing");

    let started = tokio::time::Instant::now();
    sender.shutdown().await.expect("the task ends");
    let waited = started.elapsed();

    assert!(
        waited >= Duration::from_secs(1),
        "shutdown waited for the exchange rather than dropping it, took {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(15),
        "and the grace is BOUNDED, not a second protocol deadline: {waited:?}"
    );
}

/// The frame the grace test sends, as a value rather than bytes.
fn legal_message() -> DirectMessageV2 {
    DirectMessageV2 {
        message_id: MessageId::from_bytes([9; 16]),
        sent_at_ms: 1,
        source_endpoint: endpoint("human"),
        destination_endpoint: Some(endpoint("claude")),
        payload: Payload::at_ceiling(None, b"held".to_vec()).expect("within the ceiling"),
    }
}

/// Inbound deliveries cannot starve a node's own in-flight exchange.
///
/// The polling gate leaves room for exchanges awaiting a response,
/// because polling is what settles them. `DirectDelivered` events land
/// in the SAME outbox, so if they may spend that room a peer refills it
/// and stops the polling which would settle the exchange — the freeze
/// the slack was added to prevent, reached one layer down.
///
/// The unit tests beside `may_buffer_delivery` prove the predicates.
/// This proves the LOOP uses them for their respective jobs: swapping
/// the call site to share the slack is invisible to a unit test and
/// hangs this one.
///
/// `a` holds an exchange with a peer that never answers, its event
/// capacity is one, and nothing drains it after setup.
#[tokio::test]
async fn inbound_deliveries_cannot_starve_an_outbound_exchange() {
    let silent_keys = libp2p::identity::Keypair::generate_ed25519();
    let silent_peer = TransportIdentity::parse(silent_keys.public().to_peer_id().to_string())
        .expect("a valid peer id");

    let mut silent = SwarmBuilder::with_existing_identity(silent_keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack")
        .with_behaviour(|_| {
            request_response::Behaviour::<RawCodec>::new(
                [(DIRECT_PROTOCOL, ProtocolSupport::Full)],
                request_response::Config::default(),
            )
        })
        .expect("behaviour")
        .build();
    silent
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .expect("listens");
    let silent_address = loop {
        if let Libp2pSwarmEvent::NewListenAddr { address, .. } = silent.select_next_some().await {
            break address;
        }
    };
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            if let Libp2pSwarmEvent::Behaviour(request_response::Event::Message {
                message: request_response::Message::Request { channel, .. },
                ..
            }) = silent.select_next_some().await
            {
                held.push(channel);
            }
        }
    });

    let a_id = ProfileIdentity::generate();
    let a_peer = a_id.transport_identity().expect("peer id");
    let b_id = ProfileIdentity::generate();
    let b_peer = b_id.transport_identity().expect("peer id");

    let mut a = SwarmRuntime::start(
        &a_id,
        SubstrateConfig {
            event_capacity: 1,
            ..SubstrateConfig::default()
        },
        TrustSources::new(
            PeerTrustPolicy::new([silent_peer.clone(), b_peer]).expect("two peers"),
            InfrastructureSet::default(),
        ),
    )
    .expect("a starts");
    let b = SwarmRuntime::start(
        &b_id,
        SubstrateConfig::default(),
        TrustSources::new(
            PeerTrustPolicy::new([a_peer.clone()]).expect("one peer"),
            InfrastructureSet::default(),
        ),
    )
    .expect("b starts");

    a.configure_direct(endpoints()).await.expect("a endpoints");
    b.configure_direct(endpoints()).await.expect("b endpoints");
    let a_leases = claim_all(&a, &["human", "claude"]).await;
    let b_leases = claim_all(&b, &["human", "claude"]).await;
    let a_address = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .await
        .expect("a listens");

    a.dial(silent_peer.clone(), silent_address)
        .await
        .expect("command")
        .expect("admitted");
    b.dial(a_peer.clone(), a_address)
        .await
        .expect("command")
        .expect("admitted");

    // Drain `a` only until both connections exist. After this nothing
    // reads its events, which is the condition under test.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut connected = 0;
    while connected < 2 {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!left.is_zero(), "both connections within 20s");
        if let Ok(Some(interweave_transport_libp2p::runtime::SwarmEvent::Connected { .. })) =
            tokio::time::timeout(left, a.next_event()).await
        {
            connected += 1;
        }
    }

    // `a` dispatches to the silent peer and, WHILE that is in flight,
    // `b` floods it with deliveries.
    let (own, _flood) = tokio::join!(
        tokio::time::timeout(
            Duration::from_secs(25),
            a.send_direct(&a_leases["human"], silent_peer, legal_message()),
        ),
        async {
            for id in 40..60u8 {
                let mut frame = legal_message();
                frame.message_id = MessageId::from_bytes([id; 16]);
                // EACH SEND IS BOUNDED. If `a` has stopped polling these
                // never answer, and awaiting them serially would make a
                // FAILING run take three minutes instead of seconds —
                // the flood only has to fill `a`'s outbox, not succeed.
                let _ = tokio::time::timeout(
                    Duration::from_millis(500),
                    b.send_direct(&b_leases["human"], a_peer.clone(), frame),
                )
                .await;
            }
        }
    );

    // Bounded rather than hung: the exchange reaches its own deadline
    // and reports it. With the slack shared, polling stops and nothing
    // ever settles this.
    let settled = own.expect("a's exchange settled rather than freezing behind deliveries");
    assert!(
        settled.expect("the command reaches the task").is_err(),
        "the silent peer never answers, so this is an error either way"
    );
}

/// A peer that answers with unreadable bytes violated the protocol; it
/// is not unreachable.
///
/// `read_response` reports an unknown tag, a bad endpoint label,
/// trailing bytes or an over-ceiling response as `InvalidData`, and
/// request-response wraps that in `OutboundFailure::Io` — the same
/// variant a broken socket produces. Reading them alike told the caller
/// the peer could not be reached, when the peer had answered and the
/// answer was refused by this side's own decoder.
///
/// The peer here is reachable throughout: it accepts the request and
/// replies. Only its bytes are wrong.
#[tokio::test]
async fn an_unreadable_response_is_a_protocol_violation() {
    let rude_keys = libp2p::identity::Keypair::generate_ed25519();
    let rude_peer = TransportIdentity::parse(rude_keys.public().to_peer_id().to_string())
        .expect("a valid peer id");

    let mut rude = SwarmBuilder::with_existing_identity(rude_keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack")
        .with_behaviour(|_| {
            request_response::Behaviour::<RawCodec>::new(
                [(DIRECT_PROTOCOL, ProtocolSupport::Full)],
                request_response::Config::default(),
            )
        })
        .expect("behaviour")
        .build();
    rude.listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .expect("listens");
    let address = loop {
        if let Libp2pSwarmEvent::NewListenAddr { address, .. } = rude.select_next_some().await {
            break address;
        }
    };

    // Answers every request with a tag no response uses.
    tokio::spawn(async move {
        loop {
            if let Libp2pSwarmEvent::Behaviour(request_response::Event::Message {
                message: request_response::Message::Request { channel, .. },
                ..
            }) = rude.select_next_some().await
            {
                let _ = rude
                    .behaviour_mut()
                    .send_response(channel, vec![0xFF, 0xFF, 0xFF]);
            }
        }
    });

    let sender_id = ProfileIdentity::generate();
    let mut sender = SwarmRuntime::start(
        &sender_id,
        SubstrateConfig::default(),
        TrustSources::new(
            PeerTrustPolicy::new([rude_peer.clone()]).expect("a one-peer allowlist"),
            InfrastructureSet::default(),
        ),
    )
    .expect("the sender starts");
    sender
        .configure_direct(endpoints())
        .await
        .expect("endpoints install");
    let leases = claim_all(&sender, &["human", "claude"]).await;
    sender
        .dial(rude_peer.clone(), address)
        .await
        .expect("command")
        .expect("admitted");

    // THE CONNECTION MUST EXIST FIRST. Without this the send is refused
    // as unreachable before any exchange happens — which is the very
    // answer under test, so the test would pass for the wrong reason
    // and then fail for it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!left.is_zero(), "no connection within 20s");
        if let Ok(Some(interweave_transport_libp2p::runtime::SwarmEvent::Connected { .. })) =
            tokio::time::timeout(left, sender.next_event()).await
        {
            break;
        }
    }

    let error = tokio::time::timeout(
        Duration::from_secs(20),
        sender.send_direct(&leases["human"], rude_peer, legal_message()),
    )
    .await
    .expect("the exchange settled")
    .expect("the command reaches the task")
    .expect_err("the answer was unreadable");

    assert_eq!(
        error,
        TransportError::ProtocolViolation,
        "the peer answered — badly — so this is not a reachability failure"
    );
}

/// A declared payload past the ceiling IS `too_large`.
///
/// The other half of the oversize split. Here the frame overruns the
/// request ceiling because its `payload_len` field genuinely claims more
/// than the profile allows — the one case the contract names
/// `too_large`, and the one a sender can act on by sending less.
#[tokio::test]
async fn a_declared_payload_past_the_ceiling_is_too_large() {
    let mut frame = legal_frame();
    // Overstate `payload_len`, then pad so the frame also overruns the
    // request ceiling and takes the pre-decode path.
    let at = frame.len() - 4 - 5;
    frame[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    frame.extend(std::iter::repeat_n(0u8, 64 * 1024));

    let answer = what_the_receiver_answers(frame).await;
    assert_eq!(
        answer,
        DirectResponse::Rejected {
            message_id: MessageId::from_bytes([7; 16]),
            reason: DirectRejectReason::TooLarge,
        },
        "the declared length is what makes this too_large"
    );
}

/// A non-direct notification cannot starve an in-flight exchange either.
///
/// `polling_room` adds slack so the callers waiting on in-flight work
/// get settled. `DirectDelivered` was taught to respect the base
/// capacity, but every OTHER swarm event — `Connected`, `DialFailed`,
/// an `Identify` result — went through `translate` and was appended
/// unconditionally. One of those takes the progress slot, `room` goes
/// false on the next iteration, and the direct response or its timeout
/// can no longer be polled.
///
/// Here `a` holds an exchange with a peer that never answers, its event
/// capacity is one, nothing drains it — and then a second peer connects,
/// which is a notification and not a delivery.
#[tokio::test]
async fn a_notification_cannot_starve_an_outbound_exchange() {
    let silent_keys = libp2p::identity::Keypair::generate_ed25519();
    let silent_peer = TransportIdentity::parse(silent_keys.public().to_peer_id().to_string())
        .expect("a valid peer id");
    let mut silent = SwarmBuilder::with_existing_identity(silent_keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack")
        .with_behaviour(|_| {
            request_response::Behaviour::<RawCodec>::new(
                [(DIRECT_PROTOCOL, ProtocolSupport::Full)],
                request_response::Config::default(),
            )
        })
        .expect("behaviour")
        .build();
    silent
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .expect("listens");
    let silent_address = loop {
        if let Libp2pSwarmEvent::NewListenAddr { address, .. } = silent.select_next_some().await {
            break address;
        }
    };
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            if let Libp2pSwarmEvent::Behaviour(request_response::Event::Message {
                message: request_response::Message::Request { channel, .. },
                ..
            }) = silent.select_next_some().await
            {
                held.push(channel);
            }
        }
    });

    let a_id = ProfileIdentity::generate();
    let a_peer = a_id.transport_identity().expect("peer id");
    let b_id = ProfileIdentity::generate();
    let b_peer = b_id.transport_identity().expect("peer id");
    let c_id = ProfileIdentity::generate();
    let c_peer = c_id.transport_identity().expect("peer id");

    let mut a = SwarmRuntime::start(
        &a_id,
        SubstrateConfig {
            event_capacity: 1,
            ..SubstrateConfig::default()
        },
        TrustSources::new(
            PeerTrustPolicy::new([silent_peer.clone(), b_peer, c_peer]).expect("three peers"),
            InfrastructureSet::default(),
        ),
    )
    .expect("a starts");
    let b = SwarmRuntime::start(
        &b_id,
        SubstrateConfig::default(),
        TrustSources::new(
            PeerTrustPolicy::new([a_peer.clone()]).expect("one peer"),
            InfrastructureSet::default(),
        ),
    )
    .expect("b starts");
    let c = SwarmRuntime::start(
        &c_id,
        SubstrateConfig::default(),
        TrustSources::new(
            PeerTrustPolicy::new([a_peer.clone()]).expect("one peer"),
            InfrastructureSet::default(),
        ),
    )
    .expect("c starts");

    a.configure_direct(endpoints()).await.expect("a endpoints");
    b.configure_direct(endpoints()).await.expect("b endpoints");
    c.configure_direct(endpoints()).await.expect("c endpoints");
    let leases = claim_all(&a, &["human", "claude"]).await;
    let a_address = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .await
        .expect("a listens");
    a.dial(silent_peer.clone(), silent_address)
        .await
        .expect("command")
        .expect("admitted");

    // Drain only until `a` is connected to the silent peer. After this
    // nothing reads its events.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!left.is_zero(), "no connection within 20s");
        if let Ok(Some(interweave_transport_libp2p::runtime::SwarmEvent::Connected { .. })) =
            tokio::time::timeout(left, a.next_event()).await
        {
            break;
        }
    }

    // `a` dispatches to the silent peer and, while that is in flight,
    // `b` connects — producing a NOTIFICATION on `a`, not a delivery.
    let (own, _) = tokio::join!(
        tokio::time::timeout(
            Duration::from_secs(25),
            a.send_direct(&leases["human"], silent_peer, legal_message()),
        ),
        async {
            let _ = b.dial(a_peer.clone(), a_address.clone()).await;
            let _ = c.dial(a_peer.clone(), a_address).await;
        }
    );

    let settled = own.expect("a's exchange settled rather than freezing behind a notification");
    assert!(
        settled.expect("the command reaches the task").is_err(),
        "the silent peer never answers, so this is an error either way"
    );
}
