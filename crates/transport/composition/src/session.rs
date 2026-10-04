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
    DataSessionPort, EndpointAdminView, Generation, IngressCounts, LocalAdminPort,
    LocalDataSession, PreAuthCounts, ReceivedBroadcast, ReceivedDirect, SessionEvent,
    SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, DirectMessageV2, EndpointDirectoryV1,
    EndpointId, MessageId, Payload, TransportError, TransportIdentity,
};
use interweave_transport_libp2p::{SubstrateError, SwarmCommander};
use tokio::sync::{mpsc, watch};

use crate::notices::SessionNotices;
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
fn release_now(
    commander: &SwarmCommander,
    runtime: &tokio::runtime::Handle,
    key: &str,
    channels: Vec<ChannelId>,
) {
    if commander.release_detached(key, channels.iter().cloned()) {
        return;
    }
    // ON THE RUNTIME THE BINDING WAS MADE IN, never whichever is current
    // where the drop happens: a session dropped on a thread with no
    // runtime found none, and with the command channel full its lease and
    // joins stayed held for good. A spawn on a stopped runtime is a no-op,
    // and then there is no substrate left holding anything to release.
    let commander = commander.clone();
    let key = key.to_owned();
    runtime.spawn(async move {
        for channel in channels {
            let _ = commander.leave(channel, key.clone()).await;
        }
        let _ = commander.release_session(key).await;
    });
}

/// Releases a claimed lease unless disarmed: an `open` that fails or is
/// cancelled after its claim answered leaves nothing held.
struct ClaimGuard<'a> {
    commander: &'a SwarmCommander,
    runtime: &'a tokio::runtime::Handle,
    key: &'a str,
    armed: bool,
}

impl Drop for ClaimGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            release_now(self.commander, self.runtime, self.key, Vec::new());
        }
    }
}

/// Leaves `channel` unless disarmed: a `join` its caller stopped waiting
/// for may have been joined by the substrate after the command left, and
/// nothing recorded it. A leave of a channel the substrate never joined
/// is a no-op.
///
/// ORDERED WITH THE SESSION'S OTHER MEMBERSHIP CHANGES either way. Where
/// the command channel has room the leave is queued at once, behind the
/// join and before the membership lock is released. Where it is full, the
/// leave is OWED instead -- parked in the session, and sent first by the
/// session's next `join` or `leave` under the lock, or by its teardown.
/// Spawning it was the earlier shape, and a spawned leave could land
/// after the session's next join of the same channel and undo it
/// (#144 re-review 2, F1;
/// `a_leave_owed_on_a_full_channel_is_sent_before_the_next_join`).
struct JoinGuard<'a> {
    commander: &'a SwarmCommander,
    key: &'a str,
    owed: &'a Mutex<BTreeSet<ChannelId>>,
    channel: Option<ChannelId>,
}

impl Drop for JoinGuard<'_> {
    fn drop(&mut self) {
        let Some(channel) = self.channel.take() else {
            return;
        };
        if !self.commander.leave_detached(channel.clone(), self.key) {
            let mut owed = self.owed.lock().unwrap_or_else(PoisonError::into_inner);
            debug_assert!(
                owed.is_empty(),
                "a join armed its guard with a leave still owed"
            );
            owed.insert(channel);
        }
    }
}

/// How long `ready` waits for a wake-up before asking the substrate
/// again. The substrate drops a delivery's wake-up, never the delivery,
/// when its own event allowance is full (`SwarmEvent::DirectDelivered`),
/// so without this a session could wait on a message already queued; the
/// recheck bounds that to this long (`a_lost_wake_is_found_by_the_recheck`).
pub(crate) const READY_RECHECK: Duration = Duration::from_secs(1);

/// `ready`'s wait: until `owed` says something waits here, or the
/// substrate says something waits there or has stopped. What the
/// substrate holds is ASKED, not inferred from the wake-ups: a wake that
/// came while nobody waited is a permit `notified` still sees, and one
/// the substrate dropped under backpressure is what `recheck` is for
/// (`a_lost_wake_is_found_by_the_recheck`).
async fn wait_until_owed<P, F>(
    owed: impl Fn() -> bool,
    mut pending: P,
    wake: &tokio::sync::Notify,
    recheck: Duration,
) where
    P: FnMut() -> F,
    F: std::future::Future<Output = Result<bool, SubstrateError>>,
{
    loop {
        if owed() {
            return;
        }
        match pending().await {
            Ok(false) => {}
            // Pending, or the substrate has stopped: either way the next
            // `events` answers.
            Ok(true) | Err(_) => return,
        }
        tokio::select! {
            () = wake.notified() => {}
            () = tokio::time::sleep(recheck) => {}
        }
    }
}

/// The in-process binding: opens sessions, and admin ports, on the
/// composed runtime.
#[derive(Clone)]
pub struct InProcessBinding {
    commander: SwarmCommander,
    /// The runtime this binding was made in, where a session's teardown
    /// runs when it cannot be queued at once ([`release_now`]).
    runtime: tokio::runtime::Handle,
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
    /// The notices and wake-ups the driver posts to every session holding
    /// `events`.
    notices: SessionNotices,
}

impl InProcessBinding {
    pub(crate) const fn new(
        commander: SwarmCommander,
        runtime: tokio::runtime::Handle,
        queue_bound: usize,
        driver: mpsc::WeakSender<Request>,
        peer: TransportIdentity,
        shutdown: Arc<watch::Sender<Option<ShutdownRequest>>>,
        notices: SessionNotices,
    ) -> Self {
        Self {
            commander,
            runtime,
            queue_bound,
            driver,
            peer,
            shutdown,
            notices,
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
            runtime: &self.runtime,
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
        // Owed the runtime's notices from now, and woken by what is
        // queued for it, if it reads events.
        let wake = session.holds(DataCapability::Events).then(|| {
            self.notices.register(
                session.session_id().as_str(),
                session.endpoint_lease().map(|lease| lease.endpoint.clone()),
            )
        });
        Ok(InProcessSession {
            session,
            notices: self.notices.clone(),
            wake,
            commander: self.commander.clone(),
            runtime: self.runtime.clone(),
            joined: Mutex::new(BTreeSet::new()),
            owed_leaves: Mutex::new(BTreeSet::new()),
            membership: tokio::sync::Mutex::new(()),
            closed: false,
        })
    }
}

/// One open in-process data-plane session. Dropping it ends it, as
/// `close` does, without waiting for the answers.
pub struct InProcessSession {
    /// Its id is the substrate's session key ([`Self::key`]): one value,
    /// so the key a lease was claimed under cannot drift from the session
    /// that holds it.
    session: LocalDataSession,
    /// Where this session's notices are owed; forgotten as it ends.
    notices: SessionNotices,
    /// What `ready` waits on: `Some` exactly when the session holds
    /// `events`.
    wake: Option<Arc<tokio::sync::Notify>>,
    commander: SwarmCommander,
    /// The binding's runtime, for a teardown that cannot be queued at once.
    runtime: tokio::runtime::Handle,
    /// The channels this session joined, left when it ends: the
    /// substrate's session release ends leases, not joins. A join is
    /// recorded when its answer is `Ok` (a refused one's record would be
    /// invisible outside: its leave is a no-op), and a join cancelled in
    /// flight leaves through its guard instead
    /// (`a_cancelled_join_holds_no_join_while_the_session_lives`).
    joined: Mutex<BTreeSet<ChannelId>>,
    /// Held by `join` and `leave` from before their command is sent until
    /// `joined` records the answer, so this session's membership changes
    /// settle in the order they were asked. Without it a `leave` asked
    /// before a `join` but read after it erased that join's record, or a refused join
    /// took back a concurrent accepted one's, and the substrate kept a
    /// join nothing would leave (#144 review F3,
    /// `a_leave_asked_before_a_join_leaves_the_join_recorded`).
    membership: tokio::sync::Mutex<()>,
    /// The leave a cancelled join owed while the command channel was full
    /// (`JoinGuard`), sent first by the next `join` or `leave` under the
    /// membership lock and by teardown. At most ONE channel: a join arms
    /// its guard only after sending what was owed, under the lock every
    /// insert holds, so the guard always finds it empty -- asserted in
    /// `JoinGuard::drop`, which every test that cancels a join runs.
    owed_leaves: Mutex<BTreeSet<ChannelId>>,
    /// Set by `close`, whose own awaited teardown makes `Drop`'s moot.
    closed: bool,
}

impl InProcessSession {
    /// The key the substrate holds this session's leases, joins and queue
    /// under: the session's own id, which `open` claimed under.
    fn key(&self) -> &str {
        self.session.session_id().as_str()
    }

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

    fn owed_leaves(&self) -> MutexGuard<'_, BTreeSet<ChannelId>> {
        self.owed_leaves
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Send the leaves a cancelled join owed, each forgotten once sent.
    /// Called under the membership lock, before the caller's own command.
    async fn send_owed_leaves(&self) -> Result<(), TransportError> {
        let owed: Vec<ChannelId> = self.owed_leaves().iter().cloned().collect();
        for channel in owed {
            self.commander
                .leave(channel.clone(), self.key().to_owned())
                .await
                .map_err(stopped)?;
            self.owed_leaves().remove(&channel);
        }
        Ok(())
    }

    /// What teardown leaves: the recorded joins and the owed leaves.
    fn channels_to_leave(&self) -> Vec<ChannelId> {
        let mut channels = self.joined().clone();
        channels.extend(self.owed_leaves().iter().cloned());
        channels.into_iter().collect()
    }
}

impl Drop for InProcessSession {
    fn drop(&mut self) {
        // Whichever way the session ends: `close` consumes it, so this
        // runs after a close too (`a_sessions_notice_entry_goes_when_it_ends`).
        self.notices.forget(self.key());
        if self.closed {
            return;
        }
        release_now(
            &self.commander,
            &self.runtime,
            self.key(),
            self.channels_to_leave(),
        );
    }
}

impl DataSessionPort for InProcessSession {
    fn session(&self) -> &LocalDataSession {
        &self.session
    }

    async fn join(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        let _settling = self.membership.lock().await;
        self.send_owed_leaves().await?;
        // RECORDED ONCE ACCEPTED, so `joined` holds what the substrate
        // holds. A caller that drops this future after the command left
        // is covered by the guard instead: it queues, or owes, the leave
        // the join may need (#139 review N3). Not armed for a channel
        // already joined: that join is the substrate's no-op, and a leave
        // would end the join it repeats. Declared after `_settling`, so it
        // drops -- and queues or owes its leave -- while this session's
        // membership lock is still held.
        let already = self.joined().contains(&channel);
        let mut guard = JoinGuard {
            commander: &self.commander,
            key: self.key(),
            owed: &self.owed_leaves,
            channel: (!already).then(|| channel.clone()),
        };
        let joined = self
            .commander
            .join(channel.clone(), self.key().to_owned())
            .await
            .map_err(stopped)
            .and_then(|answer| answer);
        guard.channel = None;
        if joined.is_ok() {
            self.joined().insert(channel);
        }
        joined
    }

    async fn leave(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        let _settling = self.membership.lock().await;
        self.send_owed_leaves().await?;
        self.commander
            .leave(channel.clone(), self.key().to_owned())
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
            .publish(channel, self.key().to_owned(), message)
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

    async fn events(&self, max: usize) -> Result<Vec<SessionEvent>, TransportError> {
        self.require(DataCapability::Events)?;
        // The notices first: a revocation read after the messages would
        // arrive after the drain that the revocation emptied. Each queue
        // is drained for what the earlier ones left of `max`, so what is
        // not taken stays queued under its bound (relay seq 9709).
        let owed = self
            .commander
            .take_lease_notices(self.key().to_owned(), max)
            .await
            .map_err(stopped)?;
        // The runtime's notices next -- its state, then the peer
        // disconnects -- the reserved lane's other half: before any
        // message, under the same `max`.
        let mut owed = owed;
        owed.extend(self.notices.take(self.key(), max - owed.len()));
        let room = max - owed.len();
        // LEASE-CHECKED: a lease revoked or replaced drains nothing, so a
        // stale session cannot take the next holder's messages (#139
        // review F1).
        let direct = match self.session.endpoint_lease() {
            Some(lease) if room > 0 => self
                .commander
                .drain_leased(lease, room)
                .await
                .map_err(stopped)?,
            _ => Vec::new(),
        };
        let room = room - direct.len();
        let broadcast = if room > 0 {
            self.commander
                .drain_session(self.key().to_owned(), room)
                .await
                .map_err(stopped)?
        } else {
            Vec::new()
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

    async fn ready(&self) -> Result<(), TransportError> {
        self.require(DataCapability::Events)?;
        let Some(wake) = &self.wake else {
            return Err(TransportError::CapabilityDenied);
        };
        wait_until_owed(
            || self.notices.ready(self.key()),
            || {
                self.commander
                    .session_pending(self.key(), self.session.endpoint_lease())
            },
            wake,
            READY_RECHECK,
        )
        .await;
        Ok(())
    }

    async fn close(mut self) -> Result<(), TransportError> {
        let channels = self.channels_to_leave();
        for channel in channels {
            self.commander
                .leave(channel, self.key().to_owned())
                .await
                .map_err(stopped)?;
        }
        self.commander
            .release_session(self.key().to_owned())
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
        // The funnel's and the ingress limiters' counts, from one of the
        // substrate's own snapshots.
        let substrate = ask_driver(&driver, Request::Diagnostics)
            .await?
            .ok_or(TransportError::BackendUnavailable)?
            .substrate;
        let (funnel, ingress) = (substrate.pre_auth, substrate.ingress);
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
            pre_auth: Some(PreAuthCounts {
                tracked_sources: funnel.tracked_sources,
                pending: funnel.pending,
            }),
            ingress: Some(IngressCounts {
                direct_tracked_peers: ingress.direct_tracked_peers,
                broadcast_tracked_peers: ingress.broadcast_tracked_peers,
            }),
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::wait_until_owed;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A wake the substrate never sent -- dropped under backpressure --
    /// does not leave `ready` waiting on a message already queued: the
    /// recheck asks again and finds it. The control is the first ask,
    /// which found nothing and waited.
    #[tokio::test]
    async fn a_lost_wake_is_found_by_the_recheck() {
        let asked = AtomicUsize::new(0);
        let never_woken = tokio::sync::Notify::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            wait_until_owed(
                || false,
                || {
                    let n = asked.fetch_add(1, Ordering::SeqCst);
                    // Nothing on the first ask; the message is queued by
                    // the second, with no wake sent for it.
                    async move { Ok(n >= 1) }
                },
                &never_woken,
                Duration::from_millis(50),
            ),
        )
        .await
        .expect("the recheck ends the wait with no wake");
        assert_eq!(
            asked.load(Ordering::SeqCst),
            2,
            "asked, waited, asked again"
        );
    }
}
