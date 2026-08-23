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

/// The coarse wire reasons this spike can produce.
///
/// `DIRECT.md` lists seven and is explicit that they are DISTINCT on the
/// wire. Collapsing reservation overflow into `no_route` -- which the
/// first version of this harness did -- means the spike reports
/// `Overloaded` in its own counters while the peer is told something
/// else, so it verifies neither the mapping nor the behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reason {
    /// Endpoint unknown, offline, disabled, or policy-denied -- one
    /// answer, so the reply cannot be used as an endpoint oracle.
    NoRoute,
    /// A bounded budget is exhausted. Distinct from `no_route`: the route
    /// may be perfectly good and the caller should retry later.
    Overloaded,
}

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
    loop {
        if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
            return address;
        }
    }
}

/// One line of evidence.
pub fn note(what: &str, detail: impl std::fmt::Display) {
    println!("  {what:<46} {detail}");
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
    while up < 2 {
        tokio::select! {
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
    client
        .behaviour_mut()
        .direct
        .send_request(&server_peer, request("m-1", Some("chat"), b"hello"));
    client
        .behaviour_mut()
        .direct
        .send_request(&server_peer, request("m-2", None, b"hello"));

    let mut answered = 0;
    let mut seen_explicit = false;
    let mut seen_default = false;
    while answered < 2 {
        tokio::select! {
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
                    message: RrMessage::Response { response, .. }, ..
                })) = e {
                    note("response", format!("{response:?}"));
                    answered += 1;
                }
            }
        }
    }
    note("explicit destination carried", seen_explicit);
    note("omitted destination carried", seen_default);
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
                    break;
                }
            }
        }
    }
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
    while !(direct_seen && endpoints_seen) {
        tokio::select! {
            e = server.select_next_some() => match e {
                SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { channel, .. }, ..
                })) => {
                    direct_seen = true;
                    let _ = server.behaviour_mut().direct.send_response(
                        channel, Response::AcceptedV2 { resolved_endpoint: "chat".to_owned() });
                }
                SwarmEvent::Behaviour(BehaviourEvent::Endpoints(RrEvent::Message {
                    message: RrMessage::Request { channel, .. }, ..
                })) => {
                    endpoints_seen = true;
                    let _ = server.behaviour_mut().endpoints.send_response(
                        channel, Response::AcceptedV2 { resolved_endpoint: "default".to_owned() });
                }
                _ => {}
            },
            _ = client.select_next_some() => {}
        }
    }
    note("both families answered", direct_seen && endpoints_seen);
    // THE REUSE QUESTION. Two protocol families, one connection: the
    // responder must not be opening a connection per family.
    note(
        "connections the responder holds",
        server.network_info().num_peers(),
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
    let mut sent_prompt = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !prompt_answered {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {
                note("prompt request answered while one was held", false);
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
                    message: RrMessage::Response { .. }, ..
                })) = e {
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
    note("a second peer was served while one was held", true);

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
                note("the held response arrived late", false);
                break;
            }
            _ = server.select_next_some() => {}
            e = slow.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Response { .. }, ..
                })) = e {
                    note("the held response arrived late", true);
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while outbound.is_none() || inbound.is_none() {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            e = server.select_next_some() => match e {
                SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::Message {
                    message: RrMessage::Request { channel, .. }, ..
                })) => { kept = Some(channel); }
                SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::InboundFailure { error, .. })) => {
                    inbound = Some(describe_inbound(&error));
                }
                _ => {}
            },
            e = client.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Direct(RrEvent::OutboundFailure { error, .. })) = e {
                    outbound = Some(describe_outbound(&error));
                }
            }
        }
    }
    note("requester sees", outbound.unwrap_or_else(|| "nothing".to_owned()));
    note("responder sees", inbound.unwrap_or_else(|| "nothing".to_owned()));
    // The responder is still holding a channel it can no longer answer:
    // `is_open` is false, so a late `send_response` has nowhere to go.
    note("responder still holds a channel", kept.is_some());
    note(
        "and that channel is still answerable",
        kept.is_some_and(|c| c.is_open()),
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
    let mut reservations = ReservationMap::new(64, 8);

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
    note(
        "every response is the owner's outcome",
        answered.len() == COPIES && answered.iter().all(|r| r == owner_outcome),
    );
    note("reservations still held", reservations.len());
}

/// A7 — more distinct in-flight keys than the map will hold.
///
/// Reservations are held for the whole experiment, so the budget is
/// genuinely exhausted rather than churned through.
pub async fn a7_reservation_overflow() {
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
        .filter(|r| **r == Response::Rejected { reason: Reason::Overloaded })
        .count();
    note("distinct keys sent", KEYS);
    note("per-peer budget", PER_PEER);
    note("admitted (owners)", owners);
    note("refused as overloaded", overloaded);
    note("peers told `overloaded` on the wire", overloaded_on_the_wire);
    note("reservations held", reservations.len());
}
