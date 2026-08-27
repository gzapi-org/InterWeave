// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! What the substrate can be asked to do, and what it reports back.
//!
//! Split out of `runtime.rs` unchanged. These three types are the entire
//! vocabulary between a caller and the Swarm task: commands go in over a
//! bounded channel, events come out over another, and nothing else
//! crosses that boundary — which is what keeps the Swarm unreachable
//! from outside the task that owns it.

use libp2p::Multiaddr;
use tokio::sync::oneshot;

use interweave_transport_api::TransportError as DirectError;
use interweave_transport_api::{DirectMessageV2, EndpointId, TransportIdentity};
use interweave_transport_runtime::{DialDenial, TrustSources};

// `DirectEndpoints` still lives beside the loop that consumes it.
use super::DirectEndpoints;
use super::config::SubstrateError;

/// Default depth of the command channel.
/// What the substrate can be asked to do.
#[derive(Debug)]
pub enum SwarmCommand {
    /// Start listening on an address.
    Listen {
        /// The address to listen on.
        address: Multiaddr,
        /// Answered with the listener's assigned address, once the OS
        /// has assigned it.
        ///
        /// Held until `NewListenAddr` arrives rather than answered
        /// immediately: `listen_on` returns only a `ListenerId`, so an
        /// immediate answer could carry nothing a caller could advertise
        /// or dial.
        reply: oneshot::Sender<Result<Multiaddr, String>>,
    },
    /// Install broadcast configuration and hold the desired channels.
    ///
    /// Unlike `ConfigureDirect`, which discards every open queue because
    /// a lease changing hands must not inherit the previous holder's
    /// messages, this REPLACES the desired set and KEEPS live session
    /// joins. A reconfigure is an operator action on warm-mesh policy,
    /// not a client disconnect.
    ConfigureBroadcast {
        /// The validated configuration.
        config: Box<crate::runtime::broadcast::BroadcastChannels>,
        /// Answered once installed.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Take a local join reference on a channel.
    Join {
        /// The channel to join.
        channel: interweave_transport_api::ChannelId,
        /// The session taking the reference.
        session: String,
        /// Answered with the local outcome.
        reply: oneshot::Sender<Result<(), interweave_transport_api::TransportError>>,
    },
    /// Release one session's join reference.
    Leave {
        /// The channel to leave.
        channel: interweave_transport_api::ChannelId,
        /// The session releasing it.
        session: String,
        /// Answered once released.
        reply: oneshot::Sender<()>,
    },
    /// Publish one envelope to a channel.
    ///
    /// Carries a caller-built frame for the same reason `SendDirect`
    /// does: the runtime mints no identifiers and reads no clock on a
    /// caller's behalf.
    Publish {
        /// The channel to publish on.
        channel: interweave_transport_api::ChannelId,
        /// The session publishing, whose own join authorizes it.
        session: String,
        /// The envelope to send.
        frame: Box<interweave_transport_api::BroadcastMessageV1>,
        /// Answered with the local outcome.
        reply: oneshot::Sender<Result<(), interweave_transport_api::TransportError>>,
    },
    /// Take everything waiting on one session's broadcast queue.
    DrainSession {
        /// The session draining.
        session: String,
        /// Answered with the events, oldest first.
        reply: oneshot::Sender<Vec<interweave_transport_runtime::session_queue::BroadcastEvent>>,
    },
    /// Stop a listener, naming it by an address `listen` returned.
    ///
    /// Answers `true` when a listener was serving that address and has
    /// been removed, `false` when none was. Without this a bound listener
    /// could only be closed by stopping the whole runtime.
    StopListening {
        /// An address the listener bound.
        address: Multiaddr,
        /// Whether a listener was found and removed.
        reply: oneshot::Sender<bool>,
    },
    /// Dial a peer at an address.
    ///
    /// Carries the EXPECTED PeerId, and it is bound into the dial rather
    /// than used only for admission. Dialling a bare address tells libp2p
    /// nothing about who should be there, so a server at that address can
    /// complete a Noise handshake with any key and the connection is
    /// accepted — dialling an address is not the same as reaching the
    /// peer that was supposed to be there.
    Dial {
        /// The peer this address is believed to belong to.
        peer: TransportIdentity,
        /// Where to dial.
        address: Multiaddr,
        /// Answered when the dial is admitted or refused locally.
        reply: oneshot::Sender<Result<(), DialRefusal>>,
    },
    /// Remember an address as a candidate for a peer.
    AddAddress {
        /// The peer the address belongs to.
        peer: TransportIdentity,
        /// The candidate address.
        address: Multiaddr,
        /// Answered with whether it was remembered.
        reply: oneshot::Sender<bool>,
    },
    /// Dial a peer at the best address already known for it.
    DialPeer {
        /// The peer to reach.
        peer: TransportIdentity,
        /// Answered when a dial is admitted, or with why none was.
        reply: oneshot::Sender<Result<(), DialRefusal>>,
    },
    /// Replace the trust sources, evicting what they no longer permit.
    SetTrust {
        /// Who this profile trusts, and for what.
        trust: Box<TrustSources>,
        /// Answered with the number of connections closed by the change.
        reply: oneshot::Sender<usize>,
    },
    /// Send one directed message to a peer.
    ///
    /// The frame's `source_endpoint` is supplied by the CALLER's runtime
    /// from its own lease, never by an application: ADR-0030 makes the
    /// source a routing selector derived locally, so a command that let
    /// an application choose it would be the spoofing path the contract
    /// forbids.
    SendDirect {
        /// The peer to send to.
        peer: TransportIdentity,
        /// The frame, already validated by its own types.
        frame: Box<DirectMessageV2>,
        /// Answered when the exchange settles.
        reply: oneshot::Sender<Result<EndpointId, DirectError>>,
    },
    /// Install endpoint configuration for directed messaging.
    ///
    /// Replaces whatever was there, which DISCARDS every open queue —
    /// reconfiguring endpoints is the leases changing, and a new holder
    /// must not inherit the previous one's undelivered messages.
    ConfigureDirect {
        /// The configuration to install.
        config: Box<DirectEndpoints>,
        /// Answered once installed, or with why it was not.
        reply: oneshot::Sender<Result<(), SubstrateError>>,
    },
    /// End one endpoint's lease, closing its queue with it.
    ///
    /// `testing.md` scenario 15: an endpoint lease disconnect removes the
    /// route immediately. Stage 8 calls this when an IPC session goes;
    /// until then it is how a test reaches the same state.
    RevokeEndpoint {
        /// Whose lease ends.
        endpoint: EndpointId,
        /// Answered with the number of undelivered events discarded.
        reply: oneshot::Sender<usize>,
    },
    /// Take everything waiting on one endpoint's queue.
    ///
    /// The in-process stand-in for what Stage 8's IPC session does.
    DrainEndpoint {
        /// Whose queue.
        endpoint: EndpointId,
        /// Answered with the events, oldest first.
        reply: oneshot::Sender<Vec<interweave_transport_runtime::DirectEvent>>,
    },
    /// Refuse new connectivity while keeping what is already up.
    Drain {
        /// Answered once the manager is draining.
        reply: oneshot::Sender<()>,
    },
    /// Stop, closing listeners and connections.
    Shutdown {
        /// Answered once the Swarm has been dropped.
        reply: oneshot::Sender<()>,
    },
}

/// Why a dial did not proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialRefusal {
    /// Nothing is known about where to reach this peer.
    ///
    /// Distinct from a policy refusal on purpose: "I have no address"
    /// and "I have one and will not use it" are different problems and
    /// an operator fixes them differently.
    NoKnownAddress,
    /// The local admission policy refused it.
    ///
    /// Refused BEFORE a socket is opened. That ordering is the whole
    /// value of the gate: a quarantined address costs nothing.
    Policy(DialDenial),
    /// libp2p refused the dial itself.
    Backend(String),
}

/// What the substrate reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwarmEvent {
    /// A listener is up.
    Listening {
        /// The address it bound to.
        address: Multiaddr,
    },
    /// A listener that had bound is no longer listening.
    ///
    /// The counterpart of [`Self::Listening`], and it exists because the
    /// absence of one was silent: a listener that died AFTER binding
    /// answered no pending `listen` reply, so the arm that handles it had
    /// nothing to report to and returned nothing. A node could stop
    /// accepting connections on every address it had and no caller was
    /// ever told.
    ListeningStopped {
        /// The addresses it had bound, as libp2p reports them.
        addresses: Vec<Multiaddr>,
        /// Why it closed. `None` for an orderly close.
        reason: Option<String>,
    },
    /// A connection was established and Noise authenticated the peer.
    Connected {
        /// The authenticated remote identity.
        peer: TransportIdentity,
    },
    /// A connection closed.
    Disconnected {
        /// The remote identity.
        peer: TransportIdentity,
    },
    /// Identify completed for a peer.
    Identified {
        /// The remote identity.
        peer: TransportIdentity,
        /// The protocol string it advertised.
        protocol_version: String,
        /// The addresses it claims to listen on. ADVISORY: peer-asserted
        /// and never authorization.
        listen_addresses: Vec<Multiaddr>,
    },
    /// A directed message was admitted onto a local endpoint queue.
    ///
    /// Reported AFTER queue admission, so a consumer seeing this knows
    /// the event is retrievable — not merely that a frame arrived.
    DirectDelivered {
        /// The endpoint whose queue took it.
        endpoint: EndpointId,
        /// The authenticated sender.
        peer: TransportIdentity,
    },
    /// A broadcast was admitted onto a local session's queue.
    ///
    /// Reported AFTER queue admission, so a consumer seeing this knows
    /// the event is retrievable — not merely that a message arrived. One
    /// per receiving session, because a broadcast fans out and each
    /// session drains its own queue.
    ///
    /// Carries NO endpoint: ADR-0030 keeps EndpointId out of broadcast,
    /// so two local endpoints on one PeerId are indistinguishable as
    /// originators. The absence is structural rather than an omission.
    BroadcastDelivered {
        /// The channel it arrived on, derived from the topic.
        channel: interweave_transport_api::ChannelId,
        /// The authenticated original publisher, not the relay.
        source_peer: TransportIdentity,
        /// The local session whose queue took it.
        session: String,
    },
    /// A connected peer subscribed to a channel this node holds.
    ///
    /// The one honest signal of BACKEND subscription state: it is
    /// observed at the other end of the connection, not read out of this
    /// node's own bookkeeping. A test that wants to know whether a leave
    /// really unsubscribed the mesh asks the peer, which is the only party
    /// the answer matters to.
    ///
    /// Only for channels this node has derived a topic for; a peer's
    /// subscription to a topic this node never held is dropped rather than
    /// announced under a channel it could only guess.
    PeerSubscribed {
        /// The subscribing peer.
        peer: TransportIdentity,
        /// The channel, mapped back from the topic.
        channel: interweave_transport_api::ChannelId,
    },
    /// A connected peer unsubscribed from a channel this node holds.
    PeerUnsubscribed {
        /// The unsubscribing peer.
        peer: TransportIdentity,
        /// The channel, mapped back from the topic.
        channel: interweave_transport_api::ChannelId,
    },
    /// An outbound dial failed after being admitted.
    DialFailed {
        /// The peer that was being dialed, when known.
        peer: Option<TransportIdentity>,
        /// What went wrong.
        detail: String,
    },
}
