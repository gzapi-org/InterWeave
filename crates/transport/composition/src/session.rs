// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The direct in-process `LocalDataSession` / `LocalAdminPort` binding
//! (plan §15 (3)): the embedded adapter's core, built against the composed
//! runtime here and wired into the Android app at Stage 17, not twice.
//!
//! A session is a key into the substrate's own session machinery -- its
//! lease table, its endpoint queues, its join references -- reached through
//! a [`SwarmCommander`], so the invariants `contracts/LOCAL-CLIENT.md` §7
//! lists are the substrate's, proven once for every binding by
//! `tests/local-client-conformance`. Straight to the substrate, not
//! through the runtime's driver: a direct send waits out its peer for up
//! to the request timeout, and on the driver that wait stalled every
//! event, every discovery round and every other session (#139 review F2).
//!
//! What this adapter adds is the bookkeeping only a session sees: the
//! channels it joined, left when it ends, and teardown on drop -- for an
//! in-process binding, dropping a session IS its teardown (§3, §7 item 5;
//! #139 review F3). The notice a revocation owes a session is the
//! substrate's, kept beside the lease table that knows who held the lease.
//!
//! The admin facade is a separate type built from the binding, never from
//! a session (§5): nothing here turns an [`InProcessSession`] into an
//! [`InProcessAdmin`].

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, AdminStatus, DataCapability, DataSessionBinding,
    DataSessionPort, EndpointAdminView, Generation, LocalAdminPort, LocalDataSession,
    ReceivedBroadcast, ReceivedDirect, SessionEvent, SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, DirectMessageV2, EndpointDirectoryV1,
    EndpointId, MessageId, Payload, TransportError, TransportIdentity,
};
use interweave_transport_libp2p::{SubstrateError, SwarmCommander};
use tokio::sync::{mpsc, watch};

use crate::runtime::{Request, ShutdownRequest, ask_driver};

/// A substrate that has stopped answers nothing.
#[allow(clippy::needless_pass_by_value)]
fn stopped(_: SubstrateError) -> TransportError {
    TransportError::BackendUnavailable
}

/// A fresh 128-bit generation (`LOCAL-CLIENT.md` §2, §3).
fn fresh_generation() -> Result<Generation, TransportError> {
    let bytes: [u8; 16] = rand::random();
    let hex: String = bytes.iter().fold(String::new(), |mut s, b| {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        s
    });
    Generation::parse(hex).map_err(|_| TransportError::InvalidArgument)
}

/// Release `key`'s leases and `channels` without awaiting: what a session
/// that ends without `close` owes. Queued at once where the command
/// channel has room; otherwise sent from a task when a runtime is there
/// to run one. Leaves and a release are idempotent, so a partial first
/// attempt followed by the task is harmless.
fn release_now(commander: &SwarmCommander, key: &str, channels: Vec<ChannelId>) {
    if commander.release_detached(key, channels.iter().cloned()) {
        return;
    }
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let commander = commander.clone();
        let key = key.to_owned();
        runtime.spawn(async move {
            for channel in channels {
                let _ = commander.leave(channel, key.clone()).await;
            }
            let _ = commander.release_session(key).await;
        });
    }
}

/// Releases a claimed lease unless disarmed: an `open` that fails or is
/// cancelled after its claim answered leaves nothing held.
struct ClaimGuard<'a> {
    commander: &'a SwarmCommander,
    key: &'a str,
    armed: bool,
}

impl Drop for ClaimGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            release_now(self.commander, self.key, Vec::new());
        }
    }
}

/// The in-process binding: opens sessions, and admin ports, on the
/// composed runtime.
#[derive(Clone)]
pub struct InProcessBinding {
    commander: SwarmCommander,
    queue_bound: usize,
    /// The runtime's driver, asked for the health and the summary only it
    /// computes; never for a session's own exchange (#139 review F2).
    ///
    /// WEAK, so the runtime's owner holds the only strong sender: dropping
    /// the `ComposedRuntime` closes the channel and ends the driver however
    /// many bindings and ports are still held (#144 review F1,
    /// `dropping_the_runtime_ends_it_while_a_binding_is_held`).
    driver: mpsc::WeakSender<Request>,
    peer: TransportIdentity,
    /// Where an admin port's shutdown request goes: to the runtime's
    /// owner, which stops it (plan §16 (2)).
    shutdown: Arc<watch::Sender<Option<ShutdownRequest>>>,
}

impl InProcessBinding {
    pub(crate) const fn new(
        commander: SwarmCommander,
        queue_bound: usize,
        driver: mpsc::WeakSender<Request>,
        peer: TransportIdentity,
        shutdown: Arc<watch::Sender<Option<ShutdownRequest>>>,
    ) -> Self {
        Self {
            commander,
            queue_bound,
            driver,
            peer,
            shutdown,
        }
    }
}

/// The administrative facade, for explicit local control code only
/// (`LOCAL-CLIENT.md` §5): built from the binding the runtime handed out,
/// never from a session.
impl AdminBinding for InProcessBinding {
    type Admin = InProcessAdmin;

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the trait is async for the IPC binding, whose port opens a socket"
    )]
    async fn admin(
        &self,
        capabilities: BTreeSet<AdminCapability>,
    ) -> Result<InProcessAdmin, TransportError> {
        Ok(InProcessAdmin {
            port: LocalAdminPort::new(fresh_generation()?, capabilities),
            commander: self.commander.clone(),
            driver: self.driver.clone(),
            peer: self.peer.clone(),
            shutdown: Arc::clone(&self.shutdown),
        })
    }
}

impl DataSessionBinding for InProcessBinding {
    type Session = InProcessSession;

    async fn open(&self, request: SessionRequest) -> Result<InProcessSession, TransportError> {
        let session_id = fresh_generation()?;
        let key = session_id.as_str().to_owned();
        let mut guard = ClaimGuard {
            commander: &self.commander,
            key: &key,
            armed: false,
        };
        let lease = match request.endpoint() {
            Some(endpoint) => {
                // Armed BEFORE the claim is sent: a cancellation between
                // its answer and this function's return must release it.
                guard.armed = true;
                Some(
                    self.commander
                        .claim_endpoint(key.clone(), endpoint.clone(), request.client_kind())
                        .await
                        .map_err(stopped)??,
                )
            }
            None => None,
        };
        let session = LocalDataSession::new(
            session_id,
            request.client_kind(),
            lease.clone(),
            request.capabilities().iter().copied(),
            self.queue_bound,
        )
        .map_err(|_| TransportError::InvalidArgument)?;
        guard.armed = false;
        drop(guard);
        Ok(InProcessSession {
            session,
            key,
            commander: self.commander.clone(),
            joined: Mutex::new(BTreeSet::new()),
            closed: false,
        })
    }
}

/// One open in-process data-plane session. Dropping it ends it, as
/// `close` does, without waiting for the answers.
pub struct InProcessSession {
    session: LocalDataSession,
    key: String,
    commander: SwarmCommander,
    /// The channels this session joined, left when it ends: the
    /// substrate's session release ends leases, not joins. Bounded by the
    /// subscription ceiling the substrate enforces at join.
    joined: Mutex<BTreeSet<ChannelId>>,
    /// Set by `close`, whose own awaited teardown makes `Drop`'s moot.
    closed: bool,
}

impl InProcessSession {
    fn require(&self, capability: DataCapability) -> Result<(), TransportError> {
        if self.session.holds(capability) {
            Ok(())
        } else {
            Err(TransportError::CapabilityDenied)
        }
    }

    fn joined(&self) -> MutexGuard<'_, BTreeSet<ChannelId>> {
        self.joined.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for InProcessSession {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let channels: Vec<ChannelId> = self.joined().iter().cloned().collect();
        release_now(&self.commander, &self.key, channels);
    }
}

impl DataSessionPort for InProcessSession {
    fn session(&self) -> &LocalDataSession {
        &self.session
    }

    async fn join(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        // RECORDED BEFORE THE JOIN IS SENT: a caller that drops this
        // future after the substrate joined must still leave on teardown,
        // and a join recorded but refused is taken back below. A leave of
        // a channel never joined is a no-op, so the early record costs
        // nothing (#139 review N3).
        let fresh = self.joined().insert(channel.clone());
        let joined = self
            .commander
            .join(channel.clone(), self.key.clone())
            .await
            .map_err(stopped)
            .and_then(|answer| answer);
        if joined.is_err() && fresh {
            self.joined().remove(&channel);
        }
        joined
    }

    async fn leave(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        self.commander
            .leave(channel.clone(), self.key.clone())
            .await
            .map_err(stopped)?;
        self.joined().remove(&channel);
        Ok(())
    }

    async fn broadcast(
        &self,
        channel: ChannelId,
        message: BroadcastMessageV1,
    ) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        self.commander
            .publish(channel, self.key.clone(), message)
            .await
            .map_err(stopped)?
    }

    async fn send_direct(
        &self,
        destination: DirectDestination,
        message_id: MessageId,
        payload: Payload,
    ) -> Result<EndpointId, TransportError> {
        let source = self.session.authorize_direct_send()?.clone();
        let Some(lease) = self.session.endpoint_lease() else {
            return Err(TransportError::EndpointNotRegistered);
        };
        // The source named here is the lease's own, and the substrate
        // REPLACES it from the lease regardless: the frame field exists
        // because the wire needs one, not because a caller supplies it.
        let frame = DirectMessageV2 {
            message_id,
            sent_at_ms: crate::runtime::wall_ms(),
            source_endpoint: source,
            destination_endpoint: destination.endpoint,
            payload,
        };
        self.commander
            .send_direct(lease, destination.peer, frame)
            .await
            .map_err(stopped)?
    }

    async fn events(&self) -> Result<Vec<SessionEvent>, TransportError> {
        self.require(DataCapability::Events)?;
        // The notices first: a revocation read after the messages would
        // arrive after the drain that the revocation emptied.
        let owed = self
            .commander
            .take_lease_notices(self.key.clone())
            .await
            .map_err(stopped)?;
        // LEASE-CHECKED: a lease revoked or replaced drains nothing, so a
        // stale session cannot take the next holder's messages (#139
        // review F1).
        let direct = match self.session.endpoint_lease() {
            Some(lease) => self.commander.drain_leased(lease).await.map_err(stopped)?,
            None => Vec::new(),
        };
        let broadcast = self
            .commander
            .drain_session(self.key.clone())
            .await
            .map_err(stopped)?;
        let mut events: Vec<SessionEvent> = owed.into_iter().map(SessionEvent::Local).collect();
        events.extend(direct.into_iter().map(|e| {
            SessionEvent::Direct(ReceivedDirect {
                source_peer: e.source_peer,
                source_endpoint: e.source_endpoint,
                destination_endpoint: e.destination_endpoint,
                message_id: e.message_id,
                payload: e.payload,
                received_at_ms: e.received_at,
            })
        }));
        events.extend(broadcast.into_iter().map(|e| {
            SessionEvent::Broadcast(ReceivedBroadcast {
                source_peer: e.source_peer,
                channel: e.channel,
                message_id: e.message_id,
                payload: e.payload,
                received_at_ms: e.received_at,
            })
        }));
        Ok(events)
    }

    async fn close(mut self) -> Result<(), TransportError> {
        let channels: Vec<ChannelId> = self.joined().iter().cloned().collect();
        for channel in channels {
            self.commander
                .leave(channel, self.key.clone())
                .await
                .map_err(stopped)?;
        }
        self.commander
            .release_session(self.key.clone())
            .await
            .map_err(stopped)?;
        self.closed = true;
        Ok(())
    }

    async fn query_endpoints(
        &self,
        peer: TransportIdentity,
    ) -> Result<EndpointDirectoryV1, TransportError> {
        self.require(DataCapability::EndpointsQuery)?;
        let found = self
            .commander
            .query_endpoints(peer)
            .await
            .map_err(stopped)??;
        Ok(EndpointDirectoryV1 {
            generated_at_ms: found.generated_at_ms,
            // What remains of the clamped freshness, from now: never more
            // than the 300 s ceiling, so the conversion cannot saturate.
            ttl_ms: u32::try_from(found.fresh_for_ms).unwrap_or(u32::MAX),
            endpoints: found.endpoints,
        })
    }
}

/// The administrative facade: its own authority, no endpoint lease.
pub struct InProcessAdmin {
    port: LocalAdminPort,
    commander: SwarmCommander,
    /// Weak for the reason the binding's is.
    driver: mpsc::WeakSender<Request>,
    peer: TransportIdentity,
    shutdown: Arc<watch::Sender<Option<ShutdownRequest>>>,
}

impl InProcessAdmin {
    /// The driver, while its owner still holds the runtime.
    fn driver(&self) -> Result<mpsc::Sender<Request>, TransportError> {
        self.driver
            .upgrade()
            .ok_or(TransportError::BackendUnavailable)
    }

    fn require(&self, capability: AdminCapability) -> Result<(), TransportError> {
        if self.port.holds(capability) {
            Ok(())
        } else {
            Err(TransportError::CapabilityDenied)
        }
    }
}

impl AdminPort for InProcessAdmin {
    fn port(&self) -> &LocalAdminPort {
        &self.port
    }

    async fn status(&self) -> Result<AdminStatus, TransportError> {
        self.require(AdminCapability::Status)?;
        let driver = self.driver()?;
        let health = ask_driver(&driver, Request::Health).await?;
        let connectivity = ask_driver(&driver, Request::Connectivity)
            .await?
            .ok_or(TransportError::BackendUnavailable)?;
        // Not held past the driver's answers: a strong sender outliving
        // them would keep a dropped runtime's driver alive.
        drop(driver);
        let active_leases = self
            .commander
            .list_endpoints()
            .await
            .map_err(stopped)?
            .iter()
            .filter(|view| view.lease.is_some())
            .count();
        Ok(AdminStatus {
            health: health.aggregate,
            peer: self.peer.clone(),
            connectivity,
            active_leases,
        })
    }

    async fn leases(&self) -> Result<Vec<EndpointAdminView>, TransportError> {
        self.require(AdminCapability::Endpoints)?;
        self.commander.list_endpoints().await.map_err(stopped)
    }

    async fn revoke_endpoint(&self, endpoint: EndpointId) -> Result<(), TransportError> {
        self.require(AdminCapability::Endpoints)?;
        // The holder's notice is the substrate's to record, in the same
        // step that ends the lease: nothing here can name the wrong
        // holder, and an administrator that stops waiting after the
        // command left still leaves the notice owed (#139 review N2).
        self.commander
            .revoke_endpoint(endpoint)
            .await
            .map_err(stopped)?;
        Ok(())
    }

    async fn set_endpoint_enabled(
        &self,
        endpoint: EndpointId,
        enabled: bool,
    ) -> Result<Option<Generation>, TransportError> {
        self.require(AdminCapability::Endpoints)?;
        self.commander
            .set_endpoint_enabled(endpoint, enabled)
            .await
            .map_err(stopped)?
    }

    async fn set_default_endpoint(
        &self,
        endpoint: Option<EndpointId>,
    ) -> Result<(), TransportError> {
        self.require(AdminCapability::Endpoints)?;
        self.commander
            .set_default_endpoint(endpoint)
            .await
            .map_err(stopped)?
    }

    /// Signals the runtime's owner, which stops it: this port does not own
    /// the runtime. The FIRST request stands; a later one changes nothing,
    /// so a second port cannot shorten a grace already granted.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the trait is async for the IPC binding, whose request crosses a socket"
    )]
    async fn shutdown(&self, grace: Duration) -> Result<(), TransportError> {
        self.require(AdminCapability::Shutdown)?;
        if self.shutdown.is_closed() {
            return Err(TransportError::BackendUnavailable);
        }
        let request = ShutdownRequest {
            port: self.port.port_id().clone(),
            grace,
        };
        self.shutdown.send_if_modified(|pending| {
            if pending.is_some() {
                return false;
            }
            *pending = Some(request);
            true
        });
        Ok(())
    }
}
