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
    Admission, AuthorityDomain, Close, Event, EventType, Frame, IpcVersion, Refusal, Request,
    RequestFrame, RequestId, ResponseFrame, ServerState,
};
use interweave_local_client_api::{
    AdminBinding, AdminPort, DataCapability, DataSessionBinding, DataSessionPort,
};
use interweave_transport_api::TransportError;
use tokio::io::{AsyncRead, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use crate::admission::Slot;
use crate::counters::Counters;
use crate::frames::{FrameReader, Lanes, ReadError, spawn_writer};
use crate::hello::{Established, ServerConfig, hello};
use crate::keepalive::{Action, Keepalive};
use crate::{MAX_IN_FLIGHT, MAX_PENDING, dispatch};

/// How often a data connection asks its session for events. The pump
/// drains `events()` on this timer and does not wait in the port's
/// `ready()`, so this is the latency a message waits at worst before it
/// is written.
pub(crate) const EVENT_POLL: Duration = Duration::from_millis(20);

/// How long a connection's writer has to flush its last frames after the
/// connection ends, before it is aborted.
pub(crate) const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// The outbox's backstop: everything that can be owed while reading is
/// paused -- an answer for each request in flight or waiting, a probe per
/// tolerated miss, a close -- with room to spare. Past it the invariant
/// that bounds the outbox is broken, and the connection is closed rather
/// than grown.
const OUTBOX_LIMIT: usize = MAX_IN_FLIGHT + MAX_PENDING + 16;

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
    /// The client stopped reading: its writer's lane is full, so nothing
    /// more can be queued for it, a `close` included.
    Stalled,
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
    // A data connection's writer reads the server's view itself, so at
    // most one `server_state` is ever pending for it -- one holding
    // `events`, the only kind whose session can read it, as `ServerState`
    // through `events` (`a_data_client_without_events_is_sent_no_view`).
    let state = match &port {
        Port::Data(session) if session.session().holds(DataCapability::Events) => {
            Some(shared.state.clone())
        }
        _ => None,
    };
    let (lanes, mut writer) = spawn_writer(write, lane, state, shared.config.write_stall);
    let policy = shared.config.keepalive;
    let mut connection = Connection {
        lanes,
        shared,
        port,
        version,
        in_flight: JoinSet::new(),
        outbox: VecDeque::new(),
        tasks: HashMap::new(),
        flight: HashMap::new(),
        pending: VecDeque::new(),
        keepalive: keepalive.then(|| Keepalive::new(policy, Instant::now())),
        sequence: 0,
    };
    let end = connection.serve(&mut reader, stop).await;
    connection.end(end).await;
    // The writer may be blocked on a client that no longer reads: it gets
    // CLOSE_GRACE to flush the close, and is aborted after, so the slot is
    // always released (#151 review, F1).
    if tokio::time::timeout(CLOSE_GRACE, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
    drop(slot);
}

struct Connection<S, A> {
    lanes: Lanes,
    shared: Arc<Shared>,
    port: Port<S, A>,
    version: IpcVersion,
    /// Handed to the port, answered when they finish.
    in_flight: JoinSet<ResponseFrame>,
    /// Frames owed to the client that the full control lane could not
    /// take yet, in order. Bounded by construction: while it holds
    /// anything the client's frames are not read, so only what was
    /// already owed can join it -- an answer per request in flight or
    /// waiting, a probe, a close ([`OUTBOX_LIMIT`] backs that up).
    outbox: VecDeque<Frame>,
    /// Which request each task in flight answers: a task that panics
    /// reports only its id, and its request must still be answered.
    tasks: HashMap<tokio::task::Id, RequestId>,
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
    /// The running phase. NOTHING IN THIS LOOP WAITS ON THE CLIENT (#151
    /// review, F1-F2): a frame the full control lane cannot take waits in
    /// the outbox, and while anything waits there the client's own frames
    /// are not read -- backpressure, as a socket applies it -- so a client
    /// that pipelines is slowed, never closed. One that has stopped
    /// reading stalls the writer, which gives up after `write_stall` and
    /// takes the lanes with it; keepalive and `stop` are polled
    /// throughout.
    ///
    /// No arm is guarded by a lane's room: a guard is read only when the
    /// loop wakes, so one waiting for a lane to drain would stay shut on a
    /// connection with no timer of its own -- an admin one, or a data one
    /// without `events` or keepalive (#151 re-review, 1).
    async fn serve<R: AsyncRead + Unpin>(
        &mut self,
        reader: &mut FrameReader<R>,
        mut stop: watch::Receiver<bool>,
    ) -> End {
        let pumps_events = match &self.port {
            Port::Data(session) => session.session().holds(DataCapability::Events),
            Port::Admin(_) => false,
        };
        let mut poll = tokio::time::interval(EVENT_POLL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let wake = self
                .keepalive
                .as_ref()
                .map(|k| tokio::time::Instant::from_std(k.next_wake()));
            let due = self.next_deadline().map(tokio::time::Instant::from_std);
            // Not the lane's room: a full lane moves the next frame into
            // the outbox, which is what pauses reading and arms its wake.
            let reading = self.outbox.is_empty();
            let outcome = tokio::select! {
                permit = self.lanes.control.clone().reserve_owned(), if !self.outbox.is_empty() => match permit {
                    Ok(permit) => {
                        if let Some(frame) = self.outbox.pop_front() {
                            let _ = permit.send(frame);
                        }
                        None
                    }
                    Err(_) => Some(End::Gone),
                },
                read = reader.next(), if reading => match read {
                    Ok(Some(body)) => self.frame(&body),
                    Ok(None) | Err(ReadError::Truncated | ReadError::Io) => Some(End::Gone),
                    Err(ReadError::Frame(error)) => Some(End::Violation(format!("{error:?}"))),
                },
                Some(done) = self.in_flight.join_next_with_id(), if !self.in_flight.is_empty() => {
                    let answered = self.finished(done);
                    self.promote();
                    answered
                }
                () = sleep_until(due), if due.is_some() => self.expire(Instant::now()),
                () = sleep_until(wake), if wake.is_some() => match self.keepalive.as_mut().map(|k| k.wake(Instant::now())) {
                    Some(Action::Probe(ping)) => self.push(Frame::Ping(ping)),
                    Some(Action::Close) => Some(End::Close(TransportError::Timeout)),
                    Some(Action::Nothing) | None => None,
                },
                // Unguarded by the lane's room: a select guard is read only
                // when the loop wakes, so one false while the lane was full
                // would stay false after the writer freed it, and events
                // would wait for the next request or keepalive. The pump
                // reads the room on every tick instead.
                _ = poll.tick(), if pumps_events => self.pump().await,
                // The writer gave up on a client that stopped reading;
                // seen here even when nothing else would wake the loop.
                () = self.lanes.control.closed() => Some(End::Gone),
                _ = stop.changed() => Some(End::Close(TransportError::ShuttingDown)),
            };
            if let Some(end) = outcome {
                return end;
            }
        }
    }

    /// Hand `frame` to the writer without waiting: into the lane if it
    /// has room and nothing is ahead of it, else into the outbox.
    fn push(&mut self, frame: Frame) -> Option<End> {
        if self.outbox.is_empty() {
            match self.lanes.control.try_send(frame) {
                Ok(()) => return None,
                Err(mpsc::error::TrySendError::Closed(_)) => return Some(End::Gone),
                Err(mpsc::error::TrySendError::Full(frame)) => self.outbox.push_back(frame),
            }
        } else {
            self.outbox.push_back(frame);
        }
        (self.outbox.len() > OUTBOX_LIMIT).then_some(End::Stalled)
    }

    fn respond(&mut self, response: ResponseFrame) -> Option<End> {
        self.push(Frame::Response(response))
    }

    /// A task in flight finished. Its answer goes out unless a cancel or
    /// the deadline already answered the request; a task that panicked is
    /// answered `Internal`, and its bookkeeping released either way.
    fn finished(
        &mut self,
        done: Result<(tokio::task::Id, ResponseFrame), tokio::task::JoinError>,
    ) -> Option<End> {
        let (task, response) = match done {
            Ok((task, response)) => (task, Some(response)),
            Err(error) => (error.id(), None),
        };
        let id = self.tasks.remove(&task)?;
        let held = self.flight.remove(&id)?;
        if held.answered {
            return None;
        }
        self.respond(
            response.unwrap_or_else(|| ResponseFrame::failure(id, TransportError::Internal)),
        )
    }

    /// Handle one frame from the client; `Some` ends the connection.
    fn frame(&mut self, body: &str) -> Option<End> {
        match Frame::parse(body) {
            Ok(Frame::Request(request)) => self.request(&request),
            Ok(Frame::Cancel(cancel)) => self.cancel(cancel.id),
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

    fn request(&mut self, frame: &RequestFrame) -> Option<End> {
        let id = frame.id.clone();
        // Ids are unique per connection (LOCAL-IPC, architect-cto's ruling
        // of relay seq 9709): one reused while the first still awaits its
        // response is a protocol violation, and the connection closes --
        // no response bearing that id could be told from the first's, so
        // answering it is not an option.
        let awaiting = self.flight.get(&id).is_some_and(|flight| !flight.answered)
            || self.pending.iter().any(|(p, _, _)| *p == id);
        if awaiting {
            return Some(End::Violation(
                "a request id reused while outstanding".to_owned(),
            ));
        }
        // One whose response the client already has -- a cancel or its
        // deadline answered it -- is outstanding only to this server,
        // whose task has not finished: refused, and the refusal can only
        // be read as the new request's answer.
        if self.flight.contains_key(&id) {
            return self.respond(ResponseFrame::failure(id, TransportError::InvalidArgument));
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
                return self.respond(ResponseFrame::failure(id, refusal.code()));
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
        }
    }

    fn cancel(&mut self, id: RequestId) -> Option<End> {
        if let Some(at) = self.pending.iter().position(|(p, _, _)| *p == id) {
            self.pending.remove(at);
            return self.respond(ResponseFrame::failure(
                id,
                TransportError::CancelledBeforeDispatch,
            ));
        }
        match self.flight.get_mut(&id) {
            Some(held) if !held.answered => {
                held.answered = true;
                self.respond(ResponseFrame::failure(
                    id,
                    TransportError::CancellationRaced,
                ))
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
        let task = match &self.port {
            Port::Data(session) => {
                let session = Arc::clone(session);
                let answer = id.clone();
                self.in_flight
                    .spawn(async move { dispatch::data(&*session, answer, request).await })
            }
            Port::Admin(port) => {
                let port = Arc::clone(port);
                let counters = Arc::clone(&self.shared.counters);
                let grace = self.shared.config.shutdown_grace;
                let answer = id.clone();
                self.in_flight.spawn(async move {
                    dispatch::admin(&*port, &counters, grace, answer, request).await
                })
            }
        };
        self.tasks.insert(task.id(), id);
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
    fn expire(&mut self, now: Instant) -> Option<End> {
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
            if let Some(end) = self.respond(ResponseFrame::failure(id, TransportError::Timeout)) {
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

    /// Drain the session into the event lane; `Some` ends the connection.
    async fn pump(&mut self) -> Option<End> {
        let Port::Data(session) = &self.port else {
            return None;
        };
        // The lane's free slots are reserved before the session is asked,
        // and the session is asked for no more than that: what does not
        // fit stays queued in the binding under its bound, and nothing
        // taken is ever dropped here (#151 review, F3; relay seq 9709).
        let room = self.lanes.events.capacity();
        if room == 0 {
            return None;
        }
        let mut permits = match self.lanes.events.try_reserve_many(room) {
            Ok(permits) => permits,
            Err(mpsc::error::TrySendError::Closed(())) => return Some(End::Gone),
            Err(mpsc::error::TrySendError::Full(())) => return None,
        };
        let reserved = permits.len();
        let Ok(events) = session.events(reserved).await else {
            return None;
        };
        // The port returns at most what it was asked for; a binding that
        // broke that would have its excess dropped below, so the tests
        // catch it here.
        debug_assert!(events.len() <= reserved, "the port returned more than max");
        for event in events {
            // Minors are additive: a type introduced above the negotiated
            // minor is not this client's to see, so it is skipped without
            // a number -- a gap would read as a loss (#151 re-review, 2).
            // Judged BEFORE the shape, so a refused one of such a type
            // leaves no gap either (#184 review F4).
            if EventType::of_session(&event).is_some_and(|kind| !kind.available_at(self.version)) {
                continue;
            }
            // One the protocol refuses takes a number, so it leaves a gap
            // the client can see rather than vanishing (#151 review, F6).
            let event = match Event::from_session(event) {
                Ok(Some(event)) => event,
                // The runtime's state is not numbered: it is this
                // connection's `server_state` frame, sent from the
                // server's own view, never an event.
                Ok(None) => continue,
                Err(_) => {
                    self.sequence = self.sequence.wrapping_add(1);
                    continue;
                }
            };
            let sequence = self.sequence;
            self.sequence = self.sequence.wrapping_add(1);
            let Some(permit) = permits.next() else {
                break;
            };
            permit.send(Frame::Event(event.into_frame(sequence)));
        }
        None
    }

    /// End the connection: drop what is in flight, close the session --
    /// releasing its lease and joins BEFORE the client is told, so a
    /// client that reconnects on the `close` finds the lease free -- then
    /// queue the `close` if the lane has room for it.
    async fn end(mut self, end: End) {
        self.in_flight.shutdown().await;
        if let Port::Data(session) = self.port
            && let Ok(session) = Arc::try_unwrap(session)
        {
            let _ = session.close().await;
        }
        let close = match end {
            End::Gone | End::Stalled => None,
            End::Close(code) => Some(Close::new(code)),
            End::Violation(detail) => {
                Some(Close::new(TransportError::ProtocolViolation).with_message(&detail))
            }
        };
        // What is owed goes first, then the close, as far as the lane
        // takes them now: the writer is not waited for here.
        for frame in self.outbox.drain(..).chain(close.map(Frame::Close)) {
            if self.lanes.control.try_send(frame).is_err() {
                break;
            }
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
/// (TRANSPORT.md §send(destination, payload, options?): configurable
/// 1..60 s, adopted for every IPC request by LOCAL-IPC.md). A value outside is
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
