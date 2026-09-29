// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! One connection, from `hello` to its end (`LOCAL-IPC.md` §Message
//! classes, §Cancellation mapping and request concurrency, §Push events and
//! overload, §Disconnect/reconnect; plan §16 (5), (8)).
//!
//! After `hello` the client may send `request`, `cancel` and `pong`; any
//! other class, or a body that is not a frame, closes with
//! `ProtocolViolation`. An error that has a request id is a
//! `response{ok: false}` on a connection that stays.
//!
//! Requests: at most [`MAX_IN_FLIGHT`] handed to the port at once, at
//! most [`MAX_PENDING`] more waiting, and past that `Overloaded`. A
//! `cancel` for a waiting request answers `CancelledBeforeDispatch` and
//! drops it; for one already handed over, `CancellationRaced`, and its
//! outcome is discarded when it arrives -- one response per request.
//! Every request has a deadline (`deadline_ms`, else the profile's
//! default, clamped into 1..60 s); when it passes the answer is `Timeout`,
//! with the same two cases as a cancel (architect-cto's ruling, relay seq
//! 9621).
//!
//! Events: the session is drained only while the connection's event lane
//! has room, so a slow client leaves its messages in the binding's own
//! bounded queue, where overflow is the binding's rule (plan §16 (8)).
//!
//! The end, however it comes -- the client gone, a close, the server
//! stopping -- aborts what is in flight and closes the session: its lease
//! and its joins are released at once.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use interweave_ipc_protocol::{
    Admission, AuthorityDomain, Close, Event, Frame, IpcVersion, Refusal, Request, RequestFrame,
    RequestId, ResponseFrame, ServerState,
};
use interweave_local_client_api::{
    AdminBinding, AdminPort, DataCapability, DataSessionBinding, DataSessionPort,
};
use interweave_transport_api::TransportError;
use tokio::io::{AsyncRead, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::admission::Slot;
use crate::counters::Counters;
use crate::frames::{FrameReader, Lanes, ReadError, spawn_writer};
use crate::hello::{Established, ServerConfig, hello};
use crate::keepalive::{Action, Keepalive};
use crate::{MAX_IN_FLIGHT, MAX_PENDING, dispatch};

/// How often a data connection asks its session for events. The port's
/// `events()` is a drain with no wake-up, so this is the latency a
/// message waits at worst before it is written.
pub(crate) const EVENT_POLL: Duration = Duration::from_millis(20);

/// What every connection shares with the server.
pub(crate) struct Shared {
    pub(crate) config: ServerConfig,
    pub(crate) counters: Arc<Counters>,
    pub(crate) state: watch::Receiver<Option<ServerState>>,
}

enum Port<S, A> {
    Data(Arc<S>),
    Admin(Arc<A>),
}

/// Why the running phase ended.
enum End {
    /// The client went away, or its stream broke: nothing more to write.
    Gone,
    /// Close with this code.
    Close(TransportError),
    /// Close with `ProtocolViolation`, saying what was wrong.
    Violation(String),
}

/// Serve one admitted connection to its end. `slot` is released when
/// this returns.
pub(crate) async fn run<B>(
    stream: UnixStream,
    domain: AuthorityDomain,
    binding: B,
    shared: Arc<Shared>,
    stop: watch::Receiver<bool>,
    slot: Slot,
) where
    B: DataSessionBinding + AdminBinding + Send + Sync + 'static,
    B::Session: Send + Sync + 'static,
    B::Admin: Send + Sync + 'static,
{
    let (read, mut write) = stream.into_split();
    let mut reader = FrameReader::new(read);
    let (established, reply) = hello(&mut reader, domain, &binding, &shared.config).await;
    if let Some(bytes) = reply.and_then(|frame| frame.encode().ok()) {
        let _ = write.write_all(&bytes).await;
    }
    let Some(established) = established else {
        let _ = write.shutdown().await;
        drop(slot);
        return;
    };
    let (port, version, keepalive, lane) = match established {
        Established::Data {
            session,
            version,
            keepalive,
        } => {
            let lane = session.session().event_queue();
            (Port::Data(Arc::new(session)), version, keepalive, lane)
        }
        Established::Admin {
            port,
            version,
            keepalive,
        } => (Port::Admin(Arc::new(port)), version, keepalive, 1),
    };
    let (lanes, writer) = spawn_writer(write, lane);
    let policy = shared.config.keepalive;
    let mut connection = Connection {
        lanes,
        shared,
        port,
        version,
        in_flight: JoinSet::new(),
        flight: HashMap::new(),
        pending: VecDeque::new(),
        keepalive: keepalive.then(|| Keepalive::new(policy, Instant::now())),
        sequence: 0,
    };
    let end = connection.serve(&mut reader, stop).await;
    connection.end(end).await;
    let _ = writer.await;
    drop(slot);
}

struct Connection<S, A> {
    lanes: Lanes,
    shared: Arc<Shared>,
    port: Port<S, A>,
    version: IpcVersion,
    /// Handed to the port, answered when they finish.
    in_flight: JoinSet<(RequestId, ResponseFrame)>,
    /// Each request in flight: whether a cancel or its deadline already
    /// answered it, and that deadline.
    flight: HashMap<RequestId, InFlight>,
    /// Waiting for a slot in flight, each with its deadline.
    pending: VecDeque<(RequestId, Request, Instant)>,
    keepalive: Option<Keepalive>,
    /// The next event's sequence number, per connection.
    sequence: u64,
}

impl<S, A> Connection<S, A>
where
    S: DataSessionPort + Send + Sync + 'static,
    A: AdminPort + Send + Sync + 'static,
{
    async fn serve<R: AsyncRead + Unpin>(
        &mut self,
        reader: &mut FrameReader<R>,
        mut stop: watch::Receiver<bool>,
    ) -> End {
        let mut state = self.shared.state.clone();
        let pushes_state = matches!(self.port, Port::Data(_));
        let pumps_events = match &self.port {
            Port::Data(session) => session.session().holds(DataCapability::Events),
            Port::Admin(_) => false,
        };
        // On connect: the current view, if one is known.
        if pushes_state {
            let current = state.borrow_and_update().clone();
            if let Some(view) = current
                && self
                    .lanes
                    .control
                    .send(Frame::ServerState(view))
                    .await
                    .is_err()
            {
                return End::Gone;
            }
        }
        let mut state_alive = pushes_state;
        let mut poll = tokio::time::interval(EVENT_POLL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let wake = self
                .keepalive
                .as_ref()
                .map(|k| tokio::time::Instant::from_std(k.next_wake()));
            let due = self.next_deadline().map(tokio::time::Instant::from_std);
            tokio::select! {
                read = reader.next() => match read {
                    Ok(Some(body)) => {
                        if let Some(end) = self.frame(&body).await {
                            return end;
                        }
                    }
                    Ok(None) | Err(ReadError::Truncated | ReadError::Io) => return End::Gone,
                    Err(ReadError::Frame(error)) => return End::Violation(format!("{error:?}")),
                },
                Some(done) = self.in_flight.join_next(), if !self.in_flight.is_empty() => {
                    if let Ok((id, response)) = done {
                        // A request a cancel already answered gets no
                        // second response.
                        if self.flight.remove(&id).is_some_and(|held| !held.answered)
                            && self.lanes.control.send(Frame::Response(response)).await.is_err()
                        {
                            return End::Gone;
                        }
                    }
                    self.promote();
                }
                () = sleep_until(due), if due.is_some() => {
                    if let Some(end) = self.expire(Instant::now()).await {
                        return end;
                    }
                }
                () = sleep_until(wake), if wake.is_some() => {
                    if let Some(keepalive) = self.keepalive.as_mut() {
                        match keepalive.wake(Instant::now()) {
                            Action::Nothing => {}
                            Action::Probe(ping) => {
                                if self.lanes.control.send(Frame::Ping(ping)).await.is_err() {
                                    return End::Gone;
                                }
                            }
                            Action::Close => return End::Close(TransportError::Timeout),
                        }
                    }
                }
                _ = poll.tick(), if pumps_events && self.lanes.events.capacity() > 0 => {
                    if !self.pump().await {
                        return End::Gone;
                    }
                }
                changed = state.changed(), if state_alive => match changed {
                    Ok(()) => {
                        let current = state.borrow_and_update().clone();
                        if let Some(view) = current
                            && self.lanes.control.send(Frame::ServerState(view)).await.is_err()
                        {
                            return End::Gone;
                        }
                    }
                    Err(_) => state_alive = false,
                },
                _ = stop.changed() => return End::Close(TransportError::ShuttingDown),
            }
        }
    }

    /// Handle one frame from the client; `Some` ends the connection.
    async fn frame(&mut self, body: &str) -> Option<End> {
        match Frame::parse(body) {
            Ok(Frame::Request(request)) => self.request(request).await,
            Ok(Frame::Cancel(cancel)) => self.cancel(cancel.id).await,
            Ok(Frame::Pong(pong)) => {
                if let Some(keepalive) = self.keepalive.as_mut() {
                    keepalive.pong(&pong);
                }
                None
            }
            // `hello` again, a server-only class, or not a frame.
            Ok(_) | Err(_) => Some(End::Close(TransportError::ProtocolViolation)),
        }
    }

    async fn request(&mut self, frame: RequestFrame) -> Option<End> {
        let id = frame.id.clone();
        // Ids are unique per connection: a second one outstanding would
        // make its response and any cancel ambiguous.
        if self.flight.contains_key(&id) || self.pending.iter().any(|(p, _, _)| *p == id) {
            return self
                .respond(ResponseFrame::failure(id, TransportError::InvalidArgument))
                .await;
        }
        let (domain, granted_data, granted_admin) = match &self.port {
            Port::Data(session) => (
                AuthorityDomain::Data,
                session.session().capabilities().clone(),
                std::collections::BTreeSet::default(),
            ),
            Port::Admin(port) => (
                AuthorityDomain::Admin,
                std::collections::BTreeSet::default(),
                admin_grants(port.port()),
            ),
        };
        let admitted = frame.admit(&Admission {
            domain,
            version: self.version,
            granted_data: &granted_data,
            granted_admin: &granted_admin,
        });
        let request = match admitted {
            Ok(request) => request,
            Err(refusal) => {
                if refusal == Refusal::CrossDomain {
                    self.shared.counters.cross_domain_denied();
                }
                return self
                    .respond(ResponseFrame::failure(id, refusal.code()))
                    .await;
            }
        };
        let deadline = Instant::now() + command_deadline(frame.deadline_ms, &self.shared.config);
        if self.in_flight.len() < MAX_IN_FLIGHT {
            self.dispatch(id, request, deadline);
            None
        } else if self.pending.len() < MAX_PENDING {
            self.pending.push_back((id, request, deadline));
            None
        } else {
            self.respond(ResponseFrame::failure(id, TransportError::Overloaded))
                .await
        }
    }

    async fn cancel(&mut self, id: RequestId) -> Option<End> {
        if let Some(at) = self.pending.iter().position(|(p, _, _)| *p == id) {
            self.pending.remove(at);
            return self
                .respond(ResponseFrame::failure(
                    id,
                    TransportError::CancelledBeforeDispatch,
                ))
                .await;
        }
        match self.flight.get_mut(&id) {
            Some(held) if !held.answered => {
                held.answered = true;
                self.respond(ResponseFrame::failure(
                    id,
                    TransportError::CancellationRaced,
                ))
                .await
            }
            // Already answered, or not a request this connection holds:
            // cancel is advisory, and says nothing back.
            Some(_) | None => None,
        }
    }

    fn dispatch(&mut self, id: RequestId, request: Request, deadline: Instant) {
        self.flight.insert(
            id.clone(),
            InFlight {
                answered: false,
                deadline,
            },
        );
        match &self.port {
            Port::Data(session) => {
                let session = Arc::clone(session);
                self.in_flight.spawn(async move {
                    let response = dispatch::data(&*session, id.clone(), request).await;
                    (id, response)
                });
            }
            Port::Admin(port) => {
                let port = Arc::clone(port);
                let counters = Arc::clone(&self.shared.counters);
                let grace = self.shared.config.shutdown_grace;
                self.in_flight.spawn(async move {
                    let response =
                        dispatch::admin(&*port, &counters, grace, id.clone(), request).await;
                    (id, response)
                });
            }
        }
    }

    /// The earliest deadline of a request not yet answered.
    fn next_deadline(&self) -> Option<Instant> {
        let waiting = self.pending.iter().map(|(_, _, deadline)| *deadline);
        let handed = self
            .flight
            .values()
            .filter(|held| !held.answered)
            .map(|held| held.deadline);
        waiting.chain(handed).min()
    }

    /// Answer `Timeout` for every request whose deadline has passed: one
    /// still waiting is dropped, one handed over has its later outcome
    /// discarded (the port carries no deadline, so the server can only
    /// stop waiting -- which is all TRANSPORT.md promises).
    async fn expire(&mut self, now: Instant) -> Option<End> {
        let mut timed_out = Vec::new();
        self.pending.retain(|(id, _, deadline)| {
            let keep = *deadline > now;
            if !keep {
                timed_out.push(id.clone());
            }
            keep
        });
        for (id, held) in &mut self.flight {
            if !held.answered && held.deadline <= now {
                held.answered = true;
                timed_out.push(id.clone());
            }
        }
        for id in timed_out {
            if let Some(end) = self
                .respond(ResponseFrame::failure(id, TransportError::Timeout))
                .await
            {
                return Some(end);
            }
        }
        None
    }

    /// Move waiting requests into flight while there is room.
    fn promote(&mut self) {
        while self.in_flight.len() < MAX_IN_FLIGHT {
            let Some((id, request, deadline)) = self.pending.pop_front() else {
                return;
            };
            self.dispatch(id, request, deadline);
        }
    }

    async fn respond(&self, response: ResponseFrame) -> Option<End> {
        match self.lanes.control.send(Frame::Response(response)).await {
            Ok(()) => None,
            Err(_) => Some(End::Gone),
        }
    }

    /// Drain the session into the event lane; `false` when the writer is
    /// gone.
    async fn pump(&mut self) -> bool {
        let Port::Data(session) = &self.port else {
            return true;
        };
        let Ok(events) = session.events().await else {
            return true;
        };
        for event in events {
            let Ok(event) = Event::from_session(event) else {
                continue;
            };
            // Minors are additive: a type introduced above the negotiated
            // minor is not sent.
            if !event.event_type().available_at(self.version) {
                continue;
            }
            let frame = Frame::Event(event.into_frame(self.sequence));
            self.sequence = self.sequence.wrapping_add(1);
            if self.lanes.events.send(frame).await.is_err() {
                return false;
            }
        }
        true
    }

    /// Close if asked to, drop what is in flight, and close the session:
    /// its lease and joins are released now, not when the runtime notices.
    async fn end(mut self, end: End) {
        let close = match end {
            End::Gone => None,
            End::Close(code) => Some(Close::new(code)),
            End::Violation(detail) => {
                Some(Close::new(TransportError::ProtocolViolation).with_message(&detail))
            }
        };
        if let Some(close) = close {
            let _ = self.lanes.control.send(Frame::Close(close)).await;
        }
        self.in_flight.shutdown().await;
        drop(self.lanes);
        if let Port::Data(session) = self.port
            && let Ok(session) = Arc::try_unwrap(session)
        {
            let _ = session.close().await;
        }
    }
}

/// A request handed to the port.
struct InFlight {
    /// A cancel or the deadline answered it; its outcome is discarded.
    answered: bool,
    deadline: Instant,
}

/// The shortest and longest command deadline a request may ask for
/// (TRANSPORT.md §Direct: configurable 1..60 s). A value outside is
/// clamped, not refused.
pub(crate) const MIN_DEADLINE: Duration = Duration::from_secs(1);
/// See [`MIN_DEADLINE`].
pub(crate) const MAX_DEADLINE: Duration = Duration::from_secs(60);

/// A request's deadline: its own `deadline_ms`, else the profile's
/// command-deadline default, clamped into 1..60 s.
fn command_deadline(deadline_ms: Option<u64>, config: &ServerConfig) -> Duration {
    deadline_ms
        .map_or(config.command_deadline, Duration::from_millis)
        .clamp(MIN_DEADLINE, MAX_DEADLINE)
}

fn admin_grants(
    port: &interweave_local_client_api::LocalAdminPort,
) -> std::collections::BTreeSet<interweave_local_client_api::AdminCapability> {
    use interweave_local_client_api::AdminCapability as C;
    [C::Status, C::Endpoints, C::Shutdown]
        .into_iter()
        .filter(|c| port.holds(*c))
        .collect()
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
