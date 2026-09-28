// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The direct in-process `LocalDataSession` / `LocalAdminPort` binding
//! (plan §15 (3)): the embedded adapter's core, built against the composed
//! runtime here and wired into the Android app at Stage 17, not twice.
//!
//! A session is a key into the substrate's own session machinery -- its
//! lease table, its endpoint queues, its join references -- reached through
//! the driver, so the invariants `contracts/LOCAL-CLIENT.md` §7 lists are
//! the substrate's, proven once for every binding by
//! `tests/local-client-conformance`. What this adapter adds is the neutral
//! surface and the bookkeeping only a session sees: the channels it
//! joined, left on close; and the notices a revocation owes it.
//!
//! The admin facade is a separate type built from the runtime, never from
//! a session (§5): nothing here turns a [`InProcessSession`] into an
//! [`InProcessAdmin`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use interweave_local_client_api::{
    AdminCapability, DataCapability, DataSessionBinding, DataSessionPort, EndpointLease,
    Generation, LocalAdminPort, LocalDataSession, LocalSessionEvent, ReceivedBroadcast,
    ReceivedDirect, SessionEvent, SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, DirectMessageV2, EndpointId, MessageId,
    Payload, TransportError,
};
use interweave_transport_libp2p::SwarmRuntime;
use interweave_transport_runtime::DirectEvent;
use interweave_transport_runtime::session_queue::BroadcastEvent;
use tokio::sync::{mpsc, oneshot};

use crate::runtime::Request;

/// A session command, answered by the driver against the substrate.
pub(crate) enum SessionCommand {
    Claim {
        session: String,
        endpoint: EndpointId,
        client_kind: String,
        reply: oneshot::Sender<Result<EndpointLease, TransportError>>,
    },
    Release {
        session: String,
        reply: oneshot::Sender<()>,
    },
    Join {
        session: String,
        channel: ChannelId,
        reply: oneshot::Sender<Result<(), TransportError>>,
    },
    Leave {
        session: String,
        channel: ChannelId,
        reply: oneshot::Sender<()>,
    },
    Publish {
        session: String,
        channel: ChannelId,
        frame: Box<BroadcastMessageV1>,
        reply: oneshot::Sender<Result<(), TransportError>>,
    },
    SendDirect {
        lease: EndpointLease,
        peer: interweave_transport_api::TransportIdentity,
        frame: Box<DirectMessageV2>,
        reply: oneshot::Sender<Result<EndpointId, TransportError>>,
    },
    Drain {
        session: String,
        endpoint: Option<EndpointId>,
        reply: oneshot::Sender<(Vec<DirectEvent>, Vec<BroadcastEvent>)>,
    },
    Revoke {
        endpoint: EndpointId,
        reply: oneshot::Sender<usize>,
    },
}

impl SessionCommand {
    /// Run against the substrate. A substrate that has stopped answers
    /// nothing: the reply is dropped, and the caller reads that as
    /// `BackendUnavailable`.
    pub(crate) async fn run(self, swarm: &SwarmRuntime) {
        match self {
            Self::Claim {
                session,
                endpoint,
                client_kind,
                reply,
            } => {
                if let Ok(answer) = swarm.claim_endpoint(session, endpoint, client_kind).await {
                    let _ = reply.send(answer);
                }
            }
            Self::Release { session, reply } => {
                if swarm.release_session(session).await.is_ok() {
                    let _ = reply.send(());
                }
            }
            Self::Join {
                session,
                channel,
                reply,
            } => {
                if let Ok(answer) = swarm.join(channel, session).await {
                    let _ = reply.send(answer);
                }
            }
            Self::Leave {
                session,
                channel,
                reply,
            } => {
                if swarm.leave(channel, session).await.is_ok() {
                    let _ = reply.send(());
                }
            }
            Self::Publish {
                session,
                channel,
                frame,
                reply,
            } => {
                if let Ok(answer) = swarm.publish(channel, session, *frame).await {
                    let _ = reply.send(answer);
                }
            }
            Self::SendDirect {
                lease,
                peer,
                frame,
                reply,
            } => {
                if let Ok(answer) = swarm.send_direct(&lease, peer, *frame).await {
                    let _ = reply.send(answer);
                }
            }
            Self::Drain {
                session,
                endpoint,
                reply,
            } => {
                let direct = match endpoint {
                    Some(endpoint) => match swarm.drain_endpoint(endpoint).await {
                        Ok(events) => events,
                        Err(_) => return,
                    },
                    None => Vec::new(),
                };
                if let Ok(broadcast) = swarm.drain_session(session).await {
                    let _ = reply.send((direct, broadcast));
                }
            }
            Self::Revoke { endpoint, reply } => {
                if let Ok(discarded) = swarm.revoke_endpoint(endpoint).await {
                    let _ = reply.send(discarded);
                }
            }
        }
    }
}

/// Ask the driver, and read a stopped runtime as `BackendUnavailable`.
async fn ask<T>(
    requests: &mpsc::Sender<Request>,
    make: impl FnOnce(oneshot::Sender<T>) -> SessionCommand,
) -> Result<T, TransportError> {
    let (reply, answer) = oneshot::channel();
    requests
        .send(Request::Session(make(reply)))
        .await
        .map_err(|_| TransportError::BackendUnavailable)?;
    answer.await.map_err(|_| TransportError::BackendUnavailable)
}

/// A fresh 128-bit generation (`LOCAL-CLIENT.md` §2, §3).
fn fresh_generation() -> Result<Generation, TransportError> {
    let bytes: [u8; 16] = rand::random();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Generation::parse(hex).map_err(|_| TransportError::InvalidArgument)
}

/// Which session holds each leased endpoint, and the notices owed to
/// sessions: what an admin revocation needs to tell the holder its lease
/// ended. Bounded by the open sessions -- an entry goes when its session
/// closes.
#[derive(Default)]
struct Notices {
    holders: BTreeMap<EndpointId, (String, Generation)>,
    owed: BTreeMap<String, Vec<LocalSessionEvent>>,
}

fn lock(notices: &Mutex<Notices>) -> std::sync::MutexGuard<'_, Notices> {
    notices.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The in-process binding: opens sessions on the composed runtime.
#[derive(Clone)]
pub struct InProcessBinding {
    requests: mpsc::Sender<Request>,
    queue_bound: usize,
    notices: Arc<Mutex<Notices>>,
}

impl InProcessBinding {
    pub(crate) fn new(requests: mpsc::Sender<Request>, queue_bound: usize) -> Self {
        Self {
            requests,
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
            requests: self.requests.clone(),
            notices: Arc::clone(&self.notices),
        })
    }
}

impl DataSessionBinding for InProcessBinding {
    type Session = InProcessSession;

    async fn open(&self, request: SessionRequest) -> Result<InProcessSession, TransportError> {
        let session_id = fresh_generation()?;
        let key = session_id.as_str().to_owned();
        let lease = match request.endpoint() {
            Some(endpoint) => {
                let endpoint = endpoint.clone();
                let client_kind = request.client_kind().to_owned();
                let session = key.clone();
                Some(
                    ask(&self.requests, |reply| SessionCommand::Claim {
                        session,
                        endpoint,
                        client_kind,
                        reply,
                    })
                    .await??,
                )
            }
            None => None,
        };
        let session = match LocalDataSession::new(
            session_id,
            request.client_kind(),
            lease.clone(),
            request.capabilities().iter().copied(),
            self.queue_bound,
        ) {
            Ok(session) => session,
            Err(_) => {
                // Nothing may outlive a refused open: the lease just
                // claimed goes back.
                let session = key.clone();
                let _ = ask(&self.requests, |reply| SessionCommand::Release {
                    session,
                    reply,
                })
                .await;
                return Err(TransportError::InvalidArgument);
            }
        };
        if let Some(lease) = lease {
            lock(&self.notices)
                .holders
                .insert(lease.endpoint, (key.clone(), lease.epoch));
        }
        Ok(InProcessSession {
            session,
            key,
            requests: self.requests.clone(),
            joined: Mutex::new(BTreeSet::new()),
            notices: Arc::clone(&self.notices),
        })
    }
}

/// One open in-process data-plane session.
pub struct InProcessSession {
    session: LocalDataSession,
    key: String,
    requests: mpsc::Sender<Request>,
    /// The channels this session joined, left on close: the substrate's
    /// session release ends leases, not joins. Bounded by the
    /// subscription ceiling the substrate enforces at join.
    joined: Mutex<BTreeSet<ChannelId>>,
    notices: Arc<Mutex<Notices>>,
}

impl InProcessSession {
    fn require(&self, capability: DataCapability) -> Result<(), TransportError> {
        if self.session.holds(capability) {
            Ok(())
        } else {
            Err(TransportError::CapabilityDenied)
        }
    }

    fn joined(&self) -> std::sync::MutexGuard<'_, BTreeSet<ChannelId>> {
        self.joined.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl DataSessionPort for InProcessSession {
    fn session(&self) -> &LocalDataSession {
        &self.session
    }

    async fn join(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        let session = self.key.clone();
        let joining = channel.clone();
        ask(&self.requests, |reply| SessionCommand::Join {
            session,
            channel: joining,
            reply,
        })
        .await??;
        self.joined().insert(channel);
        Ok(())
    }

    async fn leave(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        let session = self.key.clone();
        let leaving = channel.clone();
        ask(&self.requests, |reply| SessionCommand::Leave {
            session,
            channel: leaving,
            reply,
        })
        .await?;
        self.joined().remove(&channel);
        Ok(())
    }

    async fn broadcast(
        &self,
        channel: ChannelId,
        message: BroadcastMessageV1,
    ) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        let session = self.key.clone();
        ask(&self.requests, |reply| SessionCommand::Publish {
            session,
            channel,
            frame: Box::new(message),
            reply,
        })
        .await?
    }

    async fn send_direct(
        &self,
        destination: DirectDestination,
        message_id: MessageId,
        payload: Payload,
    ) -> Result<EndpointId, TransportError> {
        let source = self.session.authorize_direct_send()?.clone();
        let Some(lease) = self.session.endpoint_lease().cloned() else {
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
        ask(&self.requests, |reply| SessionCommand::SendDirect {
            lease,
            peer: destination.peer,
            frame: Box::new(frame),
            reply,
        })
        .await?
    }

    async fn events(&self) -> Result<Vec<SessionEvent>, TransportError> {
        self.require(DataCapability::Events)?;
        let owed = lock(&self.notices)
            .owed
            .remove(&self.key)
            .unwrap_or_default();
        let session = self.key.clone();
        let endpoint = self.session.source_endpoint().cloned();
        let (direct, broadcast) = ask(&self.requests, |reply| SessionCommand::Drain {
            session,
            endpoint,
            reply,
        })
        .await?;
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

    async fn close(self) -> Result<(), TransportError> {
        let channels: Vec<ChannelId> = self.joined().iter().cloned().collect();
        for channel in channels {
            let session = self.key.clone();
            ask(&self.requests, |reply| SessionCommand::Leave {
                session,
                channel,
                reply,
            })
            .await?;
        }
        let session = self.key.clone();
        ask(&self.requests, |reply| SessionCommand::Release {
            session,
            reply,
        })
        .await?;
        let mut notices = lock(&self.notices);
        notices.holders.retain(|_, (holder, _)| holder != &self.key);
        notices.owed.remove(&self.key);
        Ok(())
    }
}

/// The administrative facade: its own authority, no endpoint lease.
pub struct InProcessAdmin {
    port: LocalAdminPort,
    requests: mpsc::Sender<Request>,
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
        let revoking = endpoint.clone();
        let discarded = ask(&self.requests, |reply| SessionCommand::Revoke {
            endpoint: revoking,
            reply,
        })
        .await?;
        let mut notices = lock(&self.notices);
        if let Some((holder, epoch)) = notices.holders.remove(&endpoint) {
            notices
                .owed
                .entry(holder)
                .or_default()
                .push(LocalSessionEvent::EndpointLeaseChanged {
                    endpoint,
                    revoked_epoch: epoch,
                });
        }
        Ok(discarded)
    }
}
