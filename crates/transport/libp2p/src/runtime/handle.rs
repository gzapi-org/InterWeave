// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The handle: what a caller can ask of a running substrate.
//!
//! Split out of `runtime.rs` unchanged. `SwarmRuntime::start` and the
//! event loop stay beside each other in `mod.rs`, because the loop IS
//! the runtime; everything here is the other side of the two bounded
//! channels — a request written in, an answer read back, and nothing
//! that can reach the Swarm directly.
//!
//! Rust allows a type's inherent methods to live in several modules of
//! its defining crate, which is what keeps `start` with the loop it
//! spawns while the ask-and-answer surface sits here.

use libp2p::Multiaddr;
use tokio::sync::{mpsc, oneshot};

use interweave_transport_api::TransportError as DirectError;
use interweave_transport_api::{DirectMessageV2, EndpointId, TransportIdentity};
use interweave_transport_runtime::TrustSources;

use super::SwarmRuntime;
use super::config::SubstrateError;
use super::direct::DirectEndpoints;
use super::messages::{DialRefusal, SwarmCommand, SwarmEvent};

impl SwarmRuntime {
    /// Forward one provider command to the Kademlia driver.
    ///
    /// Fire-and-forget by design: the port is a pump, and the driver's
    /// answers arrive as [`SwarmEvent::Kademlia`] events.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone.
    pub async fn kademlia(
        &self,
        command: interweave_kademlia_control_api::KademliaCommand,
    ) -> Result<(), SubstrateError> {
        self.commands
            .send(SwarmCommand::Kademlia { command })
            .await
            .map_err(|_| SubstrateError::Stopped)
    }

    /// Start listening, returning the address that was actually bound.
    ///
    /// With port 0 the assigned port is only knowable from this answer,
    /// so it waits for the listener to report it.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone, or
    /// [`SubstrateError::Transport`] if the listener could not bind.
    pub async fn listen(&self, address: Multiaddr) -> Result<Multiaddr, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::Listen { address, reply })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer
            .await
            .map_err(|_| SubstrateError::Stopped)?
            .map_err(SubstrateError::Transport)
    }

    /// Stop the listener serving `address`.
    ///
    /// Returns whether one was found and removed. Until this existed a
    /// bound listener could only be closed by stopping the whole runtime,
    /// so a node could not withdraw one address while keeping another.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone.
    pub async fn stop_listening(&self, address: Multiaddr) -> Result<bool, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::StopListening { address, reply })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Install broadcast configuration and hold the desired channels.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone, or
    /// [`SubstrateError::Transport`] if the configuration was refused.
    pub async fn configure_broadcast(
        &self,
        config: crate::runtime::broadcast::BroadcastChannels,
    ) -> Result<(), SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::ConfigureBroadcast {
                config: Box::new(config),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer
            .await
            .map_err(|_| SubstrateError::Stopped)?
            .map_err(SubstrateError::Transport)
    }

    /// Take a local join reference on `channel` for `session`.
    ///
    /// A join is what makes this session a consumer: without one it
    /// receives nothing and may not publish, whatever the profile
    /// desires.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone. A refusal by a
    /// subscription ceiling is `Ok(Err(..))`: the command was delivered
    /// and answered, and the answer was no.
    pub async fn join(
        &self,
        channel: interweave_transport_api::ChannelId,
        session: impl Into<String>,
    ) -> Result<Result<(), interweave_transport_api::TransportError>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::Join {
                channel,
                session: session.into(),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Release `session`'s join reference on `channel`.
    ///
    /// Idempotent: leaving a channel this session does not hold is not an
    /// error, and does not disturb any other session's join.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone.
    pub async fn leave(
        &self,
        channel: interweave_transport_api::ChannelId,
        session: impl Into<String>,
    ) -> Result<(), SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::Leave {
                channel,
                session: session.into(),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Publish one envelope to `channel` on `session`'s own join.
    ///
    /// Success means the local backend accepted the publish. It does NOT
    /// mean any remote peer received it — PUBSUB.md makes local
    /// acceptance the only synchronous claim, and publishing into a mesh
    /// with no peers succeeds.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone. `ChannelNotJoined`
    /// when this session holds no join, and the other local refusals, are
    /// `Ok(Err(..))`.
    pub async fn publish(
        &self,
        channel: interweave_transport_api::ChannelId,
        session: impl Into<String>,
        frame: interweave_transport_api::BroadcastMessageV1,
    ) -> Result<Result<(), interweave_transport_api::TransportError>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::Publish {
                channel,
                session: session.into(),
                frame: Box::new(frame),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Take everything waiting on one session's broadcast queue.
    ///
    /// What an IPC session's event stream will do at Stage 13, the same
    /// way `drain_endpoint` is for direct.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone.
    pub async fn drain_session(
        &self,
        session: impl Into<String>,
    ) -> Result<Vec<interweave_transport_runtime::session_queue::BroadcastEvent>, SubstrateError>
    {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::DrainSession {
                session: session.into(),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Dial `peer` at `address`, subject to the admission policy.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone. A refusal
    /// by policy or by the backend is `Ok(Err(..))`: the command was
    /// delivered and answered, and the answer was no.
    pub async fn dial(
        &self,
        peer: TransportIdentity,
        address: Multiaddr,
    ) -> Result<Result<(), DialRefusal>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::Dial {
                peer,
                address,
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Send one directed message to an already-connected peer.
    ///
    /// Returns the endpoint the remote resolved it to, which for an
    /// omitted destination is how the caller learns the remote's
    /// default. A `Rejected` answer becomes the local error its coarse
    /// reason maps to — remote `no_route` is
    /// [`RemoteEndpointUnavailable`](DirectError::RemoteEndpointUnavailable),
    /// because that is all the peer disclosed.
    ///
    /// The peer must already be connected; see
    /// [`GatedSwarm::send_direct`](crate::gated_swarm::GatedSwarm::send_direct)
    /// for why an implicit dial is refused rather than attempted.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone; otherwise the
    /// exchange's own outcome.
    pub async fn send_direct(
        &self,
        lease: &interweave_local_client_api::EndpointLease,
        peer: TransportIdentity,
        frame: DirectMessageV2,
    ) -> Result<Result<EndpointId, DirectError>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::SendDirect {
                lease: lease.clone(),
                peer,
                frame: Box::new(frame),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Install endpoint configuration for directed messaging.
    ///
    /// Replaces whatever was there and discards every open queue: this
    /// is the leases changing hands, and a new holder must not inherit
    /// the previous one's undelivered messages.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone.
    pub async fn configure_direct(&self, config: DirectEndpoints) -> Result<(), SubstrateError> {
        // NO VALIDATION HERE. `DirectEndpoints` can only be built by
        // `from_profile`, which runs the canonical `ProfileConfig`
        // validator — duplicate ids, a default naming an absent or
        // disabled endpoint, the endpoint-count ceiling and the queue
        // depth are all decided there. A second copy of those rules on
        // this path is how the two drift.
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::ConfigureDirect {
                config: Box::new(config),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)?
    }

    /// End one endpoint's lease and close its queue with it.
    ///
    /// Returns how many undelivered events were discarded. An offline
    /// endpoint holds no daemon-side backlog, so they are dropped rather
    /// than kept for whoever leases it next.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone.
    pub async fn revoke_endpoint(&self, endpoint: EndpointId) -> Result<usize, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::RevokeEndpoint { endpoint, reply })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Ask `peer` which endpoints it advertises to this profile.
    ///
    /// Cached for the clamped TTL from local receipt; `cached` on the
    /// result says whether the wire was crossed. Advisory (ADR-0031): an
    /// entry here is what the peer CLAIMED, a send needs no prior query,
    /// and a listed endpoint may still answer `no_route` by the time a
    /// message reaches it.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone; the inner error
    /// is why the query did not produce a directory.
    pub async fn query_endpoints(
        &self,
        peer: TransportIdentity,
    ) -> Result<Result<super::endpoints::DirectoryResult, DirectError>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::QueryEndpoints { peer, reply })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Grant `session` an exclusive lease on `endpoint`, opening its queue.
    ///
    /// The source endpoint every later `send_direct` from this session
    /// carries. One lease per session; a second claim is refused, and so
    /// is a claim on an endpoint another session holds.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone; the inner error is
    /// the contract's refusal.
    pub async fn claim_endpoint(
        &self,
        session: impl Into<String>,
        endpoint: EndpointId,
        client_kind: impl Into<String>,
    ) -> Result<Result<interweave_local_client_api::EndpointLease, DirectError>, SubstrateError>
    {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::ClaimEndpoint {
                session: session.into(),
                endpoint,
                client_kind: client_kind.into(),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// End every lease `session` holds, closing each queue with it.
    ///
    /// Returns the endpoints released. What an IPC disconnect will do at
    /// Stage 13.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone.
    pub async fn release_session(
        &self,
        session: impl Into<String>,
    ) -> Result<Vec<EndpointId>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::ReleaseSession {
                session: session.into(),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Take everything waiting on one endpoint's queue, oldest first.
    ///
    /// What an IPC session's event stream will do at Stage 13, pulled
    /// rather than pushed.
    ///
    /// # Errors
    /// [`SubstrateError::Stopped`] if the task is gone.
    pub async fn drain_endpoint(
        &self,
        endpoint: EndpointId,
    ) -> Result<Vec<interweave_transport_runtime::DirectEvent>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::DrainEndpoint { endpoint, reply })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Remember an address as a candidate for `peer`.
    ///
    /// Returns whether it was remembered: an unclassified peer gets no
    /// book entry, and a peer whose eight slots are all dialable keeps
    /// them.
    ///
    /// THIS IS THE OPERATOR'S DOOR (ADR-0052 rule 2 and rule 9). An
    /// address given here is one no peer chose, so it is recorded in the
    /// operator set first and admitted at every learn site and at the
    /// root funnel whatever its class -- the operator's `/dns4` seed
    /// resolves and their LAN seed routes. It follows that a discovery
    /// candidate must never be passed in here: it would be laundered
    /// from the peer's door into the operator's (plan §15, Stage 12's
    /// composition rule). Recorded even when the book declines the
    /// entry, because the set records the door, not the book. Past the
    /// set's bound the address is not recorded, and the refusal is read
    /// through [`Self::operator_addresses_refused`] rather than returned:
    /// the `bool` below is the book's answer.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone.
    pub async fn add_address(
        &self,
        peer: TransportIdentity,
        address: Multiaddr,
    ) -> Result<bool, SubstrateError> {
        let _ = self.operator.insert(&address);
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::AddAddress {
                peer,
                address,
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// The status surface (plan §15): the computed `ConnectivitySummary`
    /// and the dial gate's introspection, photographed at one instant in
    /// the Swarm task. `peer`, when given, adds that peer's retry state.
    ///
    /// Causes nothing: no probe, reservation or hole punch is started by
    /// asking (`CONNECTIVITY.md` §4).
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone.
    pub async fn status(
        &self,
        peer: Option<TransportIdentity>,
    ) -> Result<super::RuntimeStatus, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::Status { peer, reply })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Take a discovery candidate into the book through the PEER'S door
    /// -- the in-boundary learn command plan §15 owes composition.
    ///
    /// Every address meets ADR-0052's discovery predicate -- the floor,
    /// rule 3, and every circuit refused, the peer's own included, since
    /// a candidate may carry a third party's assertion -- and the
    /// refusals are counted under the book's entry in
    /// [`Self::store_refusals`]. The operator set is read, never written:
    /// an address is admitted whatever its class only if the operator's
    /// door already holds it. So this, and not [`Self::add_address`], is
    /// where `DiscoveryManager` candidates go. The two differ in the
    /// boundary, which `add_address` does not apply, and in the operator
    /// set, which only `add_address` writes.
    ///
    /// Returns how many addresses entered the book: an unclassified peer
    /// gets none, as at every learn site.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone.
    pub async fn learn(
        &self,
        peer: TransportIdentity,
        addresses: impl IntoIterator<Item = String>,
    ) -> Result<usize, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::Learn {
                peer,
                addresses: addresses.into_iter().collect(),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Did `address` come in by the operator's door (ADR-0052 rule 9) --
    /// the profile's configuration or [`SwarmRuntime::add_address`]?
    ///
    /// An address this answers `true` for is admitted at every learn
    /// site and at the root funnel whatever its class; anything else
    /// meets the floor. Judged on the route, so a trailing `/p2p/`
    /// suffix on either side does not change the answer.
    #[must_use]
    pub fn is_operator_address(&self, address: &Multiaddr) -> bool {
        self.operator.contains(address)
    }

    /// Operator addresses refused because the set already held
    /// `operator_set::MAX_OPERATOR_ADDRESSES` -- the only trace such a
    /// refusal leaves, since [`Self::add_address`] answers for the BOOK,
    /// not the set. Readable here because a count nothing outside the
    /// task can read is a refusal nobody sees (#111 DNS review P3-4).
    #[must_use]
    pub fn operator_addresses_refused(&self) -> usize {
        self.operator.refused_full()
    }

    /// Reach `peer`: direct first, the relay after a head-start
    /// (`transport/libp2p/CONNECTIVITY.md` §12, step 9).
    ///
    /// A direct connection to the peer that carries the data plane is
    /// reused and nothing is dialled (`Ok`). Else the book's direct
    /// candidates are dialled recently good first, each admitted on its
    /// own -- the ordering is a preference; a quarantined address is
    /// left out of the candidates, and every remaining one still passes
    /// the gate -- and a
    /// circuit route in the book is dialled only after the relay
    /// client's `direct_head_start_ms` has passed with no direct
    /// connection landed, or at once when there is no direct
    /// candidate. The answer is the first dial admitted; a deferred
    /// circuit's outcome reaches the consumer as events -- `Connected`
    /// or `PeerPathChanged`, or a `DialFailed` naming the race if the
    /// gate refuses it when its turn comes.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone. A
    /// refusal is `Ok(Err(..))`, including
    /// [`DialRefusal::NoKnownAddress`] when the book holds nothing for
    /// this peer, and a policy refusal for an infrastructure-only peer
    /// even while a connection to it stands.
    pub async fn dial_peer(
        &self,
        peer: TransportIdentity,
    ) -> Result<Result<(), DialRefusal>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::DialPeer {
                peer,
                reconnect: false,
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Reach `peer` on discovery's account: [`Self::dial_peer`] under
    /// `DialOrigin::DiscoveryReconnect`, and nothing dialled while the
    /// peer holds an open connection or a dial to it is in flight.
    ///
    /// The composition root's reconnection loop toward peers this
    /// profile wants a data-plane connection to (`transport/libp2p/
    /// CONNECTIVITY.md` §11's `discovery-reconnect`); the origin names an
    /// application destination, so an infrastructure-only peer is
    /// refused at the gate.
    ///
    /// # Errors
    /// As [`Self::dial_peer`].
    pub async fn reconnect(
        &self,
        peer: TransportIdentity,
    ) -> Result<Result<(), DialRefusal>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::DialPeer {
                peer,
                reconnect: true,
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Replace the trust sources, evicting connections they no longer
    /// permit.
    ///
    /// Returns how many connections the change closed. ADR-0012 makes
    /// that the observable part: a revocation whose only effect was on
    /// the next dial would leave the revoked peer connected.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone.
    pub async fn set_trust(&self, trust: TrustSources) -> Result<usize, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::SetTrust {
                trust: Box::new(trust),
                reply,
            })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// The next event, or `None` once the substrate has stopped.
    pub async fn next_event(&mut self) -> Option<SwarmEvent> {
        self.events.recv().await
    }

    /// Refuse new connectivity, keeping the connections already up.
    ///
    /// After this, outbound admission answers
    /// [`DialDenial::ShuttingDown`] and no inbound connection is
    /// retained. Existing connections are untouched: a node leaving
    /// service stops taking new work before it drops the work it has.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone.
    pub async fn drain(&self) -> Result<(), SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::Drain { reply })
            .await
            .map_err(|_| SubstrateError::Stopped)?;
        answer.await.map_err(|_| SubstrateError::Stopped)
    }

    /// Stop the substrate and WAIT for its task to finish, returning the
    /// events it emitted that nobody had read.
    ///
    /// The waiting is the point. "Shut down without leaked tasks" is only
    /// checkable if something observed the task ending, and a dropped
    /// handle observes nothing.
    ///
    /// THE EVENTS ARE READ WHILE THE TASK STOPS, not dropped with the
    /// handle: an event already queued -- a `RouteConfirmed` the peer
    /// cache has yet to record -- would otherwise die with the receiver,
    /// and one the task flushes on its way out would find the channel
    /// full with nobody reading (#137 carried N1). Read until the task
    /// drops its sender: the OLDEST kept, up to four times the event
    /// capacity -- the backlog queued when the shutdown began comes
    /// first, and a shutdown waiting out its grace still polls the
    /// network, so what that adds past the bound is read and COUNTED in
    /// [`ShutdownReport::dropped`], never kept without limit and never
    /// dropped unsaid (#138 re-review N1; `collect_unread`).
    /// `shutdown_returns_the_events_nobody_read` pins the return.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task had already
    /// ended — which is not a failure, only a race a caller may want to
    /// know about.
    pub async fn shutdown(mut self) -> Result<ShutdownReport, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        // Best-effort: if the task already ended, the send fails and the
        // join below still confirms it.
        let asked = self
            .commands
            .send(SwarmCommand::Shutdown { reply })
            .await
            .is_ok();
        let unread = collect_unread(&mut self.events, self.unread_capacity).await;
        if asked {
            let _ = answer.await;
        }
        match self.task.take() {
            Some(handle) => handle
                .await
                .map(|()| unread)
                .map_err(|e| SubstrateError::Transport(e.to_string())),
            None => Err(SubstrateError::Stopped),
        }
    }
}

/// What [`SwarmRuntime::shutdown`] read and nobody had.
#[derive(Debug, Default)]
pub struct ShutdownReport {
    /// The events kept, oldest first.
    pub events: Vec<SwarmEvent>,
    /// Events read past the bound and dropped.
    pub dropped: usize,
}

/// Read `events` until its sender is gone, keeping the first `capacity`
/// and counting the rest. `the_unread_keep_the_oldest_and_count_the_rest`
/// pins it.
async fn collect_unread(
    events: &mut mpsc::Receiver<SwarmEvent>,
    capacity: usize,
) -> ShutdownReport {
    let mut report = ShutdownReport::default();
    while let Some(event) = events.recv().await {
        if report.events.len() < capacity {
            report.events.push(event);
        } else {
            report.dropped += 1;
        }
    }
    report
}

impl Drop for SwarmRuntime {
    /// Aborts the task if `shutdown` was not called.
    ///
    /// A safety net so a forgotten runtime cannot outlive its owner, NOT
    /// the intended exit: an abort gives the Swarm no chance to close
    /// connections, and nothing waits to see that it happened.
    fn drop(&mut self) {
        if let Some(handle) = self.task.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod unread_tests {
    use super::{SwarmEvent, collect_unread};
    use interweave_profile_identity::ProfileIdentity;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn the_unread_keep_the_oldest_and_count_the_rest() {
        let (tx, mut rx) = mpsc::channel(16);
        let peers: Vec<_> = (0..6)
            .map(|_| {
                ProfileIdentity::generate()
                    .transport_identity()
                    .expect("peer id")
            })
            .collect();
        for peer in &peers {
            tx.send(SwarmEvent::Disconnected { peer: peer.clone() })
                .await
                .expect("room");
        }
        drop(tx);
        let report = collect_unread(&mut rx, 4).await;
        assert_eq!(report.dropped, 2, "the two past the bound are counted");
        let kept: Vec<_> = report
            .events
            .iter()
            .map(|e| match e {
                SwarmEvent::Disconnected { peer } => peer.clone(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(kept, peers[..4], "the oldest four are kept, in order");
    }
}
