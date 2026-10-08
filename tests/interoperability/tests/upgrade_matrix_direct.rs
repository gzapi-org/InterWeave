// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The upgrade matrix's direct-wire rows that need no older build
//! (testing.md §Compatibility fixtures, A 2026-10-08), between HEAD's
//! composed runtime and a raw libp2p peer over real sockets:
//!
//! - **unsupported major**: a peer that lists only `/interweave/direct/3.0.0`
//!   gets `UnsupportedProtocols` when it sends to HEAD, and HEAD's send to
//!   it fails `ProtocolUnsupported`. No request is read on either side.
//! - **minor bump**: a peer that lists `/interweave/direct/2.1.0` beside
//!   `2.0.0` (DIRECT.md §Protocol family, A 2026-10-08) exchanges with
//!   HEAD on 2.0.0 in both directions. Neither side ever reads a byte on
//!   2.1.0.
//!
//! The raw peer stands in for a newer build. Every frame it writes or
//! reads goes through the INDEPENDENT codecs, so each exchange is also a
//! cross-implementation one. Each refusal has a control beside it: the
//! same peer, listing 2.0.0, exchanges.
//!
//! Both runtimes listen on the host's private-range address: ADR-0052
//! refuses a loopback address a peer supplies, so HEAD would never dial
//! the peer on 127.0.0.1.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::net::Ipv4Addr;
use std::time::Duration;

use futures::{AsyncReadExt as _, AsyncWriteExt as _, StreamExt as _};
use interweave_independent_codecs::direct_response_v2::DirectResponseV2;
use interweave_independent_codecs::direct_v2::DirectMessageV2 as IndependentFrame;
use interweave_local_client_api::{
    DataCapability, DataSessionBinding as _, DataSessionPort as _, SessionEvent, SessionRequest,
};
use interweave_local_client_conformance_tests::{PATIENCE, receive};
use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    DirectDestination, EndpointId, MessageId, Payload, TransportError, TransportIdentity,
    TransportRuntime as _,
};
use interweave_transport_composition::{ComposedRuntime, CompositionOptions};
use libp2p::request_response::{self, Codec, OutboundFailure, ProtocolSupport};
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId, StreamProtocol, SwarmBuilder};
use tokio::sync::{mpsc, oneshot};

const V2_0: &str = "/interweave/direct/2.0.0";
const V2_1: &str = "/interweave/direct/2.1.0";
const V3_0: &str = "/interweave/direct/3.0.0";

/// What the raw peer read: the protocol id the stream negotiated, and the
/// bytes.
#[derive(Debug, Clone)]
struct Read {
    protocol: String,
    bytes: Vec<u8>,
}

/// Bytes in, bytes out, with the negotiated protocol id recorded on every
/// read: that id is the row's evidence.
#[derive(Clone, Default)]
struct RawCodec;

impl Codec for RawCodec {
    type Protocol = StreamProtocol;
    type Request = Read;
    type Response = Read;

    async fn read_request<T>(&mut self, p: &StreamProtocol, io: &mut T) -> std::io::Result<Read>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let mut bytes = Vec::new();
        io.take(64 * 1024).read_to_end(&mut bytes).await?;
        Ok(Read {
            protocol: p.to_string(),
            bytes,
        })
    }

    async fn read_response<T>(&mut self, p: &StreamProtocol, io: &mut T) -> std::io::Result<Read>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let mut bytes = Vec::new();
        io.take(1024).read_to_end(&mut bytes).await?;
        Ok(Read {
            protocol: p.to_string(),
            bytes,
        })
    }

    async fn write_request<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        r: Read,
    ) -> std::io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        io.write_all(&r.bytes).await?;
        io.close().await
    }

    async fn write_response<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        r: Read,
    ) -> std::io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        io.write_all(&r.bytes).await?;
        io.close().await
    }
}

type Outcome = Result<Read, String>;

/// A raw peer listing `protocols`, in that order: the order a newer build
/// lists them, newest first.
struct RawPeer {
    id: PeerId,
    address: Multiaddr,
    send: mpsc::Sender<(Multiaddr, PeerId, Vec<u8>, oneshot::Sender<Outcome>)>,
    inbound: mpsc::Receiver<Read>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for RawPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RawPeer {
    async fn start(ip: Ipv4Addr, protocols: &[&'static str]) -> Self {
        let mut swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .expect("the transport stack")
            .with_behaviour(|_| {
                request_response::Behaviour::<RawCodec>::new(
                    protocols
                        .iter()
                        .map(|p| (StreamProtocol::new(p), ProtocolSupport::Full)),
                    request_response::Config::default(),
                )
            })
            .expect("the behaviour")
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();
        let id = *swarm.local_peer_id();
        swarm
            .listen_on(format!("/ip4/{ip}/tcp/0").parse().expect("an address"))
            .expect("listens");
        let address = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                break address;
            }
        };
        let (send, mut commands) =
            mpsc::channel::<(Multiaddr, PeerId, Vec<u8>, oneshot::Sender<Outcome>)>(4);
        let (inbound_tx, inbound) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            let mut waiting: Vec<(PeerId, Vec<u8>, oneshot::Sender<Outcome>)> = Vec::new();
            let mut pending = std::collections::HashMap::new();
            loop {
                tokio::select! {
                    Some((addr, to, bytes, reply)) = commands.recv() => {
                        if swarm.is_connected(&to) {
                            let rid = swarm.behaviour_mut().send_request(&to, Read { protocol: String::new(), bytes });
                            pending.insert(rid, reply);
                        } else {
                            let _ = swarm.dial(addr);
                            waiting.push((to, bytes, reply));
                        }
                    }
                    event = swarm.select_next_some() => match event {
                        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                            let (now, later): (Vec<_>, Vec<_>) =
                                waiting.drain(..).partition(|(p, _, _)| *p == peer_id);
                            waiting = later;
                            for (to, bytes, reply) in now {
                                let rid = swarm.behaviour_mut().send_request(&to, Read { protocol: String::new(), bytes });
                                pending.insert(rid, reply);
                            }
                        }
                        SwarmEvent::OutgoingConnectionError { peer_id: Some(p), error, .. } => {
                            for (_, _, reply) in waiting.extract_if(.., |(q, _, _)| *q == p) {
                                let _ = reply.send(Err(format!("dial: {error}")));
                            }
                        }
                        SwarmEvent::Behaviour(request_response::Event::Message { message, .. }) => match message {
                            request_response::Message::Request { request, channel, .. } => {
                                let answer = answer_for(&request.bytes);
                                let _ = swarm.behaviour_mut().send_response(channel, Read { protocol: String::new(), bytes: answer });
                                let _ = inbound_tx.send(request).await;
                            }
                            request_response::Message::Response { request_id, response } => {
                                if let Some(reply) = pending.remove(&request_id) {
                                    let _ = reply.send(Ok(response));
                                }
                            }
                        },
                        SwarmEvent::Behaviour(request_response::Event::OutboundFailure { request_id, error, .. }) => {
                            if let Some(reply) = pending.remove(&request_id) {
                                let _ = reply.send(Err(match error {
                                    OutboundFailure::UnsupportedProtocols => "UnsupportedProtocols".to_owned(),
                                    other => format!("{other}"),
                                }));
                            }
                        }
                        _ => {}
                    },
                }
            }
        });
        Self {
            id,
            address,
            send,
            inbound,
            task,
        }
    }

    fn route(&self) -> String {
        format!("{}/p2p/{}", self.address, self.id)
    }

    /// Send `frame` to `to` at `addr` and wait for the outcome.
    async fn request(&self, addr: Multiaddr, to: PeerId, frame: Vec<u8>) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        self.send
            .send((addr, to, frame, reply))
            .await
            .expect("the peer runs");
        tokio::time::timeout(PATIENCE, outcome)
            .await
            .expect("an outcome in time")
            .expect("the peer answers")
    }
}

/// The raw peer accepts every legal request into `human`, read and
/// answered by the independent codecs; anything else is `malformed`.
fn answer_for(request: &[u8]) -> Vec<u8> {
    let response = match IndependentFrame::decode(request) {
        Ok(frame) => DirectResponseV2::Accepted {
            message_id: frame.message_id,
            resolved_destination_endpoint: frame
                .destination_endpoint
                .unwrap_or_else(|| "human".to_owned()),
        },
        Err(_) => DirectResponseV2::Rejected {
            message_id: request
                .get(..16)
                .and_then(|b| b.try_into().ok())
                .unwrap_or([0; 16]),
            reason: "malformed",
        },
    };
    response.encode().expect("a legal response")
}

fn profile(trusted: &PeerId, route: &str) -> ProfileConfig {
    let doc = format!(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: [\"{trusted}\"]
endpoints:
  default_direct_endpoint: human
  entries:
    - id: human
      enabled: true
      advertise: false
channels:
  desired: []
discovery:
  providers:
    - type: static-bootstrap
      enabled: true
      priority: 5
      config:
        peers: [\"{route}\"]
"
    );
    serde_norway::from_str(&doc).expect("the document parses")
}

/// HEAD, trusting the raw peer and holding a static route to it.
struct Head {
    runtime: ComposedRuntime,
    peer: TransportIdentity,
    address: Multiaddr,
}

impl Head {
    async fn start(ip: Ipv4Addr, raw: &RawPeer) -> Self {
        let identity = ProfileIdentity::generate();
        let peer = identity.transport_identity().expect("peer id");
        let runtime = ComposedRuntime::start(
            &identity,
            &profile(&raw.id, &raw.route()),
            CompositionOptions {
                listen: vec![format!("/ip4/{ip}/tcp/0")],
                ..CompositionOptions::default()
            },
        )
        .await
        .expect("HEAD composes");
        let address = runtime.listening()[0].parse().expect("an address");
        Self {
            runtime,
            peer,
            address,
        }
    }

    fn peer_id(&self) -> PeerId {
        self.peer.as_str().parse().expect("a PeerId")
    }

    async fn stop(self) {
        self.runtime.shutdown().await.expect("HEAD stops");
    }
}

fn session_request() -> SessionRequest {
    SessionRequest::new(
        "human-client",
        Some(EndpointId::parse("human").expect("valid")),
        [DataCapability::Commands, DataCapability::Events],
    )
    .expect("in bounds")
}

fn frame_to_human(id: u8) -> Vec<u8> {
    IndependentFrame {
        message_id: [id; 16],
        sent_at_ms: 1_786_600_000_000,
        source_endpoint: "newer".to_owned(),
        destination_endpoint: Some("human".to_owned()),
        media_type: Some("text/plain".to_owned()),
        payload: b"from the raw peer".to_vec(),
    }
    .encode()
    .expect("a legal frame")
}

/// HEAD's send to the raw peer, retried while HEAD has not yet connected:
/// the static route is dialled on HEAD's schedule. Returns the first
/// answer that is not "no connection yet".
async fn head_sends(head: &Head, to: &RawPeer, id: u8) -> Result<EndpointId, TransportError> {
    let session = head
        .runtime
        .sessions()
        .open(session_request())
        .await
        .expect("leases");
    let peer = TransportIdentity::parse(to.id.to_string()).expect("a peer id");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let sent = session
            .send_direct(
                DirectDestination {
                    peer: peer.clone(),
                    endpoint: Some(EndpointId::parse("human").expect("valid")),
                },
                MessageId::from_bytes([id; 16]),
                Payload::at_ceiling(None, b"from HEAD".to_vec()).expect("within the ceiling"),
            )
            .await;
        match sent {
            Err(TransportError::PeerUnknown | TransportError::PeerUnreachable)
                if tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            other => {
                session.close().await.expect("closes");
                return other;
            }
        }
    }
}

/// Row: a peer listing only 3.0.0 is refused in both directions, and
/// nothing is read on either side. Control: a peer listing 2.0.0
/// exchanges both ways.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_unsupported_major_is_refused_both_ways_beside_a_2_0_control() {
    let ip = interweave_test_support::net::require_private_interface_v4();

    let mut control = RawPeer::start(ip, &[V2_0]).await;
    let head = Head::start(ip, &control).await;
    // A leased `human` at HEAD, or the control is answered `no_route`.
    let receiver = head
        .runtime
        .sessions()
        .open(session_request())
        .await
        .expect("leases");
    let answered = control
        .request(head.address.clone(), head.peer_id(), frame_to_human(1))
        .await
        .expect("the control exchanges");
    assert_eq!(answered.protocol, V2_0);
    let decoded = DirectResponseV2::decode(&answered.bytes);
    assert!(
        matches!(decoded, Ok(DirectResponseV2::Accepted { .. })),
        "{decoded:?}"
    );
    receiver.close().await.expect("closes");
    assert_eq!(
        head_sends(&head, &control, 2)
            .await
            .expect("HEAD's send is accepted")
            .as_str(),
        "human"
    );
    assert_eq!(control.inbound.recv().await.expect("read").protocol, V2_0);
    head.stop().await;

    let mut newer = RawPeer::start(ip, &[V3_0]).await;
    let head = Head::start(ip, &newer).await;
    let refused = newer
        .request(head.address.clone(), head.peer_id(), frame_to_human(3))
        .await;
    assert_eq!(refused.unwrap_err(), "UnsupportedProtocols");
    assert_eq!(
        head_sends(&head, &newer, 4).await,
        Err(TransportError::ProtocolUnsupported)
    );
    assert!(
        newer.inbound.try_recv().is_err(),
        "the 3.0.0 peer read nothing"
    );
    head.stop().await;
}

/// Row: a peer listing 2.1.0 beside 2.0.0 exchanges on 2.0.0 both ways;
/// HEAD never reads, and the peer never reads, a byte on 2.1.0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_minor_bump_negotiates_2_0_both_ways() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let mut newer = RawPeer::start(ip, &[V2_1, V2_0]).await;
    let head = Head::start(ip, &newer).await;
    let receiver = head
        .runtime
        .sessions()
        .open(session_request())
        .await
        .expect("leases");

    // The newer peer sends first, listing 2.1.0 first: multistream-select
    // offers it, HEAD declines it, 2.0.0 is agreed.
    let answered = newer
        .request(head.address.clone(), head.peer_id(), frame_to_human(5))
        .await
        .expect("exchanges");
    assert_eq!(answered.protocol, V2_0, "the stream negotiated 2.0.0");
    assert_eq!(
        DirectResponseV2::decode(&answered.bytes).expect("HEAD's response, independently decoded"),
        DirectResponseV2::Accepted {
            message_id: [5; 16],
            resolved_destination_endpoint: "human".to_owned(),
        }
    );
    let got = receive(&receiver, PATIENCE).await;
    let [SessionEvent::Direct(message)] = got.as_slice() else {
        panic!("one direct message at HEAD: {got:?}");
    };
    assert_eq!(message.message_id, MessageId::from_bytes([5; 16]));
    receiver.close().await.expect("closes");

    // HEAD sends: it lists 2.0.0 only, the newer peer supports it.
    assert_eq!(
        head_sends(&head, &newer, 6)
            .await
            .expect("accepted")
            .as_str(),
        "human"
    );
    let read = newer
        .inbound
        .recv()
        .await
        .expect("the peer read HEAD's request");
    assert_eq!(read.protocol, V2_0);
    let frame = IndependentFrame::decode(&read.bytes).expect("HEAD's frame, independently decoded");
    assert_eq!(frame.message_id, [6; 16]);
    assert_eq!(frame.payload, b"from HEAD");

    head.stop().await;
}
