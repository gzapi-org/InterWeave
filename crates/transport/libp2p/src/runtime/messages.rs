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

use interweave_kademlia_control_api::{KademliaCommand, KademliaEvent};
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
    /// Reach a peer: reuse a direct data-plane connection, else dial
    /// the book's direct candidates and defer its circuit routes
    /// behind the head-start (§12, step 9).
    DialPeer {
        /// The peer to reach.
        peer: TransportIdentity,
        /// Answered `Ok` when a connection is reused or a dial is
        /// admitted, else with why none was; a deferred circuit is
        /// reported through events, not here.
        reply: oneshot::Sender<Result<(), DialRefusal>>,
    },
    /// Replace the trust sources, evicting what they no longer permit.
    SetTrust {
        /// Who this profile trusts, and for what.
        trust: Box<TrustSources>,
        /// Answered with the number of connections closed by the change.
        reply: oneshot::Sender<usize>,
    },
    /// Send one directed message to a peer, AS the lease's own endpoint.
    ///
    /// The frame's `source_endpoint` is OVERWRITTEN from the lease before
    /// anything else happens: ADR-0030 makes the source a routing selector
    /// derived locally, and a command that consulted the frame's field —
    /// even to compare — would make it something a caller chooses. The
    /// lease is the unforgeable capability, not a name: its epoch is
    /// verified against the live lease, so a caller cannot send as an
    /// endpoint it did not claim (`ENDPOINTS.md`: "callers cannot spoof
    /// another local endpoint"). A stale or unheld lease is refused
    /// `EndpointNotRegistered`.
    SendDirect {
        /// The lease authorising the send. Its endpoint is the source, its
        /// epoch the proof.
        lease: interweave_local_client_api::EndpointLease,
        /// The peer to send to.
        peer: TransportIdentity,
        /// The frame. Its `source_endpoint` is replaced, not read.
        frame: Box<DirectMessageV2>,
        /// Answered when the exchange settles.
        reply: oneshot::Sender<Result<EndpointId, DirectError>>,
    },
    /// Ask a trusted, connected peer which endpoints it advertises.
    ///
    /// Answered from the cache when a fresh entry exists, otherwise by one
    /// exchange over `/interweave/endpoints/1.0.0`. The result is
    /// advisory: it gates no send and grants no trust (ADR-0031).
    QueryEndpoints {
        /// Whose directory.
        peer: TransportIdentity,
        /// Answered when the exchange settles or the cache answers.
        reply: oneshot::Sender<Result<super::endpoints::DirectoryResult, DirectError>>,
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
    /// Grant a session an exclusive lease on one configured endpoint.
    ///
    /// The epoch is minted by the runtime, fresh per grant. This is the
    /// claim an IPC session will make at Stage 13; until then the handle
    /// holder is the session.
    ClaimEndpoint {
        /// The session claiming. One lease per session, ever.
        session: String,
        /// The endpoint it wants.
        endpoint: EndpointId,
        /// What kind of client it says it is — hygiene, never authority.
        client_kind: String,
        /// Answered with the lease, or the contract's refusal.
        reply: oneshot::Sender<Result<interweave_local_client_api::EndpointLease, DirectError>>,
    },
    /// End every lease a session holds, closing each queue with it.
    ReleaseSession {
        /// The session going away.
        session: String,
        /// Answered with the endpoints released.
        reply: oneshot::Sender<Vec<EndpointId>>,
    },
    /// End one endpoint's lease, closing its queue with it.
    ///
    /// `testing.md` scenario 15: an endpoint lease disconnect removes the
    /// route immediately. `ReleaseSession` is what a session's own end
    /// does; this is the operator's revoke of one endpoint regardless of
    /// who holds it.
    RevokeEndpoint {
        /// Whose lease ends.
        endpoint: EndpointId,
        /// Answered with the number of undelivered events discarded.
        reply: oneshot::Sender<usize>,
    },
    /// Take everything waiting on one endpoint's queue.
    ///
    /// What an IPC session's event stream will do at Stage 13, pulled
    /// rather than pushed.
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
    /// Forward one provider command to the Kademlia driver.
    ///
    /// FIRE-AND-FORGET, unlike every other command: the port is a pump
    /// the composition root drives, and the driver's answers travel
    /// back as [`SwarmEvent::Kademlia`] events rather than replies —
    /// a reply channel here would be a second event stream.
    Kademlia {
        /// The provider's command, from `kademlia-control-api`.
        command: KademliaCommand,
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

/// What happened to a relay reservation (`RELAY.md` §11's outcomes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayReservationOutcome {
    /// An address is newly advertised: the first of a reservation, or
    /// another the relay reported for it.
    Accepted,
    /// The relay reported an address already advertised -- a renewal.
    Renewed,
    /// An active reservation's listener closed; its addresses are
    /// withdrawn.
    Lost,
    /// An ask closed before any address was reported: the dial was
    /// refused or failed, or the relay refused.
    Failed,
    /// This profile gave the reservation up: the target fell, or the
    /// relay lost its authorization.
    Released,
}

impl RelayReservationOutcome {
    /// §11's `outcome` label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Renewed => "renewed",
            Self::Lost => "lost",
            Self::Failed => "failed",
            Self::Released => "released",
        }
    }
}

/// How a peer is reached (`contracts/schemas/connectivity/peer-path`):
/// over a connection this profile made or accepted itself, or over a
/// circuit through a relay. Direct is preferred whenever it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PeerPath {
    /// Through a relay's circuit.
    Relayed,
    /// A connection to the peer's own address.
    Direct,
}

impl PeerPath {
    /// `contracts/CONNECTIVITY.md` §5's word for it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Relayed => "relayed",
            Self::Direct => "direct",
        }
    }
}

/// Why a peer's best path changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathChange {
    /// A direct connection was established beside a relayed one, by a
    /// dial or an inbound that was not a hole punch.
    DirectEstablished,
    /// A direct connection established by a DCUtR punch became the
    /// peer's path: `DCUTR.md` §7's `reason=dcutr` (step 8). Announced
    /// once the connection has held for `direct_stability_period`
    /// (step 9, `DCUTR.md` §4), the relay staying the announced path
    /// until then -- or at the relayed connection's close, if that
    /// comes first (the far end retired it at its own instant, or the
    /// relay dropped it): the announced path is always a connection
    /// that exists, so the young punched one becomes it then. Not
    /// announced at all if the punched connection closes within the
    /// interval while the relayed one stands. Pinned by
    /// `a_punched_direct_ranks_below_the_relay_until_it_is_stable`.
    HolePunched,
    /// The last direct connection closed and a relayed one remains.
    DirectLost,
}

/// What became of a hole-punch attempt, or why none began
/// (`DCUTR.md` §§7-8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolePunchOutcome {
    /// A relayed connection was not given a DCUtR handler; the label is
    /// §8's `outcome`.
    Declined {
        /// `declined_direct_exists`, `declined_cooldown`,
        /// `declined_peer_busy` or `declined_busy`.
        reason: &'static str,
    },
    /// An attempt began on a relayed connection.
    Started,
    /// The crate established a direct connection. It is the peer's
    /// path once it has held for the stability interval.
    Succeeded,
    /// The punched direct connection closed before the stability
    /// interval elapsed (`DCUTR.md` §4); the peer is in cooldown and
    /// the relayed path was never left.
    Unstable,
    /// The crate gave up; the peer is in cooldown.
    Failed {
        /// The crate's reason.
        detail: String,
    },
    /// Nothing was reported within the attempt horizon; the peer is in
    /// cooldown.
    TimedOut,
    /// The relayed connection closed while the attempt was in flight,
    /// or a network change gave the attempt up and whatever ended it
    /// afterwards -- the crate's outcome, the relayed close, the
    /// horizon -- was not a landed punch (which is `Succeeded`); no
    /// cooldown.
    Abandoned,
    /// A punch dial carried a candidate outside `DCUTR.md` §6's
    /// address-class boundary and was refused before any socket; the
    /// peer is in cooldown. The class is named, never the address.
    RefusedByClass {
        /// `not_literal`, `relayed`, `special_use` or
        /// `private_without_private_listener`.
        class: &'static str,
    },
}

/// What happened at this profile's relay server (`RELAY.md` §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayServerOutcome {
    /// A new reservation was accepted.
    ReservationAccepted,
    /// An existing reservation was renewed.
    ReservationRenewed,
    /// A reservation was refused, with the crate's status -- a
    /// ceiling or a rate limiter.
    ReservationDenied {
        /// The status sent, as the crate names it.
        status: String,
    },
    /// The exchange answering a reservation request failed.
    ReservationExchangeFailed {
        /// The crate's error text.
        detail: String,
    },
    /// The reserving peer's connection closed.
    ReservationClosed,
    /// The reservation ran out and was not renewed.
    ReservationTimedOut,
    /// A circuit was opened to `destination`.
    CircuitAccepted,
    /// A circuit was refused, with the crate's status.
    CircuitDenied {
        /// The status sent, as the crate names it.
        status: String,
    },
    /// Opening or answering a circuit failed in the exchange.
    CircuitExchangeFailed {
        /// The crate's error text.
        detail: String,
    },
    /// A circuit ended, with the error if it did not end cleanly.
    CircuitClosed {
        /// The crate's error text, when there was one.
        detail: Option<String>,
    },
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
    /// The network this profile is on changed (`transport/libp2p/
    /// CONNECTIVITY.md` §14, step 10): the set of addresses its
    /// listeners have bound -- loopback, unspecified and link-local
    /// ones aside -- differs from the last observation, after the
    /// first bind. When an address was REMOVED, what followed inside
    /// the runtime, in the same turn -- an addition alone is reported
    /// and offered, and invalidates nothing (§14 item 1):
    /// the AutoNAT verdict went to `unknown` and was published as a
    /// `ConnectivityChanged`, which the relay target follows; every
    /// reachability candidate is due for a re-test within the jitter;
    /// every DCUtR attempt in flight was given up (it keeps its permit
    /// while the crate's rounds run and ends `Abandoned`, no cooldown,
    /// when they do -- or `Succeeded`, if the punch lands after all)
    /// and every cooldown was lifted. Nothing was closed
    /// by the runtime:
    /// a connection that died with its interface is reported as it
    /// closes, and one that survived is kept (§14 item 5). Nothing is
    /// replayed (item 7): an exchange the transition failed was
    /// answered to its caller. Informational; dropped when the outbox
    /// has no base room.
    NetworkChanged {
        /// Addresses bound at the last observation and not now.
        removed: Vec<String>,
        /// Addresses bound now and not at the last observation.
        added: Vec<String>,
    },
    /// A LOGICAL peer became connected: its first retained connection
    /// was established and Noise authenticated it. Emitted once per
    /// peer, not once per connection: a second connection to a peer
    /// already connected -- a hole punch beside a circuit, a dial-back
    /// beside an outbound -- is a `PeerPathChanged` when it changes
    /// the best path, and nothing otherwise. That is the once-per-peer
    /// half of `contracts/CONNECTIVITY.md` §5's `PeerConnected`; the
    /// "application peer" half is not this event's, which fires for a
    /// retained infrastructure-only connection too -- a relay reserved
    /// on, an AutoNAT server dialled (`tests/connectivity/tests/
    /// autonat_client.rs` waits for one) -- and the class the peer
    /// holds is the consumer's to read.
    Connected {
        /// The authenticated remote identity.
        peer: TransportIdentity,
        /// The path the peer is reached over at this moment.
        path: PeerPath,
    },
    /// A connected peer's best path changed while it stayed connected:
    /// a direct connection came up beside a relayed one, or the last
    /// direct one closed with a relayed one remaining (`contracts/
    /// CONNECTIVITY.md` §5). Emitted the moment the set changes for a
    /// dialled or inbound direct connection; for a punched one, once it
    /// has held for the stability interval (step 9).
    PeerPathChanged {
        /// The peer.
        peer: TransportIdentity,
        /// The path before.
        previous: PeerPath,
        /// The path now.
        current: PeerPath,
        /// Why.
        reason: PathChange,
    },
    /// A logical peer's last usable connection closed
    /// (`PeerDisconnected`). Once per peer, never for a connection that
    /// was refused at establishment.
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
    /// The Kademlia driver reported on the provider port.
    ///
    /// Carried out of the Swarm task as an ordinary event so the
    /// composition root can pump it into the provider with everything
    /// else; the payload is the neutral port type, so nothing libp2p
    /// crosses here either.
    Kademlia {
        /// The driver's event, from `kademlia-control-api`.
        event: KademliaEvent,
    },
    /// mDNS heard a LAN announcement, and it survived the boundary.
    ///
    /// Carried out as an ordinary event for the reason the Kademlia one
    /// is: the composition root pumps it into the provider, and the
    /// payload is the neutral `discovery-api` type so nothing libp2p
    /// crosses here.
    ///
    /// EVERY CANDIDATE HERE IS ALREADY INSIDE ADR-0052'S BOUNDARY. The
    /// driver filtered at the learn site, so a consumer does not repeat
    /// the check and -- more to the point -- must not read this event as
    /// permission to dial: a candidate is advisory reachability, and
    /// ConnectionManager admission is still what decides (ADR-0011).
    MdnsDiscovered {
        /// The candidates, grouped one per peer.
        candidates: Vec<interweave_discovery_api::CandidatePeer>,
    },
    /// mDNS retracted a pair whose record lapsed.
    ///
    /// NOT filtered on address class, unlike the discovery above: a
    /// retraction for an address the floor would refuse must still
    /// reach the provider, or whatever it holds is stranded.
    MdnsExpired {
        /// The `(peer, address)` pairs that lapsed.
        expired: Vec<(TransportIdentity, String)>,
    },
    /// An interface the mDNS provider was using cannot discover: its
    /// bind or multicast join failed when it came up, a receive error
    /// ended it, or a send failed (ADR-0053 rule 5).
    ///
    /// THE DEGRADED SIGNAL `providers/mdns.md` §Failure needs for the
    /// causes `MdnsUnavailable` does not cover. The node keeps running,
    /// and whether the provider as a whole is degraded is the
    /// consumer's call, since other interfaces may still work.
    /// Whether to re-create the interface is not decided here.
    ///
    /// Held rather than dropped under backpressure, one per interface,
    /// the latest reason winning, so the set is bounded by this node's
    /// own interfaces, which a remote host cannot add.
    MdnsInterfaceFailed {
        /// This node's own interface address -- never a peer's.
        address: std::net::IpAddr,
        /// The operating system's error.
        detail: String,
    },
    /// The mDNS crate's interface watcher reported an error after start
    /// (ADR-0053 rule 5), so interfaces coming and going may no longer be
    /// seen. The crate reports it once until the watcher works again, and
    /// stops polling a watcher that fails twice in a row; held under
    /// backpressure as the latest one, so it is bounded to one. The
    /// runtime answers it by rebuilding the behaviour on its next mDNS
    /// refresh tick, with a fresh watcher; a rebuild that cannot build one
    /// is reported as [`SwarmEvent::MdnsRebuildFailed`] and tried again on
    /// the tick after.
    MdnsWatcherFailed {
        /// The watcher's error.
        detail: String,
    },
    /// A rebuild after [`SwarmEvent::MdnsWatcherFailed`] could not build a
    /// fresh interface watcher (ADR-0053 rule 5).
    ///
    /// NOT [`SwarmEvent::MdnsUnavailable`]: mDNS is still running, serving
    /// the interfaces it had, and only new ones go unseen; the rebuild is
    /// tried again on each refresh tick until one succeeds. Held, the
    /// latest winning, when the outbox has no room (`mdns_tick`,
    /// unit-tested).
    MdnsRebuildFailed {
        /// The operating system's error for the watcher.
        detail: String,
    },
    /// The host has no resolver configuration this process can read, so
    /// the node came up resolving no name at all.
    ///
    /// DEGRADED, NOT FATAL. The DNS transport is built for every profile,
    /// and refusing to start without a resolver made a profile that
    /// names no DNS host -- an air-gapped LAN node, say -- fail where it
    /// had started before the transport existed (#111 DNS review P2-3).
    /// Such a node loses nothing; one that names a `/dns4` host sees each
    /// dial to it fail as an ordinary lookup failure, and this event is
    /// what says why, once, before any other.
    ///
    /// Produced only by `resolver_or_empty`, whose mapping is unit-tested,
    /// and that it arrives on a started runtime, first, is
    /// `a_runtime_whose_resolver_read_fails_starts_and_says_so`, through
    /// `start`'s resolver seam. What no test arranges is a host that
    /// really lacks a configuration: `start` passes the system read
    /// through that seam and nothing else.
    ResolverUnavailable {
        /// The resolver's own message.
        detail: String,
    },
    /// A profile asked for LAN discovery and did not get it.
    ///
    /// `providers/mdns.md` §Failure says an mDNS environment failure
    /// makes the provider "degraded/unavailable" and does "not kill
    /// transport or static/cache discovery", so the runtime comes up
    /// without it rather than refusing to start.
    ///
    /// THIS EVENT KEEPS ONE CAUSE FROM BEING SILENT: the interface
    /// watcher could not be created. The per-interface causes in
    /// `providers/mdns.md` §Failure's list -- a multicast bind or join
    /// that fails, a send or receive error -- arrive as
    /// [`SwarmEvent::MdnsInterfaceFailed`] since ADR-0053 rule 5; as
    /// released, the crate logged them and produced no event. What stays
    /// silent: a network that drops the packets without an error -- a
    /// profile on one looks configured and hears nothing, and no event
    /// says so. An error the interface watcher reports after start was
    /// logged only until #112, and arrives now as
    /// [`SwarmEvent::MdnsWatcherFailed`], once until the watcher recovers
    /// (a watcher that fails twice in a row is not polled again).
    /// An earlier version said this event kept every cause from being
    /// silent (#111 mDNS review F4).
    ///
    /// A rebuild that cannot build a watcher later is a different event,
    /// [`SwarmEvent::MdnsRebuildFailed`]: mDNS is running then.
    ///
    /// What holds, and how: it is produced only for a profile that
    /// asked for mDNS and whose construction failed (`mdns_or_degraded`,
    /// unit-tested), and it is pushed before the Swarm task's loop begins,
    /// so it precedes every event the loop produces. That push is one
    /// line no test reaches: the only failure that produces this event is
    /// the kernel's interface watcher, which a test cannot break without
    /// a test-only knob in production configuration. An earlier version
    /// of this doc said "emitted once, before any other event, and only
    /// when mDNS was asked for" as though all of it were enforced (#111
    /// re-review P2-6).
    ///
    /// A settings rule the driver refuses is NOT this: that is the
    /// operator asking for something impossible, and it fails
    /// [`SubstrateConfig::validate`] before the runtime starts.
    ///
    /// [`SubstrateConfig::validate`]: crate::SubstrateConfig::validate
    MdnsUnavailable {
        /// The operating system's own message, which is the only thing
        /// that distinguishes "no interface watcher" from the next
        /// cause this arm acquires.
        detail: String,
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
    /// A publish was accepted locally with NO mesh peers to carry it.
    ///
    /// PUBSUB.md: local acceptance is the only synchronous success claim,
    /// and diagnostics "must expose `mesh_peer_count=0` as degraded
    /// channel reachability rather than claiming delivery". Reading the
    /// backend's `NoPeersSubscribedToTopic` straight to success is
    /// correct for the CALLER -- the publish did happen, and broadcast
    /// promises nothing about reach -- but it erased the one signal that
    /// separates healthy propagation from a channel nobody is listening
    /// on. Both answers are now given: `Ok` to the caller, this to the
    /// operator.
    BroadcastUnreachable {
        /// The channel that has no mesh peers.
        channel: interweave_transport_api::ChannelId,
    },
    /// A broadcast was refused by one or more sessions' queues.
    ///
    /// The overload drop broadcast is allowed to take — a session whose
    /// consumer is behind loses the message rather than stalling the
    /// mesh for everyone. Allowed is not the same as invisible: without
    /// this the consumer's gap is indistinguishable from a message that
    /// was never sent, which is the difference between a slow client and
    /// a broken network.
    ///
    /// ONE event per message, carrying a count, not one per dropped
    /// session. A message that every session refuses would otherwise
    /// notify once per session — the same amplification that let a
    /// fan-out run past the outbox bound.
    BroadcastDropped {
        /// The channel it arrived on.
        channel: interweave_transport_api::ChannelId,
        /// The publisher, as authenticated by the mesh.
        source_peer: TransportIdentity,
        /// How many sessions refused it.
        sessions: usize,
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
    /// The direct-inbound reachability verdict changed (`AUTONAT.md`
    /// §5; `contracts/CONNECTIVITY.md` §3's `connectivity-summary`).
    ///
    /// Emitted by the AutoNAT client adapter on every change of the
    /// normalized state or of the verified set, and only then -- a
    /// moved expiry horizon alone is not a change (the manager's
    /// `ConnectivityChanged` doc says why). A profile with no client
    /// configured has no source for it and is `unknown`.
    ConnectivityChanged {
        /// The neutral three-word state.
        direct_inbound: interweave_transport_api::DirectInboundState,
        /// Every address currently meeting the threshold; empty unless
        /// verified. These are the addresses the Swarm advertises.
        verified_addresses: Vec<String>,
    },
    /// The AutoNAT client reported an outcome the manager refused.
    ///
    /// `AUTONAT.md` §9's `refused_*` outcomes, as an event rather than
    /// only a counter: a refusal nobody can see is the SPIKE-004 shape
    /// CLAUDE.md §1 records as binding. Informational; dropped when the
    /// outbox has no base room, like every other diagnostic.
    ReachabilityReportRefused {
        /// The server the crate says reported.
        server: TransportIdentity,
        /// The address it reported on.
        address: String,
        /// Which of the manager's two refusals it was.
        reason: interweave_transport_runtime::reachability::RefusedReport,
    },
    /// This profile, as an AutoNAT v2 SERVER, finished a probe for a
    /// client whose dial-back was MADE: it established, or it failed
    /// after the pool took it. A dial-back refused before it was made
    /// -- by this wrapper's target rule, a budget, or the outbound gate
    /// -- is `AutonatProbeRefused` instead, never this. `AUTONAT.md` §9's `autonat_server_probes_total{outcome=
    /// served_ok|served_failed}`. Informational; dropped when the outbox
    /// has no base room.
    AutonatProbeServed {
        /// The client that asked.
        client: TransportIdentity,
        /// The address the crate dialled back to.
        address: String,
        /// Whether the dial-back CONNECTION was established, from the
        /// wrapper's own decision -- true for a connection that was
        /// made whatever the exchange then did.
        reached: bool,
        /// Whether the exchange succeeded and the client was told `OK`:
        /// the dial status the vendored crate carries on its event
        /// (ADR-0051's second patch). `served_ok` counts exactly this;
        /// a delivered negative response is `served_failed`.
        succeeded: bool,
        /// Bytes the client sent as dial data before the dial-back.
        data_amount: usize,
    },
    /// This profile, as an AutoNAT v2 SERVER, refused a probe under
    /// `AUTONAT.md` §7 -- a budget, or the dial-back target rule -- so
    /// no dial was made. `autonat_server_probes_total{outcome=refused_*}`,
    /// as an event for the same reason the client's refusals are:
    /// a refusal nobody can see is the SPIKE-004 shape. Informational.
    AutonatProbeRefused {
        /// The client that asked.
        client: TransportIdentity,
        /// The address, when the refusal came after the crate named it;
        /// `None` for a budget refusal, which precedes that.
        address: Option<String>,
        /// The §9 label: `refused_concurrent_probes`, `refused_client_rate`,
        /// `refused_global_rate`, `refused_no_address`,
        /// `refused_not_literal_ip`, `refused_source_mismatch`,
        /// `refused_source_unknown`, `refused_not_global`,
        /// `refused_unexpected_dial`, `refused_by_gate`.
        reason: &'static str,
    },
    /// The AutoNAT client reported an outcome the adapter could not
    /// classify, so no vote was recorded.
    ///
    /// `AUTONAT.md` §9's `unclassified` outcome, as an event for the
    /// same reason as the refusal above. Unreachable with the pinned
    /// crate, whose public error has exactly the two texts the adapter
    /// knows (`autonat_driver::DIAL_BACK_FAILURE_TEXTS` and its pin);
    /// a re-vendor that adds a third produces this rather than silence.
    ReachabilityOutcomeUnclassified {
        /// The server the crate says reported.
        server: TransportIdentity,
        /// The address it reported on.
        address: String,
        /// The error's own text, bounded by the crate's fixed set.
        detail: String,
    },
    /// The AutoNAT client's candidate scope refused a distinct address
    /// for room (`AUTONAT.md` §6's bound), said at most once per
    /// silence bound rather than once per refusal. Informational.
    ReachabilityCandidatesTruncated {
        /// Refusals at the SEND since the runtime started: distinct
        /// addresses the candidate scope would not forward to the client
        /// for room. Cumulative.
        at_the_send: usize,
        /// Refusals at the COUNT: the largest number of candidates the
        /// scope forwarded that the manager had no room to count on any
        /// tick since the last report. A peak, not a total.
        at_the_count: usize,
    },
    /// A relay reservation moved (`RELAY.md` §11's
    /// `relay_reservation_events_total{outcome}`): an address newly
    /// advertised, a re-report, a loss, a failed ask, or a release --
    /// with the addresses concerned, which for a loss, a release or a
    /// forget are ALREADY gone from what the Swarm advertises. A
    /// profile with no relay client configured has no source for it.
    /// Informational; dropped when the outbox has no base room.
    RelayReservationChanged {
        /// The relay.
        relay: TransportIdentity,
        /// What happened.
        outcome: RelayReservationOutcome,
        /// The relay-derived addresses concerned: the one newly
        /// advertised or re-reported, or every one withdrawn.
        addresses: Vec<String>,
        /// Why: the listener's close, with its error text when it had
        /// one and the address the ask went through; or why an ask
        /// failed before a listener existed; or why a reservation was
        /// released. `None` for an acceptance and a re-report.
        detail: Option<String>,
    },
    /// The relay client reported something the reservation manager
    /// refused, by name -- `RELAY.md` §11's `refused_*` outcomes, as an
    /// event for the same reason the AutoNAT client's refusals are: a
    /// refusal nobody can see is the SPIKE-004 shape CLAUDE.md §1
    /// records as binding. Informational.
    RelayReportRefused {
        /// The relay the crate's event named.
        relay: TransportIdentity,
        /// The address reported, when the report carried one.
        address: Option<String>,
        /// Which of the manager's refusals it was.
        reason: interweave_transport_runtime::relay::RefusedRelayReport,
    },
    /// Where the reservation target stands, whenever that changed:
    /// `RELAY.md` §11's `relay_reservations_active` and
    /// `relay_reservation_target`, and `CONNECTIVITY.md` §8's `Partial`
    /// -- reported here rather than retried into. Informational.
    RelayStandingChanged {
        /// Satisfied, partial, or a zero target.
        standing: interweave_transport_runtime::relay::Standing,
        /// Reservations held.
        active: usize,
        /// Reservations wanted.
        target: usize,
        /// Asks out and not yet answered.
        requested: usize,
        /// Relays the next tick could ask: idle, or backed off and
        /// due. Zero under `Partial` is the deployment's shortfall,
        /// not a storm (`CONNECTIVITY.md` §8).
        askable: usize,
        /// Relays known, static and learned.
        candidates: usize,
    },
    /// A relayed connection to a peer whose announced path is a stable
    /// direct one -- punched and past its interval, or dialled -- was
    /// closed by this runtime (`transport/libp2p/CONNECTIVITY.md` §13's
    /// "retire redundant relayed peer connection when safe" and §12's
    /// lost race, step 9): safe meaning no direct or directory exchange
    /// this profile started with the peer is awaiting its answer. The
    /// far end's exchanges on it, if any, fail there; it retires at its
    /// own instant too, and reports nothing when this end closed first.
    /// Reported once per connection. The reservation and the route
    /// stay (§13: warm for inbound failover). Informational; dropped
    /// when the outbox has no base room.
    RelayedConnectionRetired {
        /// The peer.
        peer: TransportIdentity,
    },
    /// A DCUtR attempt began, ended, or was not begun (`DCUTR.md`
    /// §§7-8). Reported once per relayed connection per attempt; the
    /// direct connection a success produces is announced as a
    /// `PeerPathChanged` with `PathChange::HolePunched` once it has held
    /// for the stability interval, or sooner at the relayed connection's
    /// close (see `PathChange::HolePunched`). Informational; dropped
    /// when the outbox has no base room.
    HolePunch {
        /// The peer at the far end of the circuit.
        peer: TransportIdentity,
        /// What happened.
        outcome: HolePunchOutcome,
    },
    /// This profile, as a Circuit Relay v2 SERVER, decided a request
    /// or saw a reservation or circuit move (`RELAY.md` §11's
    /// `relay_server_*`): every event the crate emits, translated, so a
    /// denial is never a counter nobody reads. Informational; dropped
    /// when the outbox has no base room.
    RelayServed {
        /// The requester: the reserving peer, or a circuit's source.
        peer: TransportIdentity,
        /// A circuit's destination; `None` for a reservation event.
        destination: Option<TransportIdentity>,
        /// What happened.
        outcome: RelayServerOutcome,
    },
    /// An outbound dial failed after being admitted.
    DialFailed {
        /// The peer that was being dialed, when known.
        peer: Option<TransportIdentity>,
        /// What went wrong.
        detail: String,
    },
}
