// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Experiment A: rust-libp2p `request-response` under the direct-v2 shape.
//!
//! NON-PRODUCTION protocol names and a non-production codec, per
//! `SPIKES.md`. Nothing here is an implementation of
//! `/interweave/direct/2.0.0`; it is the smallest thing that makes the
//! library answer the questions Stage 6 needs answered before it is
//! written.

use std::time::Duration;

use futures::StreamExt;
use libp2p::request_response::{
    self, InboundFailure, OutboundFailure, ProtocolSupport, ResponseChannel,
};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, StreamProtocol, Swarm, noise, tcp, yamux};
use serde::{Deserialize, Serialize};

use interweave_transport_api::{EndpointId, MessageId, TransportIdentity};
use interweave_transport_runtime::dedup::{
    DedupKey, DestinationSelector, Reservation, ReservationFailure, ReservationMap,
};
use interweave_transport_runtime::fingerprint::direct_content_fingerprint_v1;

/// The spike's stand-in for the direct protocol. Deliberately NOT
/// `/interweave/direct/2.0.0`: a spike that speaks the production
/// protocol name is one `git mv` away from being mistaken for it.
const DIRECT_V2: StreamProtocol = StreamProtocol::new("/spike-002/direct/2.0.0");
/// A second major, supported by only one side, for the negotiation half.
const DIRECT_V3: StreamProtocol = StreamProtocol::new("/spike-002/direct/3.0.0");
/// A second protocol FAMILY on the same behaviour.
const ENDPOINTS_V1: StreamProtocol = StreamProtocol::new("/spike-002/endpoints/1.0.0");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Stands in for the direct envelope's message id.
    pub message_id: String,
    /// `None` means "the remote default endpoint", never fan-out.
    pub destination: Option<String>,
    pub body: Vec<u8>,
}

/// THE PRODUCTION WIRE VOCABULARY, not a spike-private copy of it.
///
/// The first version of this harness carried its own two-variant enum
/// here. That made every "the peer was told X" line a statement about
/// the harness's own type, and A9 in particular a statement about an
/// encoder the harness had written for the purpose. The type that
/// reaches the wire in Stage 6 is `DirectRejectReason`, its serde
/// derive IS the encoder, and `ResolveFailure::to_wire` is the
/// production collapse -- so those are what every response below is
/// built from.
pub use interweave_transport_api::DirectRejectReason as Reason;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    /// Bounded remote endpoint queue admission. NOT application processing.
    AcceptedV2 { resolved_endpoint: String },
    /// Refused, with the coarse reason.
    Rejected { reason: Reason },
}

#[derive(NetworkBehaviour)]
pub struct Behaviour {
    pub direct: request_response::cbor::Behaviour<Request, Response>,
    pub endpoints: request_response::cbor::Behaviour<Request, Response>,
}

/// Build a node. `direct_protocols` lets one side advertise a different
/// major, which is the negotiation experiment.
pub fn node(
    direct_protocols: Vec<(StreamProtocol, ProtocolSupport)>,
    request_timeout: Duration,
) -> Swarm<Behaviour> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("tcp")
        .with_behaviour(|_| Behaviour {
            direct: request_response::cbor::Behaviour::new(
                direct_protocols,
                request_response::Config::default().with_request_timeout(request_timeout),
            ),
            endpoints: request_response::cbor::Behaviour::new(
                [(ENDPOINTS_V1, ProtocolSupport::Full)],
                request_response::Config::default().with_request_timeout(request_timeout),
            ),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
        .build()
}

/// `node`, but presenting a CALLER-CHOSEN identity rather than a fresh
/// one. Two swarms built with the same keypair present the same PeerId
/// -- and therefore the same `source_peer` for `DedupKey` purposes --
/// while remaining physically distinct connections. That is what makes
/// a genuine cancellation race constructible: two independent
/// connections carrying retransmissions of ONE key, so one can be
/// killed without the other.
pub fn node_as(
    keypair: libp2p::identity::Keypair,
    direct_protocols: Vec<(StreamProtocol, ProtocolSupport)>,
    request_timeout: Duration,
) -> Swarm<Behaviour> {
    libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("tcp")
        .with_behaviour(|_| Behaviour {
            direct: request_response::cbor::Behaviour::new(
                direct_protocols,
                request_response::Config::default().with_request_timeout(request_timeout),
            ),
            endpoints: request_response::cbor::Behaviour::new(
                [(ENDPOINTS_V1, ProtocolSupport::Full)],
                request_response::Config::default().with_request_timeout(request_timeout),
            ),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
        .build()
}

pub fn full_direct() -> Vec<(StreamProtocol, ProtocolSupport)> {
    vec![(DIRECT_V2, ProtocolSupport::Full)]
}

pub fn only_v3() -> Vec<(StreamProtocol, ProtocolSupport)> {
    vec![(DIRECT_V3, ProtocolSupport::Full)]
}

/// Listen on loopback and return the bound address.
pub async fn listen(swarm: &mut Swarm<Behaviour>) -> Multiaddr {
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("addr"))
        .expect("listen");
    // Bounded for the same reason every experiment loop is: a setup
    // step that waits forever reports nothing at all, and no exit code
    // can express a run that never ends. Panicking here is deliberate --
    // it names the step and exits non-zero, where hanging names nothing.
    let deadline = tokio::time::sleep(Duration::from_secs(20));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => panic!("no listen address within 20s"),
            event = swarm.select_next_some() => {
                if let SwarmEvent::NewListenAddr { address, .. } = event {
                    return address;
                }
            }
        }
    }
}

/// One line of evidence.
pub fn note(what: &str, detail: impl std::fmt::Display) {
    println!("  {what:<46} {detail}");
}

/// Required observations that came out false.
static FAILURES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One line of evidence that the spike's conclusions REST on.
///
/// Prints like [`note`] and records a false answer. Without this the
/// harness printed a false verdict and still exited 0, so `cargo run`
/// -- the reproduction the README tells a reader to run -- reported
/// success while its own output disproved the recorded PASS. A script
/// checking the exit status would have been told the spike passed.
pub fn check(what: &str, ok: bool) -> bool {
    note(what, ok);
    if !ok {
        FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    ok
}

/// How many required observations failed.
#[must_use]
pub fn failures() -> usize {
    FAILURES.load(std::sync::atomic::Ordering::Relaxed)
}

/// The dedup key both sides of the race use.
pub fn key(source: &TransportIdentity, message_id: &str) -> DedupKey {
    DedupKey::Direct {
        source_peer: source.clone(),
        source_endpoint: EndpointId::parse("chat").expect("endpoint"),
        destination_selector: DestinationSelector::Explicit(
            EndpointId::parse("chat").expect("endpoint"),
        ),
        // A 128-bit id derived from the spike's label, so two copies of
        // one logical message hash to one key exactly as the wire ids
        // would.
        message_id: MessageId::from_bytes(label_to_id(message_id)),
    }
}

/// A stable 128-bit id for a human-readable label.
fn label_to_id(label: &str) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(label.as_bytes());
    let mut id = [0_u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

pub use libp2p::request_response::Event as RrEvent;
pub use libp2p::request_response::Message as RrMessage;

pub fn describe_outbound(f: &OutboundFailure) -> String {
    match f {
        OutboundFailure::DialFailure => "DialFailure".to_owned(),
        OutboundFailure::Timeout => "Timeout".to_owned(),
        OutboundFailure::ConnectionClosed => "ConnectionClosed".to_owned(),
        OutboundFailure::UnsupportedProtocols => "UnsupportedProtocols".to_owned(),
        OutboundFailure::Io(e) => format!("Io({e})"),
    }
}

pub fn describe_inbound(f: &InboundFailure) -> String {
    match f {
        InboundFailure::Timeout => "Timeout".to_owned(),
        InboundFailure::ConnectionClosed => "ConnectionClosed".to_owned(),
        InboundFailure::UnsupportedProtocols => "UnsupportedProtocols".to_owned(),
        InboundFailure::ResponseOmission => "ResponseOmission".to_owned(),
        InboundFailure::Io(e) => format!("Io({e})"),
    }
}

/// A PeerId as the neutral contract sees it.
pub fn identity(peer: &PeerId) -> TransportIdentity {
    TransportIdentity::parse(peer.to_base58()).expect("canonical peer id")
}

/// Connect `a` to `b`, returning once both report the connection.
async fn connect(a: &mut Swarm<Behaviour>, b: &mut Swarm<Behaviour>, addr: Multiaddr) {
    a.dial(addr).expect("dial");
    let mut up = 0;
    let deadline = tokio::time::sleep(Duration::from_secs(20));
    tokio::pin!(deadline);
    while up < 2 {
        tokio::select! {
            _ = &mut deadline => panic!("connection not established by both sides within 20s"),
            e = a.select_next_some() => {
                if matches!(e, SwarmEvent::ConnectionEstablished { .. }) { up += 1; }
            }
            e = b.select_next_some() => {
                if matches!(e, SwarmEvent::ConnectionEstablished { .. }) { up += 1; }
            }
        }
    }
}

fn request(message_id: &str, destination: Option<&str>, body: &[u8]) -> Request {
    Request {
        message_id: message_id.to_owned(),
        destination: destination.map(str::to_owned),
        body: body.to_vec(),
    }
}

/// A1 — the ordinary case, plus what an omitted destination looks like.
pub async fn a1_matching_majors() {
    let mut server = node(full_direct(), Duration::from_secs(5));
    let mut client = node(full_direct(), Duration::from_secs(5));
    let addr = listen(&mut server).await;
    let server_peer = *server.local_peer_id();
    connect(&mut client, &mut server, addr).await;

    // Explicit destination, then omitted. Omitted must mean the remote
    // DEFAULT endpoint and never fan-out; here that is the responder's
    // decision to make, and the spike only shows the request carries the
    // distinction.
    // EACH REQUEST KEEPS ITS ID, so a reply can be tied to the request
    // that produced it. The previous fix collected replies into a bag
    // and asked whether `chat` and `default` were both present; a
    // responder that SWAPPED them -- `default` for the explicit request
    // and `chat` for the omitted one -- satisfied that while every
    // per-request claim below was false.
    let explicit_id = client
        .behaviour_mut()
        .direct
        .send_request(&server_peer, request("m-1", Some("chat"), b"hello"));
    let omitted_id = client
        .behaviour_mut()
        .direct
        .send_request(&server_peer, request("m-2", None, b"hello"));

    let mut answered = 0;
    // WHAT CAME BACK, keyed by WHICH request it answers. `seen_explicit`
    // and `seen_default` below are set on the SERVER side -- they say
    // the responder received both destination forms, which is a
    // different claim from the one this experiment reports.
    let mut replies: std::collections::BTreeMap<
        libp2p::request_response::OutboundRequestId,
        Response,
    > = std::collections::BTreeMap::new();
    let mut seen_explicit = false;
    let mut seen_default = false;
    // A response that never arrives is a RESULT, not a reason to wait
    // forever: without this the run hangs and reports nothing at all,
    // which is the one outcome no exit code can express.
    let deadline = tokio::time::sleep(Duration::from_secs(20));
    tokio::pin!(deadline);
    while answered < 2 {
        tokio::select! {
            _ = &mut deadline => break,
            e = server.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { request, channel, .. }, ..
                })) = e {
                    let resolved = request.destination.clone().unwrap_or_else(|| "default".to_owned());
                    if request.destination.is_some() { seen_explicit = true; } else { seen_default = true; }
                    let _ = server.behaviour_mut().direct.send_response(
                        channel,
                        Response::AcceptedV2 { resolved_endpoint: resolved },
                    );
                }
            }
            e = client.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Response { request_id, response }, ..
                })) = e {
                    note("response", format!("{request_id:?} -> {response:?}"));
                    replies.insert(request_id, response);
                    answered += 1;
                }
            }
        }
    }
    check(
        "both requests were answered within the deadline",
        answered == 2,
    );
    check("explicit destination carried", seen_explicit);
    check("omitted destination carried", seen_default);

    // THE ROUND TRIP THIS EXPERIMENT ACTUALLY CLAIMS, per request. Both
    // checks above are about what the RESPONDER saw. The first repair of
    // this asked only whether `chat` and `default` were both present
    // somewhere in the replies, which a responder that swapped the two
    // satisfies -- so each reply is now matched to the request that
    // produced it, and the claim is about that pairing.
    let resolved_to = |id, want: &str| {
        matches!(
            replies.get(&id),
            Some(Response::AcceptedV2 { resolved_endpoint }) if resolved_endpoint == want
        )
    };
    check(
        "the EXPLICIT request resolved to `chat`",
        resolved_to(explicit_id, "chat"),
    );
    check(
        "and the OMITTED request to `default` -- not swapped, not the same",
        resolved_to(omitted_id, "default"),
    );
    check(
        "with nothing refused",
        replies
            .values()
            .all(|r| matches!(r, Response::AcceptedV2 { .. })),
    );
}

/// A2 — one side speaks only a different major.
pub async fn a2_unsupported_major() {
    let mut server = node(only_v3(), Duration::from_secs(5));
    let mut client = node(full_direct(), Duration::from_secs(5));
    let addr = listen(&mut server).await;
    let server_peer = *server.local_peer_id();
    connect(&mut client, &mut server, addr).await;

    client
        .behaviour_mut()
        .direct
        .send_request(&server_peer, request("m-3", Some("chat"), b"hello"));

    // Finding 3 is that an unsupported MAJOR is refused as
    // `UnsupportedProtocols` rather than hanging or reporting something
    // else. Printing whatever arrived would record a PASS for a timeout
    // or for a different error, so the exact variant is asserted.
    let mut unsupported = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {
                note("outbound verdict", "NONE within 10s");
                break;
            }
            e = server.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::InboundFailure { error, .. })) = e {
                    note("responder sees", describe_inbound(&error));
                }
            }
            e = client.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::OutboundFailure { error, .. })) = e {
                    note("requester sees", describe_outbound(&error));
                    unsupported = matches!(error, OutboundFailure::UnsupportedProtocols);
                    break;
                }
            }
        }
    }
    check(
        "the requester was told UnsupportedProtocols, not merely something",
        unsupported,
    );
}

/// A3 — both protocol families over one connection.
pub async fn a3_two_families() {
    let mut server = node(full_direct(), Duration::from_secs(5));
    let mut client = node(full_direct(), Duration::from_secs(5));
    let addr = listen(&mut server).await;
    let server_peer = *server.local_peer_id();
    connect(&mut client, &mut server, addr).await;

    client
        .behaviour_mut()
        .direct
        .send_request(&server_peer, request("m-4", Some("chat"), b"direct"));
    client
        .behaviour_mut()
        .endpoints
        .send_request(&server_peer, request("m-5", None, b"directory"));

    let mut direct_seen = false;
    let mut endpoints_seen = false;
    // Same bound A1 needed, and missed here when A1 got it: a request
    // that never reaches the responder leaves this loop with nothing to
    // end it, and a run that hangs reports no verdict at all.
    let deadline = tokio::time::sleep(Duration::from_secs(20));
    tokio::pin!(deadline);
    // THE CONNECTION IDS THE REQUESTS ACTUALLY ARRIVED ON. Counting
    // peers answers a different question: `num_peers()` is 1 whether one
    // connection carried both families or each opened its own, so the
    // first version of this experiment would have reported "one
    // connection" in exactly the case it was meant to detect.
    let mut connection_ids: Vec<libp2p::swarm::ConnectionId> = Vec::new();
    while !(direct_seen && endpoints_seen) {
        tokio::select! {
            _ = &mut deadline => break,
            e = server.select_next_some() => match e {
                SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { channel, .. }, connection_id, ..
                })) => {
                    direct_seen = true;
                    if !connection_ids.contains(&connection_id) {
                        connection_ids.push(connection_id);
                    }
                    let _ = server.behaviour_mut().direct.send_response(
                        channel, Response::AcceptedV2 { resolved_endpoint: "chat".to_owned() });
                }
                SwarmEvent::Behaviour(BehaviourEvent::Endpoints(RrEvent::Message {
                    message: RrMessage::Request { channel, .. }, connection_id, ..
                })) => {
                    endpoints_seen = true;
                    if !connection_ids.contains(&connection_id) {
                        connection_ids.push(connection_id);
                    }
                    let _ = server.behaviour_mut().endpoints.send_response(
                        channel, Response::AcceptedV2 { resolved_endpoint: "default".to_owned() });
                }
                _ => {}
            },
            _ = client.select_next_some() => {}
        }
    }
    check("both families answered", direct_seen && endpoints_seen);
    // The recorded finding is "one connection serves both protocol
    // families, so no connection-per-protocol accounting is needed".
    // Printing this count let the run agree with itself when the count
    // was 2 -- the exact case the experiment exists to detect.
    check(
        "and both arrived on ONE connection",
        connection_ids.len() == 1,
    );
    note(
        "distinct connections the two families arrived on",
        connection_ids.len(),
    );
    note(
        "established connections on the responder",
        server
            .network_info()
            .connection_counters()
            .num_established(),
    );
}

/// A4 — hold a response across an await and show the Swarm still serves.
pub async fn a4_withheld_response() {
    let mut server = node(full_direct(), Duration::from_secs(30));
    let mut slow = node(full_direct(), Duration::from_secs(30));
    let mut prompt = node(full_direct(), Duration::from_secs(30));
    let addr = listen(&mut server).await;
    let server_peer = *server.local_peer_id();
    connect(&mut slow, &mut server, addr.clone()).await;
    connect(&mut prompt, &mut server, addr).await;

    slow.behaviour_mut()
        .direct
        .send_request(&server_peer, request("m-slow", Some("chat"), b"slow"));

    // The responder takes the channel and does NOT answer yet: this is
    // "withhold AcceptedV2 until bounded local route admission".
    let mut held: Option<ResponseChannel<Response>> = None;
    let mut prompt_answered = false;
    let mut prompt_accepted = false;
    let mut sent_prompt = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !prompt_answered {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {
                check("prompt request answered while one was held", false);
                return;
            }
            e = server.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { request, channel, .. }, ..
                })) = e {
                    if request.message_id == "m-slow" {
                        held = Some(channel);
                        note("held one response channel", "yes");
                    } else {
                        let _ = server.behaviour_mut().direct.send_response(
                            channel, Response::AcceptedV2 { resolved_endpoint: "chat".to_owned() });
                    }
                }
            }
            e = slow.select_next_some() => { let _ = e; }
            e = prompt.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Response { response, .. }, ..
                })) = e {
                    // SERVED, not merely answered. A rejection is an
                    // answer too, and "a second peer was served while
                    // one was held" is false if that is what arrived.
                    prompt_accepted = matches!(response, Response::AcceptedV2 { .. });
                    prompt_answered = true;
                }
            }
        }
        // Send the second request only once the first is being held.
        if held.is_some() && !sent_prompt {
            prompt
                .behaviour_mut()
                .direct
                .send_request(&server_peer, request("m-prompt", Some("chat"), b"prompt"));
            sent_prompt = true;
        }
    }
    check("a second peer was served while one was held", true);
    check("  and SERVED means accepted, not refused", prompt_accepted);

    // Now answer the held one and see it still lands.
    if let Some(channel) = held {
        let _ = server.behaviour_mut().direct.send_response(
            channel,
            Response::AcceptedV2 {
                resolved_endpoint: "chat".to_owned(),
            },
        );
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {
                check("the held response arrived late", false);
                break;
            }
            _ = server.select_next_some() => {}
            e = slow.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Response { response, .. }, ..
                })) = e {
                    check("the held response arrived late", true);
                    check(
                        "  and it was the acceptance, not a refusal",
                        matches!(response, Response::AcceptedV2 { .. }),
                    );
                    break;
                }
            }
        }
    }
}

/// A5 — a response that never comes.
pub async fn a5_timeout() {
    let mut server = node(full_direct(), Duration::from_secs(2));
    let mut client = node(full_direct(), Duration::from_secs(2));
    let addr = listen(&mut server).await;
    let server_peer = *server.local_peer_id();
    connect(&mut client, &mut server, addr).await;

    client
        .behaviour_mut()
        .direct
        .send_request(&server_peer, request("m-6", Some("chat"), b"never"));

    let mut kept: Option<ResponseChannel<Response>> = None;
    let mut outbound = None;
    let mut inbound = None;
    let mut inbound_timeout = false;
    let mut outbound_permitted = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while outbound.is_none() || inbound.is_none() {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            e = server.select_next_some() => match e {
                SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { channel, .. }, ..
                })) => { kept = Some(channel); }
                SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::InboundFailure { error, .. })) => {
                    inbound_timeout = matches!(error, InboundFailure::Timeout);
                    inbound = Some(describe_inbound(&error));
                }
                _ => {}
            },
            e = client.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::OutboundFailure { error, .. })) = e {
                    outbound_permitted =
                        matches!(error, OutboundFailure::Timeout | OutboundFailure::Io(_));
                    outbound = Some(describe_outbound(&error));
                }
            }
        }
    }
    note(
        "requester sees",
        outbound.unwrap_or_else(|| "nothing".to_owned()),
    );
    note(
        "responder sees",
        inbound.unwrap_or_else(|| "nothing".to_owned()),
    );
    // The responder is still holding a channel it can no longer answer:
    // `is_open` is false, so a late `send_response` has nowhere to go.
    // That is the FINDING, so it is asserted rather than printed -- the
    // label used to read "still answerable" against a value of `false`,
    // which is the shape of a failing line sitting inside a PASS.
    // THE ATTRIBUTION IS THE FIRST HALF OF THE FINDING, and it was
    // only printed: a regression to `ConnectionClosed`, or no event at
    // all, left the two checks below covering the retained channel
    // while the documented Timeout/Io result quietly stopped holding.
    check("the responder was told Timeout", inbound_timeout);
    // The SET, not one variant. Both sides run the same request_timeout
    // and whichever fires first decides what the other is told, so
    // Timeout and Io are both correct here -- which is exactly why
    // Stage 6 must treat them as one class.
    check(
        "and the requester saw Timeout or Io, the two the race permits",
        outbound_permitted,
    );
    check("responder still holds a channel", kept.is_some());
    check(
        "and that channel is NO LONGER answerable",
        !kept.is_some_and(|c| c.is_open()),
    );
}

/// A6 — the race the architecture depends on, run as a race.
///
/// `DIRECT.md`: "Matching concurrent duplicates attach as waiters and
/// receive the same eventual response." The first version of this
/// experiment admitted every copy on one mutable state in one pass and
/// had the waiter manufacture its own `AcceptedV2` immediately. That
/// proved a retained map entry returns `Waiter` -- a fact about a
/// `BTreeMap` -- and said nothing about waiters sharing an
/// ASYNCHRONOUSLY produced owner result, which is the whole claim.
///
/// So the owner's admission is genuinely asynchronous here: it parks its
/// response channel, a timer stands in for bounded endpoint-queue
/// admission, every matching copy that arrives meanwhile parks alongside
/// it, and one outcome answers all of them.
pub async fn a6_same_key_race() {
    for outcome in [
        Response::AcceptedV2 {
            resolved_endpoint: "chat".to_owned(),
        },
        // THE REJECTION HALF. A waiter that manufactured an acceptance
        // would look identical to a correct implementation on the happy
        // path and would fabricate deliveries on this one.
        Response::Rejected {
            reason: Reason::NoRoute,
        },
    ] {
        println!("  -- owner outcome: {outcome:?}");
        same_key_race_once(&outcome).await;
    }
}

async fn same_key_race_once(owner_outcome: &Response) {
    const COPIES: usize = 24;
    /// Long enough that every copy arrives while admission is still
    /// pending, which is the interleaving under test.
    const ADMISSION: Duration = Duration::from_millis(400);

    let mut server = node(full_direct(), Duration::from_secs(20));
    let mut client = node(full_direct(), Duration::from_secs(20));
    let addr = listen(&mut server).await;
    let server_peer = *server.local_peer_id();
    let client_peer = *client.local_peer_id();
    connect(&mut client, &mut server, addr).await;

    for _ in 0..COPIES {
        client
            .behaviour_mut()
            .direct
            .send_request(&server_peer, request("m-race", Some("chat"), b"identical"));
    }

    let source = identity(&client_peer);
    let key = key(&source, "m-race");
    let fingerprint = direct_content_fingerprint_v1(None, b"identical").expect("fingerprint");
    // A BUDGET LARGE ENOUGH FOR ALL OF THEM, deliberately: this
    // experiment is about waiters SHARING one outcome, and a budget
    // that refused most of them would be measuring the bound instead.
    // A11 is where the bound is measured; here it must not bind.
    let mut reservations = ReservationMap::new(64, COPIES);

    // Channels waiting on ONE outcome: the owner's and every waiter's.
    let mut parked: Vec<ResponseChannel<Response>> = Vec::new();
    let mut admission_due: Option<tokio::time::Instant> = None;
    let mut enqueues = 0_usize;
    let mut waiters = 0_usize;
    let mut refused = 0_usize;
    let mut answered: Vec<Response> = Vec::new();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while answered.len() < COPIES {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            // The owner's admission completing, asynchronously, while
            // the Swarm keeps running.
            () = async {
                match admission_due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                admission_due = None;
                // ONE outcome, to everyone parked on this key.
                for channel in parked.drain(..) {
                    let _ = server.behaviour_mut().direct.send_response(channel, owner_outcome.clone());
                }
                reservations.release(&key);
            }
            e = server.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { channel, .. }, ..
                })) = e {
                    match reservations.acquire(&key, fingerprint) {
                        Ok(Reservation::Owner) => {
                            enqueues += 1;
                            // The one local enqueue. Its result is not
                            // known yet, which is the point.
                            admission_due = Some(tokio::time::Instant::now() + ADMISSION);
                            parked.push(channel);
                        }
                        Ok(Reservation::Waiter) => {
                            waiters += 1;
                            // ATTACHED, not answered.
                            parked.push(channel);
                        }
                        Err(ReservationFailure::Overloaded) => {
                            refused += 1;
                            let _ = server.behaviour_mut().direct.send_response(
                                channel, Response::Rejected { reason: Reason::Overloaded });
                        }
                        Err(ReservationFailure::Conflict) => {
                            refused += 1;
                            let _ = server.behaviour_mut().direct.send_response(
                                channel, Response::Rejected { reason: Reason::NoRoute });
                        }
                    }
                }
            }
            e = client.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Response { response, .. }, ..
                })) = e {
                    answered.push(response);
                }
            }
        }
    }

    note("copies sent", COPIES);
    note("responses received", answered.len());
    note("local enqueues (owners)", enqueues);
    note("waiters attached to the owner", waiters);
    note("refused outright", refused);
    // ATTACHED, then shared. A request the budget refused was never
    // attached and never promised the owner's outcome -- conflating the
    // two would make this assertion unfalsifiable the moment any bound
    // binds, which is exactly what happened when the waiter bound
    // landed and this line still said "every response".
    check(
        "every ATTACHED request got the owner's outcome",
        refused == 0 && answered.len() == COPIES && answered.iter().all(|r| r == owner_outcome),
    );
    // ONE local enqueue is the claim; sharing an outcome is only how it
    // shows. If `acquire` handed out several owners they would each be
    // configured with the same outcome and every channel would still
    // receive it, so the check above stays true while the sentence it
    // is standing in for has become false.
    check("exactly ONE local enqueue", enqueues == 1);
    check(
        "and every other copy attached as a waiter",
        waiters == COPIES - 1,
    );
    note("reservations still held", reservations.len());
    // REQUIRED, for BOTH outcomes, and it was only printed.
    //
    // Every check above is satisfied once the parked channels have
    // received the shared outcome, which happens whether or not the
    // owner path then releases its reservation. So a release that
    // stopped happening left this experiment passing while contradicting
    // its own recorded "reservations still held 0" -- and for the
    // rejected outcome it also contradicts the thing that makes
    // rejection survivable, which is that a later retry can become an
    // owner rather than attaching to a corpse.
    check(
        "and the owner's reservation was released",
        reservations.is_empty(),
    );
    // The consequence, stated as the caller sees it. `is_ok()` alone
    // would be true for a WAITER, which is exactly what a leaked entry
    // produces: the old reservation survives, the retry attaches to it,
    // and a check meant to rule the leak out passes BECAUSE of it.
    check(
        "so a later retry for the same key becomes an OWNER",
        matches!(
            reservations.acquire(&key, fingerprint),
            Ok(Reservation::Owner)
        ),
    );
}

/// A7 — more distinct in-flight keys than the map will hold.
///
/// Reservations are held for the whole experiment, so the budget is
/// genuinely exhausted rather than churned through.
pub async fn a7_reservation_overflow() {
    // PER-PEER, deliberately: the per-peer budget is smaller than the
    // global one, so every refusal here is the per-peer check. The
    // GLOBAL bound is a separate limit with its own failure mode, and
    // A10 is what reaches it -- this experiment alone would leave
    // broken global accounting producing exactly the same 4/12.
    const KEYS: usize = 16;
    const PER_PEER: usize = 4;

    let mut server = node(full_direct(), Duration::from_secs(20));
    let mut client = node(full_direct(), Duration::from_secs(20));
    let addr = listen(&mut server).await;
    let server_peer = *server.local_peer_id();
    let client_peer = *client.local_peer_id();
    connect(&mut client, &mut server, addr).await;

    for i in 0..KEYS {
        client.behaviour_mut().direct.send_request(
            &server_peer,
            request(&format!("m-flood-{i}"), Some("chat"), b"distinct"),
        );
    }

    let source = identity(&client_peer);
    let mut reservations = ReservationMap::new(64, PER_PEER);
    let mut owners = 0_usize;
    let mut overloaded = 0_usize;
    let mut answered: Vec<Response> = Vec::new();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while answered.len() < KEYS {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            e = server.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { request, channel, .. }, ..
                })) = e {
                    let key = key(&source, &request.message_id);
                    let fingerprint =
                        direct_content_fingerprint_v1(None, &request.body).expect("fingerprint");
                    // Held for the rest of the experiment, deliberately.
                    let response = match reservations.acquire(&key, fingerprint) {
                        Ok(Reservation::Owner) => {
                            owners += 1;
                            Response::AcceptedV2 { resolved_endpoint: "chat".to_owned() }
                        }
                        Ok(Reservation::Waiter) => {
                            // Impossible here: every key is distinct.
                            Response::AcceptedV2 { resolved_endpoint: "chat".to_owned() }
                        }
                        // OVERFLOW IS `overloaded`, not `no_route`. They
                        // are different wire reasons and an operator acts
                        // on them differently: one says retry later, the
                        // other says there is nowhere to deliver.
                        Err(ReservationFailure::Overloaded) => {
                            overloaded += 1;
                            Response::Rejected { reason: Reason::Overloaded }
                        }
                        Err(ReservationFailure::Conflict) => {
                            Response::Rejected { reason: Reason::NoRoute }
                        }
                    };
                    let _ = server.behaviour_mut().direct.send_response(channel, response);
                }
            }
            e = client.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Response { response, .. }, ..
                })) = e {
                    answered.push(response);
                }
            }
        }
    }
    let overloaded_on_the_wire = answered
        .iter()
        .filter(|r| {
            **r == Response::Rejected {
                reason: Reason::Overloaded,
            }
        })
        .count();
    note("distinct keys sent", KEYS);
    note("per-peer budget", PER_PEER);
    note("admitted (owners)", owners);
    note("refused as overloaded", overloaded);
    note(
        "peers told `overloaded` on the wire",
        overloaded_on_the_wire,
    );
    note("reservations held", reservations.len());
    // This experiment printed all of the above and asserted none of it,
    // so the per-peer bound could admit the wrong number, refuse the
    // wrong number, or answer with a reason other than `overloaded`,
    // and the run would still end in `done` and exit 0.
    check(
        "exactly the per-peer budget was admitted",
        owners == PER_PEER,
    );
    check(
        "and every excess key was refused",
        overloaded == KEYS - PER_PEER,
    );
    check(
        "with `overloaded` on the wire, not another reason",
        overloaded_on_the_wire == KEYS - PER_PEER,
    );
    // THE TOTAL, which classifying the refusals does not imply. If the
    // accepted responses never reached the client, `overloaded_on_the_wire`
    // is still KEYS - PER_PEER and the run reads as a success with four
    // outcomes missing. Same gap A10 and A11 had.
    check("every request was answered", answered.len() == KEYS);
    check(
        "and the budget's worth were accepted on the wire",
        answered
            .iter()
            .filter(|r| matches!(r, Response::AcceptedV2 { .. }))
            .count()
            == PER_PEER,
    );
    check(
        "and the map holds exactly the admitted keys",
        reservations.len() == PER_PEER,
    );
}

/// A8 — a cancellation race, not merely a slow race.
///
/// `SPIKES.md` requires "response timeout/cancellation races" alongside
/// the same-key retransmission claim A6 tests. A6 kept every response
/// channel alive until an outcome was sent; nothing there ever cancels.
/// This does: the connection carrying the OWNER's request is killed
/// mid-admission, while waiters on a SEPARATE connection remain — the
/// same key, genuinely split across two connections, because two
/// `Swarm`s built from one shared keypair present one `source_peer` for
/// `DedupKey` purposes while being physically distinct connections.
///
/// The question: does an owner's connection dying orphan the surviving
/// waiters, or leak the reservation forever? Production code has to get
/// this right; nothing forces it to without a test that can fail this
/// way.
pub async fn a8_cancellation_race() {
    const ADMISSION: Duration = Duration::from_millis(600);
    const SURVIVING_WAITERS: usize = 4;

    let shared = libp2p::identity::Keypair::generate_ed25519();
    let source = identity(&PeerId::from(shared.public()));

    let mut server = node(full_direct(), Duration::from_secs(20));
    let addr = listen(&mut server).await;
    let server_peer = *server.local_peer_id();

    let mut owner_conn = node_as(shared.clone(), full_direct(), Duration::from_secs(20));
    let mut waiter_conn = node_as(shared, full_direct(), Duration::from_secs(20));
    connect(&mut owner_conn, &mut server, addr.clone()).await;
    connect(&mut waiter_conn, &mut server, addr).await;

    // THE OWNER, alone on its own connection.
    owner_conn
        .behaviour_mut()
        .direct
        .send_request(&server_peer, request("m-cancel", Some("chat"), b"canceled"));

    let key = key(&source, "m-cancel");
    let fingerprint = direct_content_fingerprint_v1(None, b"canceled").expect("fingerprint");
    let mut reservations = ReservationMap::new(64, 8);

    let mut owner_connection_id = None;
    let mut waiter_channels: Vec<ResponseChannel<Response>> = Vec::new();
    let mut admission_due: Option<tokio::time::Instant> = None;
    let mut killed = false;
    let mut owner_inbound_failure: Option<String> = None;
    let mut waiter_answers = 0_usize;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if waiter_answers >= SURVIVING_WAITERS {
            break;
        }
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {
                check("every surviving waiter answered before the deadline", false);
                break;
            }
            () = async {
                match admission_due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                admission_due = None;
                // THE RACE HAS TO HAVE HAPPENED FIRST.
                //
                // `close_connection` only REQUESTS teardown, and
                // `killed` was set the instant it returned. If the
                // connection is still alive when this timer fires,
                // completing admission here answers the waiters in the
                // ordinary way, releases the reservation in the
                // ordinary way, and every check passes without a
                // connection-death race ever having occurred -- the
                // experiment reporting success for the one scenario it
                // does not exercise.
                //
                // So the observation gates it: wait until the server
                // has actually seen the owner's connection fail. The
                // outer deadline still bounds this, and expiring there
                // fails the run rather than passing it, which is the
                // honest outcome for a race that would not start.
                if owner_inbound_failure.is_none() {
                    admission_due = Some(tokio::time::Instant::now() + ADMISSION);
                } else {
                // THE OWNER'S CONNECTION IS ALREADY GONE. Its channel is
                // not answered -- there is nowhere for the answer to go
                // -- but the waiters on the surviving connection still
                // get one, and the reservation is released either way.
                for channel in waiter_channels.drain(..) {
                    let _ = server.behaviour_mut().direct.send_response(
                        channel,
                        Response::AcceptedV2 { resolved_endpoint: "chat".to_owned() },
                    );
                }
                reservations.release(&key);
                note("reservation released after the owner's connection died", true);
                }
            }
            e = server.select_next_some() => match e {
                SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { channel, .. }, connection_id, ..
                })) => {
                    match reservations.acquire(&key, fingerprint) {
                        Ok(Reservation::Owner) => {
                            owner_connection_id = Some(connection_id);
                            admission_due = Some(tokio::time::Instant::now() + ADMISSION);
                            note("owner admitted, on its own connection", format!("{connection_id:?}"));
                            // KILL IT NOW, before admission has a chance
                            // to complete. The channel is dropped with
                            // it; there is no cancel API for one
                            // request, so this is what a cancellation
                            // looks like at this layer -- the connection
                            // carrying it is gone.
                            server.close_connection(connection_id);
                            killed = true;
                            // SENT HERE, synchronously with the kill:
                            // every one of these genuinely arrives while
                            // admission is pending on a connection that
                            // no longer exists, which is the scenario
                            // under test.
                            for _ in 0..SURVIVING_WAITERS {
                                waiter_conn.behaviour_mut().direct.send_request(
                                    &server_peer, request("m-cancel", Some("chat"), b"canceled"));
                            }
                        }
                        Ok(Reservation::Waiter) => {
                            waiter_channels.push(channel);
                        }
                        Err(_) => {}
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::InboundFailure { connection_id, error, .. })) => {
                    if Some(connection_id) == owner_connection_id {
                        owner_inbound_failure = Some(describe_inbound(&error));
                    }
                }
                _ => {}
            },
            e = owner_conn.select_next_some() => { let _ = e; }
            e = waiter_conn.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Response { .. }, ..
                })) = e {
                    waiter_answers += 1;
                }
            }
        }
    }

    // ASKED, which is not the same as HAPPENED -- see the admission
    // branch above, where the distinction is now load-bearing.
    check(
        "owner's connection close was requested mid-admission",
        killed,
    );
    let observed_death = owner_inbound_failure.is_some();
    note(
        "server learned the owner's connection died",
        owner_inbound_failure
            .clone()
            .unwrap_or_else(|| "no InboundFailure event arrived".to_owned()),
    );
    // THE PRECONDITION OF THE WHOLE EXPERIMENT. Without it the run
    // exercised an ordinary admission and reported it as a survived
    // race.
    check(
        "and the server OBSERVED it die before admission completed",
        observed_death,
    );
    note(
        "surviving waiters that still received an answer",
        waiter_answers,
    );
    note(
        "reservations held after the race settled",
        reservations.len(),
    );
    // `is_ok()` was true for a WAITER as well, which is what a leaked
    // reservation produces: the old entry survives, the new request
    // attaches to it, and the check that exists to rule the leak out
    // passes BECAUSE of the leak. Requiring OWNER is what distinguishes
    // "released" from "still there and I joined it".
    check(
        "the race left no reservation behind",
        reservations.is_empty(),
    );
    check(
        "so a NEW request for the same key becomes an OWNER, not a waiter",
        matches!(
            reservations.acquire(&key, fingerprint),
            Ok(Reservation::Owner)
        ),
    );
}

/// A9 -- the `no_route` privacy class, driven through the PRODUCTION
/// routing predicates.
///
/// Two earlier versions of this experiment were tautologies. The first
/// never ran the case at all. The second converted a label the request
/// carried into an enum and passed it to a function that discarded its
/// argument and returned `NoRoute` -- so it measured whether a
/// function that ignores its input returns the same thing for every
/// input, and reported the answer as a VERDICT. Review caught both.
///
/// What the property actually rests on is `EndpointRegistry::
/// resolve_inbound`, whose five refusals are selected by five
/// independent predicates over real registry state -- is the endpoint
/// configured, is it enabled, does the endpoint policy admit the
/// sender, is anything leasing it, is there a default -- and
/// `ResolveFailure::to_wire`, the production collapse. This experiment
/// puts the responder in five genuinely different registry STATES,
/// lets the production code decide, and checks two things: that five
/// DISTINCT local failures were produced (the predicates are
/// independent, so a bug in any one of them would show), and that all
/// five reach the wire as one byte-identical answer.
///
/// The request carries the destination it asks for, exactly as Stage 6
/// will receive it; nothing in the harness maps a label to an outcome.
pub async fn a9_no_route_is_one_answer() {
    use interweave_transport_api::EndpointId;
    use interweave_transport_runtime::{EndpointRegistry, RegisteredEndpoint, ResolveFailure};
    use std::collections::BTreeMap;

    /// One registry per scenario, each in the state that makes ONE
    /// predicate in `resolve_inbound` refuse. Which one refuses is the
    /// production code's decision; the harness only arranges the state.
    struct Scenario {
        label: &'static str,
        registry: EndpointRegistry,
        /// The destination the REQUEST names -- what a remote peer
        /// would actually send, passed through untouched.
        destination: Option<&'static str>,
        /// The endpoint-policy answer for this sender. Profile trust is
        /// the caller's concern in production too; this closure is
        /// exactly the argument `resolve_inbound` takes.
        policy_admits: bool,
    }

    fn chat() -> EndpointId {
        EndpointId::parse("chat").expect("endpoint")
    }
    fn with_chat(enabled: bool) -> EndpointRegistry {
        let mut endpoints = BTreeMap::new();
        endpoints.insert(
            chat(),
            RegisteredEndpoint {
                enabled,
                ..RegisteredEndpoint::default()
            },
        );
        EndpointRegistry::new(endpoints, Some(chat()))
    }

    let scenarios = vec![
        // Configured endpoints exist, but not the one asked for.
        Scenario {
            label: "unknown",
            registry: with_chat(true),
            destination: Some("absent"),
            policy_admits: true,
        },
        // Asked-for endpoint exists and is disabled.
        Scenario {
            label: "disabled",
            registry: with_chat(false),
            destination: Some("chat"),
            policy_admits: true,
        },
        // Exists, enabled, admitted -- and nothing is leasing it.
        Scenario {
            label: "unleased",
            registry: with_chat(true),
            destination: Some("chat"),
            policy_admits: true,
        },
        // No destination named and no default configured.
        Scenario {
            label: "nodefault",
            registry: EndpointRegistry::new(BTreeMap::new(), None),
            destination: None,
            policy_admits: true,
        },
        // Exists, enabled -- and the endpoint policy excludes this sender.
        Scenario {
            label: "denied",
            registry: with_chat(true),
            destination: Some("chat"),
            policy_admits: false,
        },
    ];

    let mut server = node(full_direct(), Duration::from_secs(20));
    let mut client = node(full_direct(), Duration::from_secs(20));
    let addr = listen(&mut server).await;
    let server_peer = *server.local_peer_id();
    connect(&mut client, &mut server, addr).await;

    for sc in &scenarios {
        client.behaviour_mut().direct.send_request(
            &server_peer,
            request(&format!("m-route-{}", sc.label), sc.destination, b"probe"),
        );
    }

    // What the PRODUCTION code decided locally, per scenario.
    let mut local: Vec<(&'static str, ResolveFailure)> = Vec::new();
    let mut answers: Vec<Response> = Vec::new();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while answers.len() < scenarios.len() {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            e = server.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { request, channel, .. }, ..
                })) = e {
                    let sc = scenarios
                        .iter()
                        .find(|s| request.message_id == format!("m-route-{}", s.label))
                        .expect("every request names a scenario");
                    // THE DESTINATION IS PASSED THROUGH, not mapped.
                    let requested = request
                        .destination
                        .as_deref()
                        .map(|d| EndpointId::parse(d).expect("endpoint grammar"));
                    let admits = sc.policy_admits;
                    let outcome = sc
                        .registry
                        .resolve_inbound(requested.as_ref(), |_| admits);
                    let response = match outcome {
                        Ok(_) => Response::AcceptedV2 { resolved_endpoint: "chat".to_owned() },
                        Err(failure) => {
                            local.push((sc.label, failure));
                            // THE PRODUCTION COLLAPSE, not a harness one.
                            Response::Rejected { reason: failure.to_wire() }
                        }
                    };
                    let _ = server.behaviour_mut().direct.send_response(channel, response);
                }
            }
            e = client.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Response { response, .. }, ..
                })) = e {
                    answers.push(response);
                }
            }
        }
    }

    // FIVE DIFFERENT LOCAL FAILURES. This is the half the tautology
    // lacked: proof that five independent predicates each fired, so a
    // regression in any one of them -- or two collapsing into one
    // upstream of the encoder -- would show here.
    let mut distinct: Vec<ResolveFailure> = local.iter().map(|(_, f)| *f).collect();
    distinct.sort_by_key(|f| format!("{f:?}"));
    distinct.dedup();
    for (label, failure) in &local {
        note(&format!("  {label:<10} -> local"), format!("{failure:?}"));
    }
    note("scenarios exercised", scenarios.len());
    note(
        "distinct LOCAL failures the production predicates produced",
        distinct.len(),
    );
    note("responses received", answers.len());

    let all_same = answers.windows(2).all(|w| w[0] == w[1]);
    let all_no_route = answers.iter().all(|r| {
        *r == Response::Rejected {
            reason: Reason::NoRoute,
        }
    });
    check("every response decodes to one identical value", all_same);
    check("and that value is no_route", all_no_route);

    // ENCODED equality through the production type's own serde and the
    // codec's own CBOR library -- the bytes the wire carries.
    let encodings: Vec<Vec<u8>> = answers.iter().map(serde_cbor_bytes).collect();
    let bytes_identical = encodings.windows(2).all(|w| w[0] == w[1]);
    check("and every encoding is byte-identical", bytes_identical);
    check(
        "VERDICT: five independent refusals, one wire answer",
        distinct.len() == scenarios.len()
            && answers.len() == scenarios.len()
            && all_same
            && all_no_route
            && bytes_identical,
    );
}

/// Encode exactly as the codec does, so the byte comparison above is
/// about the wire and not about `Debug`.
fn serde_cbor_bytes(response: &Response) -> Vec<u8> {
    let mut out = Vec::new();
    cbor4ii::serde::to_writer(&mut out, response).expect("encode");
    out
}

/// A10 -- the GLOBAL reservation budget, reached by many peers.
///
/// A7 sends everything from one peer against a per-peer limit smaller
/// than the global one, so the global bound is never touched: broken
/// global accounting would have produced A7's documented 4/12 exactly.
/// `DIRECT.md` states the two limits separately ("128 global / 8 per
/// source PeerId by default"), so a spike that only ever reached one of
/// them has evidence about one of them.
///
/// Here the per-peer budget is generous and the global one is small,
/// and enough DISTINCT source peers connect that only the global limit
/// can be what refuses.
pub async fn a10_global_reservation_budget() {
    const PEERS: usize = 8;
    const MAX_GLOBAL: usize = 3;
    const PER_PEER: usize = 8;

    let mut server = node(full_direct(), Duration::from_secs(20));
    let addr = listen(&mut server).await;
    let server_peer = *server.local_peer_id();

    // A DISTINCT IDENTITY PER CLIENT, so every reservation is charged
    // to a different `source_peer` and the per-peer budget -- eight,
    // versus one request each -- cannot be what refuses any of them.
    let mut clients = Vec::new();
    for _ in 0..PEERS {
        let mut client = node(full_direct(), Duration::from_secs(20));
        connect(&mut client, &mut server, addr.clone()).await;
        clients.push(client);
    }

    for (i, client) in clients.iter_mut().enumerate() {
        client.behaviour_mut().direct.send_request(
            &server_peer,
            request(&format!("m-global-{i}"), Some("chat"), b"distinct"),
        );
    }

    let mut reservations = ReservationMap::new(MAX_GLOBAL, PER_PEER);
    let mut owners = 0_usize;
    let mut overloaded = 0_usize;
    let mut answered = 0_usize;
    let mut accepted_on_the_wire = 0_usize;
    let mut overloaded_on_the_wire = 0_usize;
    let mut unexpected_on_the_wire = 0_usize;
    let mut charged_to: Vec<TransportIdentity> = Vec::new();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while answered < PEERS {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            e = server.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { request, channel, .. }, peer, ..
                })) = e {
                    // THE SOURCE IS THE CONNECTED PEER, so each of the
                    // eight is a genuinely different accounting key
                    // rather than eight requests wearing one.
                    let source = identity(&peer);
                    let key = key(&source, &request.message_id);
                    let fingerprint =
                        direct_content_fingerprint_v1(None, &request.body).expect("fingerprint");
                    let response = match reservations.acquire(&key, fingerprint) {
                        Ok(Reservation::Owner) => {
                            owners += 1;
                            charged_to.push(source);
                            Response::AcceptedV2 { resolved_endpoint: "chat".to_owned() }
                        }
                        Ok(Reservation::Waiter) => {
                            Response::AcceptedV2 { resolved_endpoint: "chat".to_owned() }
                        }
                        Err(ReservationFailure::Overloaded) => {
                            overloaded += 1;
                            Response::Rejected { reason: Reason::Overloaded }
                        }
                        Err(ReservationFailure::Conflict) => {
                            Response::Rejected { reason: Reason::NoRoute }
                        }
                    };
                    let _ = server.behaviour_mut().direct.send_response(channel, response);
                }
            }
            e = futures::future::select_all(
                clients.iter_mut().map(|c| Box::pin(c.select_next_some()))
            ) => {
                let (event, _, _) = e;
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Response { response, .. }, ..
                })) = event {
                    answered += 1;
                    match response {
                        Response::AcceptedV2 { .. } => accepted_on_the_wire += 1,
                        Response::Rejected { reason: Reason::Overloaded } => {
                            overloaded_on_the_wire += 1;
                        }
                        _ => unexpected_on_the_wire += 1,
                    }
                }
            }
        }
    }

    let distinct_sources: std::collections::BTreeSet<&TransportIdentity> =
        charged_to.iter().collect();
    note("distinct source peers", PEERS);
    note("global budget", MAX_GLOBAL);
    note(
        "per-peer budget (generous, so it cannot be the refuser)",
        PER_PEER,
    );
    note("responses received", answered);
    note("admitted (owners)", owners);
    note("refused as overloaded", overloaded);
    note(
        "distinct peers actually charged a reservation",
        distinct_sources.len(),
    );
    note("accepted ON THE WIRE", accepted_on_the_wire);
    note("overloaded ON THE WIRE", overloaded_on_the_wire);
    note("unexpected responses", unexpected_on_the_wire);

    // THE CLIENTS' SIDE, which the server-side counters do not imply.
    // The verdict checked only `owners`, `overloaded` and the distinct
    // sources — all decided at request time — so a run where responses
    // were lost, timed out, or came back with an unexpected reason left
    // `answered < PEERS` at the deadline and still passed. The
    // experiment is about what the budget DOES to callers; a caller
    // that never heard is not a caller that was refused.
    check("every request was answered", answered == PEERS);
    check(
        "the global budget's worth were accepted on the wire",
        accepted_on_the_wire == MAX_GLOBAL,
    );
    check(
        "and every other request was refused as Overloaded on the wire",
        overloaded_on_the_wire == PEERS - MAX_GLOBAL,
    );
    check(
        "with no other response shape at all",
        unexpected_on_the_wire == 0,
    );

    check(
        "VERDICT: the GLOBAL budget refused the excess",
        owners == MAX_GLOBAL
            && overloaded == PEERS - MAX_GLOBAL
            && distinct_sources.len() == owners
            && answered == PEERS
            && accepted_on_the_wire == MAX_GLOBAL
            && overloaded_on_the_wire == PEERS - MAX_GLOBAL
            && unexpected_on_the_wire == 0,
    );
}

/// A11 -- the waiters attached to ONE key.
///
/// A6 proved waiters share the owner's outcome; A7 and A10 proved the
/// reservation map is bounded by requests from many peers and many
/// keys. None of them asked how many waiters ONE key may accumulate,
/// and the first answer this experiment measured was: unbounded.
/// `ReservationMap::acquire` matched an existing key and returned
/// `Waiter` before consulting either budget, so a peer flooding
/// matching retransmissions while the owner awaited endpoint admission
/// never received `Overloaded` however many it sent -- 39 waiters from
/// 40 copies, zero refusals.
///
/// Each waiter costs a held `ResponseChannel`: A4 established that
/// holding one across an await is legitimate and A6 that every waiter
/// must be held until the owner resolves, so the cost is real and per
/// request. That made it a memory-exhaustion path, and the pattern
/// Stage 6 derives from A6 would have inherited it.
///
/// Fixed in the production `ReservationMap` rather than worked around
/// here: waiters are charged against the same per-peer and global
/// budgets owners are, and releasing the key returns all of it. This
/// experiment now measures that the MAP refuses, with no cap of the
/// caller's own.
pub async fn a11_same_key_waiter_flood() {
    const COPIES: usize = 40;
    const PER_PEER: usize = 8;
    const ADMISSION: Duration = Duration::from_millis(600);

    let mut server = node(full_direct(), Duration::from_secs(20));
    let mut client = node(full_direct(), Duration::from_secs(20));
    let addr = listen(&mut server).await;
    let server_peer = *server.local_peer_id();
    let client_peer = *client.local_peer_id();
    connect(&mut client, &mut server, addr).await;

    for _ in 0..COPIES {
        client
            .behaviour_mut()
            .direct
            .send_request(&server_peer, request("m-flood", Some("chat"), b"identical"));
    }

    let source = identity(&client_peer);
    let key = key(&source, "m-flood");
    let fingerprint = direct_content_fingerprint_v1(None, b"identical").expect("fingerprint");
    let mut reservations = ReservationMap::new(128, PER_PEER);

    let mut parked: Vec<ResponseChannel<Response>> = Vec::new();
    let mut admission_due: Option<tokio::time::Instant> = None;
    let mut owners = 0_usize;
    let mut waiters = 0_usize;
    let mut overloaded = 0_usize;
    let mut high_water = 0_usize;
    let mut answered = 0_usize;
    // WHAT CAME BACK ON THE WIRE, classified. `answered` alone counts
    // responses without asking what they said, so a run full of
    // unexpected `NoRoute` refusals looked identical to the run this
    // experiment is trying to observe.
    let mut accepted_on_the_wire = 0_usize;
    let mut overloaded_on_the_wire = 0_usize;
    let mut unexpected_on_the_wire = 0_usize;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while answered < COPIES {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            () = async {
                match admission_due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                admission_due = None;
                for channel in parked.drain(..) {
                    let _ = server.behaviour_mut().direct.send_response(
                        channel,
                        Response::AcceptedV2 { resolved_endpoint: "chat".to_owned() },
                    );
                }
                reservations.release(&key);
            }
            e = server.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { channel, .. }, ..
                })) = e {
                    // NO CAP OF THE HARNESS'S OWN. Whatever bounds this
                    // is the map's doing, which is the point: Stage 6
                    // inherits the bound rather than having to remember
                    // to add one.
                    match reservations.acquire(&key, fingerprint) {
                        Ok(Reservation::Owner) => {
                            owners += 1;
                            admission_due = Some(tokio::time::Instant::now() + ADMISSION);
                            parked.push(channel);
                        }
                        Ok(Reservation::Waiter) => {
                            waiters += 1;
                            parked.push(channel);
                        }
                        Err(ReservationFailure::Overloaded) => {
                            overloaded += 1;
                            let _ = server.behaviour_mut().direct.send_response(
                                channel, Response::Rejected { reason: Reason::Overloaded });
                        }
                        Err(ReservationFailure::Conflict) => {
                            let _ = server.behaviour_mut().direct.send_response(
                                channel, Response::Rejected { reason: Reason::NoRoute });
                        }
                    }
                    high_water = high_water.max(parked.len());
                }
            }
            e = client.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Response { response, .. }, ..
                })) = e {
                    answered += 1;
                    match response {
                        Response::AcceptedV2 { .. } => accepted_on_the_wire += 1,
                        Response::Rejected { reason: Reason::Overloaded } => {
                            overloaded_on_the_wire += 1;
                        }
                        _ => unexpected_on_the_wire += 1,
                    }
                }
            }
        }
    }

    note("same-key copies sent", COPIES);
    note("responses received", answered);
    note("per-peer budget", PER_PEER);
    note("owners", owners);
    note("waiters attached", waiters);
    note("refused as overloaded BY THE MAP", overloaded);
    note("accepted ON THE WIRE", accepted_on_the_wire);
    note("overloaded ON THE WIRE", overloaded_on_the_wire);
    note("unexpected responses", unexpected_on_the_wire);
    note("highest number of channels held at once", high_water);

    // EVERY REQUEST HAS TO BE ACCOUNTED FOR, and the old verdict did not
    // require that. `owners == 1 && overloaded == COPIES - PER_PEER`
    // classifies 1 + 32 = 33 of 40; the remaining seven -- the WAITERS,
    // which is the bound this experiment exists to measure -- were
    // merely printed. A run where seven requests never reached the
    // server at all satisfied the expression and closed the
    // waiter-bound experiment as a success.
    //
    // The loop can also exit on its deadline with `answered < COPIES`,
    // so the count is not implied by having got here.
    check("every request was answered", answered == COPIES);
    check("exactly one owner", owners == 1);
    check(
        "and exactly PER_PEER - 1 waiters attached to it",
        waiters == PER_PEER - 1,
    );
    check(
        "the map never held more than the budget",
        high_water <= PER_PEER,
    );
    check(
        "the rest were refused as overloaded BY THE MAP",
        overloaded == COPIES - PER_PEER,
    );
    // ...and the WIRE agrees with the map. Counting only the map's own
    // decisions would accept a run where the refusals never reached the
    // client, or reached it wearing a different reason.
    check(
        "the budget's worth were accepted on the wire",
        accepted_on_the_wire == PER_PEER,
    );
    check(
        "and every other request was refused as Overloaded on the wire",
        overloaded_on_the_wire == COPIES - PER_PEER,
    );
    check(
        "with no other response shape at all",
        unexpected_on_the_wire == 0,
    );

    check(
        "VERDICT: one key cannot accumulate unbounded waiters",
        answered == COPIES
            && owners == 1
            && waiters == PER_PEER - 1
            && high_water <= PER_PEER
            && overloaded == COPIES - PER_PEER
            && accepted_on_the_wire == PER_PEER
            && overloaded_on_the_wire == COPIES - PER_PEER
            && unexpected_on_the_wire == 0,
    );
}
