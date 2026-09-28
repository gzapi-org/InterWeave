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
//! channels it joined, left when it ends; the notices a revocation owes
//! it; and teardown on drop -- for an in-process binding, dropping a
//! session IS its teardown (§3, §7 item 5; #139 review F3).
//!
//! The admin facade is a separate type built from the binding, never from
//! a session (§5): nothing here turns an [`InProcessSession`] into an
//! [`InProcessAdmin`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use interweave_local_client_api::{
    AdminCapability, DataCapability, DataSessionBinding, DataSessionPort, Generation,
    LocalAdminPort, LocalDataSession, LocalSessionEvent, ReceivedBroadcast, ReceivedDirect,
    SessionEvent, SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, DirectMessageV2, EndpointId, MessageId,
    Payload, TransportError,
};
use interweave_transport_libp2p::{SubstrateError, SwarmCommander};

/// A substrate that has stopped answers nothing.
#[allow(clippy::needless_pass_by_value)]
fn stopped(_: SubstrateError) -> TransportError {
    TransportError::BackendUnavailable
}

/// A fresh 128-bit generation (`LOCAL-CLIENT.md` §2, §3).
fn fresh_generation() -> Result<Generation, TransportError> {
    let bytes: [u8; 16] = rand::random();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Generation::parse(hex).map_err(|_| TransportError::InvalidArgument)
}

/// Which session holds each leased endpoint, and the notices owed to
/// sessions: what an admin revocation needs to tell the holder its lease
/// ended. Bounded by the open sessions: a session's entries go when it
/// closes or is dropped.
#[derive(Default)]
struct Notices {
    holders: BTreeMap<EndpointId, (String, Generation)>,
    owed: BTreeMap<String, Vec<LocalSessionEvent>>,
}

impl Notices {
    fn forget(&mut self, key: &str) {
        self.holders.retain(|_, (holder, _)| holder != key);
        self.owed.remove(key);
    }
}

fn lock(notices: &Mutex<Notices>) -> MutexGuard<'_, Notices> {
    notices.lock().unwrap_or_else(PoisonError::into_inner)
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

/// The in-process binding: opens sessions on the composed runtime.
#[derive(Clone)]
pub struct InProcessBinding {
    commander: SwarmCommander,
    queue_bound: usize,
    notices: Arc<Mutex<Notices>>,
}

impl InProcessBinding {
    pub(crate) fn new(commander: SwarmCommander, queue_bound: usize) -> Self {
        Self {
            commander,
            queue_bound,
            notices: Arc::default(),
        }
    }

    /// The administrative facade, for explicit local control code only
    /// (`LOCAL-CLIENT.md` §5). Built from the binding the runtime handed
    /// out, never from a session.
    ///
    /// # Errors
    /// `InvalidArgument` if a port id could not be minted.
    pub fn admin(
        &self,
        capabilities: impl IntoIterator<Item = AdminCapability>,
    ) -> Result<InProcessAdmin, TransportError> {
        Ok(InProcessAdmin {
            port: LocalAdminPort::new(fresh_generation()?, capabilities),
            commander: self.commander.clone(),
            notices: Arc::clone(&self.notices),
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
        if let Some(lease) = lease {
            lock(&self.notices)
                .holders
                .insert(lease.endpoint, (key.clone(), lease.epoch));
        }
        guard.armed = false;
        drop(guard);
        Ok(InProcessSession {
            session,
            key,
            commander: self.commander.clone(),
            joined: Mutex::new(BTreeSet::new()),
            notices: Arc::clone(&self.notices),
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
    notices: Arc<Mutex<Notices>>,
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
        lock(&self.notices).forget(&self.key);
    }
}

impl DataSessionPort for InProcessSession {
    fn session(&self) -> &LocalDataSession {
        &self.session
    }

    async fn join(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        self.commander
            .join(channel.clone(), self.key.clone())
            .await
            .map_err(stopped)??;
        self.joined().insert(channel);
        Ok(())
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
        let owed = lock(&self.notices)
            .owed
            .remove(&self.key)
            .unwrap_or_default();
        let drained = async {
            // LEASE-CHECKED: a lease revoked or replaced drains nothing,
            // so a stale session cannot take the next holder's messages
            // (#139 review F1).
            let direct = match self.session.endpoint_lease() {
                Some(lease) => self.commander.drain_leased(lease).await?,
                None => Vec::new(),
            };
            let broadcast = self.commander.drain_session(self.key.clone()).await?;
            Ok::<_, SubstrateError>((direct, broadcast))
        }
        .await;
        let (direct, broadcast) = match drained {
            Ok(drained) => drained,
            Err(e) => {
                // The notices were taken before the drain: put them back
                // rather than lose them with the failure.
                if !owed.is_empty() {
                    lock(&self.notices)
                        .owed
                        .entry(self.key.clone())
                        .or_default()
                        .splice(0..0, owed);
                }
                return Err(stopped(e));
            }
        };
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
        lock(&self.notices).forget(&self.key);
        self.closed = true;
        Ok(())
    }
}

/// The administrative facade: its own authority, no endpoint lease.
pub struct InProcessAdmin {
    port: LocalAdminPort,
    commander: SwarmCommander,
    notices: Arc<Mutex<Notices>>,
}

impl InProcessAdmin {
    /// The port's identity and authorities.
    #[must_use]
    pub const fn port(&self) -> &LocalAdminPort {
        &self.port
    }

    /// End `endpoint`'s lease, telling its holder, and discard its queue.
    /// Returns how many undelivered events were discarded.
    ///
    /// # Errors
    /// `CapabilityDenied` without `admin.endpoints`, or
    /// `BackendUnavailable` once the runtime has stopped.
    pub async fn revoke_endpoint(&self, endpoint: EndpointId) -> Result<usize, TransportError> {
        if !self.port.holds(AdminCapability::Endpoints) {
            return Err(TransportError::CapabilityDenied);
        }
        // THE HOLDER IS TAKEN BEFORE THE REVOKE, not after: once the
        // substrate has revoked, another session may claim the endpoint
        // and record itself here, and a lookup after the revoke would
        // tell the NEW holder its live lease ended (#139 review F9).
        // Until the revoke lands the substrate refuses any other claim.
        let holder = lock(&self.notices).holders.remove(&endpoint);
        let discarded = self
            .commander
            .revoke_endpoint(endpoint.clone())
            .await
            .map_err(stopped)?;
        if let Some((holder, epoch)) = holder {
            lock(&self.notices).owed.entry(holder).or_default().push(
                LocalSessionEvent::EndpointLeaseChanged {
                    endpoint,
                    revoked_epoch: epoch,
                },
            );
        }
        Ok(discarded)
    }
}
