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
use tokio::sync::oneshot;

use interweave_transport_api::TransportError as DirectError;
use interweave_transport_api::{DirectMessageV2, EndpointId, TransportIdentity};
use interweave_transport_runtime::TrustSources;

use super::SwarmRuntime;
use super::config::SubstrateError;
use super::direct::DirectEndpoints;
use super::messages::{DialRefusal, SwarmCommand, SwarmEvent};

impl SwarmRuntime {
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
        peer: TransportIdentity,
        frame: DirectMessageV2,
    ) -> Result<Result<EndpointId, DirectError>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::SendDirect {
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

    /// Take everything waiting on one endpoint's queue, oldest first.
    ///
    /// The in-process stand-in for what Stage 8's IPC session does.
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
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone.
    pub async fn add_address(
        &self,
        peer: TransportIdentity,
        address: Multiaddr,
    ) -> Result<bool, SubstrateError> {
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

    /// Dial `peer` at the best address known for it.
    ///
    /// Known-good routes are tried first, and each candidate is
    /// admitted on its own: the ordering is a preference, and a
    /// quarantined address is refused by the gate rather than skipped
    /// by the sort.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task is gone. A
    /// refusal is `Ok(Err(..))`, including
    /// [`DialRefusal::NoKnownAddress`] when the book holds nothing for
    /// this peer.
    pub async fn dial_peer(
        &self,
        peer: TransportIdentity,
    ) -> Result<Result<(), DialRefusal>, SubstrateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(SwarmCommand::DialPeer { peer, reply })
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

    /// Stop the substrate and WAIT for its task to finish.
    ///
    /// The waiting is the point. "Shut down without leaked tasks" is only
    /// checkable if something observed the task ending, and a dropped
    /// handle observes nothing.
    ///
    /// # Errors
    /// Returns [`SubstrateError::Stopped`] if the task had already
    /// ended — which is not a failure, only a race a caller may want to
    /// know about.
    pub async fn shutdown(mut self) -> Result<(), SubstrateError> {
        let (reply, answer) = oneshot::channel();
        // Best-effort: if the task already ended, the send fails and the
        // join below still confirms it.
        if self
            .commands
            .send(SwarmCommand::Shutdown { reply })
            .await
            .is_ok()
        {
            let _ = answer.await;
        }
        match self.task.take() {
            Some(handle) => handle
                .await
                .map_err(|e| SubstrateError::Transport(e.to_string())),
            None => Err(SubstrateError::Stopped),
        }
    }
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
