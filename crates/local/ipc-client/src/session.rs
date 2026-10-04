// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The binding and its data session.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use interweave_ipc_protocol::{
    ChannelParams, ClientInfo, DirectoryResult, EmptyResult, EndpointClaim, FEATURE_KEEPALIVE,
    Hello, HelloResponse, HelloTag, IPC_MAJOR, IPC_MAX_MINOR, IpcVersion, PublishParams,
    QueryParams, Request, RequestedCapability, SendParams, SendResult,
};
use interweave_local_client_api::{
    DEFAULT_EVENT_QUEUE, DataCapability, DataSessionBinding, DataSessionPort, EndpointLease,
    Generation, LocalDataSession, MAX_EVENT_QUEUE, SessionEvent, SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointDirectoryV1, EndpointId, MessageId,
    Payload, TransportError, TransportIdentity,
};
use tokio::sync::{Mutex, mpsc};

use crate::connection::{Connection, Inbox, open};

/// Where the daemon listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketPaths {
    /// The data socket: sessions.
    pub data: PathBuf,
    /// The admin socket: administrative ports.
    pub admin: PathBuf,
}

/// The desktop IPC binding: opens sessions and admin ports on a daemon's
/// sockets.
#[derive(Debug, Clone)]
pub struct IpcBinding {
    paths: Arc<SocketPaths>,
    admin_kind: String,
}

impl IpcBinding {
    /// A binding to the daemon at `paths`. `admin_kind` is the client kind
    /// its admin ports present: a hygiene label, never authority (ADR-0037).
    #[must_use]
    pub fn new(paths: SocketPaths, admin_kind: impl Into<String>) -> Self {
        Self {
            paths: Arc::new(paths),
            admin_kind: admin_kind.into(),
        }
    }

    pub(crate) fn paths(&self) -> &SocketPaths {
        &self.paths
    }

    pub(crate) fn admin_kind(&self) -> &str {
        &self.admin_kind
    }
}

/// A session's and a port's local id: 128 bits, opaque, never on the wire
/// (`LOCAL-CLIENT.md`, A 2026-09-30).
pub(crate) fn mint() -> Generation {
    let bits: u128 = rand::random();
    Generation::parse(format!("{bits:032x}"))
        .unwrap_or_else(|_| unreachable!("32 hex digits are a generation"))
}

pub(crate) const fn requested(capability: DataCapability) -> RequestedCapability {
    match capability {
        DataCapability::Events => RequestedCapability::Events,
        DataCapability::Commands => RequestedCapability::Commands,
        DataCapability::EndpointsQuery => RequestedCapability::EndpointsQuery,
    }
}

pub(crate) fn hello(
    kind: &str,
    endpoint: Option<&EndpointId>,
    requested_capabilities: BTreeSet<RequestedCapability>,
    features: BTreeSet<String>,
) -> Hello {
    Hello {
        frame_type: HelloTag::Hello,
        ipc_version: IpcVersion {
            major: IPC_MAJOR,
            minor: IPC_MAX_MINOR,
        },
        client: ClientInfo {
            kind: kind.to_owned(),
            version: None,
        },
        endpoint: endpoint.map(EndpointClaim::new),
        requested_capabilities,
        features,
    }
}

/// The bound the server granted with the lease, or the contract's default
/// for a session it granted none; never past what a session may hold.
fn receive_buffer(response: &HelloResponse) -> usize {
    response
        .lease
        .as_ref()
        .map_or(DEFAULT_EVENT_QUEUE, |lease| {
            usize::try_from(lease.event_queue.get()).unwrap_or(usize::MAX)
        })
        .min(MAX_EVENT_QUEUE)
}

/// The receive buffer to open for a response: none unless `events` was
/// granted, since events are pushed only to a session that holds it.
fn events_buffer(response: &HelloResponse) -> Option<usize> {
    response
        .granted_capabilities
        .contains(&RequestedCapability::Events)
        .then(|| receive_buffer(response))
}

impl DataSessionBinding for IpcBinding {
    type Session = IpcSession;

    async fn open(&self, request: SessionRequest) -> Result<IpcSession, TransportError> {
        let hello = hello(
            request.client_kind(),
            request.endpoint(),
            request
                .capabilities()
                .iter()
                .copied()
                .map(requested)
                .collect(),
            // Always offered: a profile that requires keepalive for a lease
            // refuses a claim without it, and the reader answers pings.
            [FEATURE_KEEPALIVE.to_owned()].into(),
        );
        let opened = open(&self.paths().data, hello, events_buffer).await?;
        let response = opened.response;
        let lease = match (request.endpoint(), &response.lease) {
            (Some(asked), Some(granted)) if &granted.endpoint == asked => Some(EndpointLease {
                endpoint: granted.endpoint.clone(),
                epoch: granted.endpoint_lease_epoch.clone(),
            }),
            (None, None) => None,
            // A grant for another endpoint, or none for a claim the server
            // did not refuse: the server broke the handshake.
            _ => return Err(TransportError::ProtocolViolation),
        };
        let session = LocalDataSession::new(
            mint(),
            request.client_kind(),
            lease,
            response
                .granted_capabilities
                .iter()
                .filter_map(|c| c.as_data()),
            receive_buffer(&response),
        )
        .map_err(|_| TransportError::ProtocolViolation)?;
        Ok(IpcSession {
            session,
            connection: opened.connection,
            events: opened
                .events
                .map(|(buffer, inbox)| (Mutex::new(buffer), inbox)),
        })
    }
}

/// A data-plane session over the daemon's data socket.
///
/// Its receive buffer is bounded at the granted `event_queue`: when it is
/// full the client stops reading its socket, so a response queued behind
/// undrained events waits -- a caller that awaits `send_direct` on a
/// session whose events it never drains waits until it drains them. And a
/// client that pauses reading also stops answering keepalive pings, so
/// one that leaves events undrained past the miss threshold is closed by
/// the server as wedged and loses its lease (`LOCAL-IPC.md`, A
/// 2026-09-30). The buffer is bounded at the granted `event_queue` plus
/// the one event its reader holds while it pauses.
pub struct IpcSession {
    session: LocalDataSession,
    connection: Connection,
    /// Present exactly when `events` was granted: the buffer, and the
    /// inbox holding the server's newest state and the wake.
    events: Option<(Mutex<mpsc::Receiver<SessionEvent>>, Arc<Inbox>)>,
}

impl std::fmt::Debug for IpcSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcSession")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl DataSessionPort for IpcSession {
    fn session(&self) -> &LocalDataSession {
        &self.session
    }

    async fn join(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.connection
            .call::<EmptyResult>(Request::ChannelJoin(ChannelParams { channel }))
            .await
            .map(|_| ())
    }

    async fn leave(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.connection
            .call::<EmptyResult>(Request::ChannelLeave(ChannelParams { channel }))
            .await
            .map(|_| ())
    }

    async fn broadcast(
        &self,
        channel: ChannelId,
        message: BroadcastMessageV1,
    ) -> Result<(), TransportError> {
        // `sent_at_ms` is diagnostic and not on the wire: the daemon
        // stamps its own.
        self.connection
            .call::<EmptyResult>(Request::BroadcastPublish(PublishParams {
                channel,
                message_id: message.message_id,
                payload: message.payload,
            }))
            .await
            .map(|_| ())
    }

    async fn send_direct(
        &self,
        destination: DirectDestination,
        message_id: MessageId,
        payload: Payload,
    ) -> Result<EndpointId, TransportError> {
        self.connection
            .call::<SendResult>(Request::DirectSend(SendParams {
                peer: destination.peer,
                endpoint: destination.endpoint,
                message_id,
                payload,
            }))
            .await
            .map(|result| result.resolved_endpoint)
    }

    async fn events(&self, max: usize) -> Result<Vec<SessionEvent>, TransportError> {
        // Never reaches the server: this takes from what it pushed. The
        // capability is judged here as the in-process binding judges it.
        //
        // Order: within one server pump the grouped order holds (session
        // notices, then direct, then broadcast, each oldest first); across
        // pumps batches are read as they arrive, so a notice pumped after
        // a direct message follows it. A consumer that needs one order
        // across a session uses the receipt times a direct message and a
        // broadcast carry; a notice carries none and is read as of its
        // arrival (`LOCAL-IPC.md` §Push events and overload, A 2026-09-30).
        //
        // The server's state, held apart as the newest only, comes first.
        let Some((buffer, inbox)) = &self.events else {
            return Err(TransportError::CapabilityDenied);
        };
        let mut buffer = buffer.lock().await;
        let mut taken = Vec::new();
        if max > 0
            && let Some(state) = inbox.take_state()
        {
            taken.push(state);
        }
        while taken.len() < max {
            match buffer.try_recv() {
                Ok(event) => taken.push(event),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    // What arrived before the end is still delivered; an
                    // empty buffer on an ended connection is the end.
                    if taken.is_empty() {
                        return Err(self.connection.gone());
                    }
                    break;
                }
            }
        }
        if taken.is_empty() && self.connection.has_ended() {
            return Err(self.connection.gone());
        }
        Ok(taken)
    }

    async fn ready(&self) -> Result<(), TransportError> {
        let Some((buffer, inbox)) = &self.events else {
            return Err(TransportError::CapabilityDenied);
        };
        loop {
            let woken = inbox.wake().notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            // Looked at, not taken: the buffer stays the server's bound.
            if inbox.holds_state() || !buffer.lock().await.is_empty() || self.connection.has_ended()
            {
                return Ok(());
            }
            woken.await;
        }
    }

    async fn query_endpoints(
        &self,
        peer: TransportIdentity,
    ) -> Result<EndpointDirectoryV1, TransportError> {
        self.connection
            .call::<DirectoryResult>(Request::EndpointsQuery(QueryParams { peer }))
            .await
            .map(EndpointDirectoryV1::from)
    }

    async fn close(self) -> Result<(), TransportError> {
        let Self {
            connection, events, ..
        } = self;
        // The buffer goes first, so a reader held by a full one is freed
        // to read on to the server's close.
        drop(events);
        connection.close().await
    }
}
