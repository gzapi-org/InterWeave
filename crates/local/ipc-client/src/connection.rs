// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! One connection to the daemon: the hello, then a reader and a writer.
//!
//! The reader answers every frame the server sends: a response goes to
//! the call waiting on its id, an event into the session's bounded
//! buffer, a ping is echoed. The writer owns the socket's write half and
//! carries requests, cancels and pongs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use interweave_ipc_protocol::{
    Cancel, DecodedFrame, Frame, FrameError, HELLO_TIMEOUT, Hello, HelloResponse, IPC_MAX_MINOR,
    IpcVersion, Request, RequestId, ResponseFrame, decode_frame,
};
use interweave_local_client_api::{LocalSessionEvent, SessionEvent};
use interweave_transport_api::TransportError;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

/// Frames waiting for the writer. A request past the server's 64
/// outstanding is answered `Overloaded` by the server, so this only has
/// to hold what a caller sends before the writer takes it.
const OUTGOING: usize = 64;

/// How long `close` waits for the server to finish the session -- it
/// releases the lease before it closes the socket -- before giving up.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// What the writer is handed.
enum Outgoing {
    Frame(Frame),
    /// Shut the write half: the server reads end of stream, closes the
    /// session and then the socket.
    Finish,
}

/// What the reader and the callers share.
#[derive(Default)]
struct Shared {
    /// Calls waiting for their response, by id.
    pending: Mutex<HashMap<String, oneshot::Sender<ResponseFrame>>>,
    /// Why the connection ended, once it has.
    ended: Mutex<Option<TransportError>>,
    /// The recorded end is the writer's own failure, which a server's
    /// `close` read afterwards replaces with the code it named (#224
    /// review B F1). Read and written under `ended`'s lock.
    provisional: AtomicBool,
    /// Set by `close` before it asks the server to end.
    closing: AtomicBool,
    /// The end was not the answer to `close`: anything but a clean end of
    /// stream read after `closing` was set. The server answers a client's
    /// Finish with end of stream and never a `close` frame, so a `close`
    /// frame, a protocol error or a read error is always a server's own
    /// end. A bare end of stream is ambiguous: a server that gave up on a
    /// client that stopped reading (`Stalled`, a writer that timed out, or
    /// a `close` that did not fit its full lane) also ends with one, and
    /// if that lands after `close` asked, it reads as the answer.
    uninvited: AtomicBool,
}

impl Shared {
    fn ended(&self) -> Option<TransportError> {
        *self.ended.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register a call, unless the connection has already ended. Checked
    /// under the lock `end` drains under, for the window between the
    /// reader's end and the writer's exit: a call registered after `end`
    /// drained would wait on an answer nothing will send. Outside that
    /// window the writer's exit fails the send first (the scripted
    /// `a_call_on_an_ended_connection_is_refused_not_left_waiting`); the
    /// window itself is too narrow for a test to reach on purpose.
    fn register(
        &self,
        id: &RequestId,
        answer: oneshot::Sender<ResponseFrame>,
    ) -> Result<(), TransportError> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(code) = self.ended() {
            return Err(code);
        }
        pending.insert(id.as_str().to_owned(), answer);
        Ok(())
    }

    fn take(&self, id: &str) -> Option<oneshot::Sender<ResponseFrame>> {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id)
    }

    /// The connection is over: every waiting call is dropped, and reads
    /// the code. The first end recorded stands, except that a writer's
    /// failure is provisional: the server's own `close` frame, read after
    /// it, replaces its code, so a caller is told why the SERVER ended the
    /// connection rather than that a write failed against the socket it
    /// was closing (`a_failed_write_keeps_the_servers_close_code`).
    fn end(&self, code: TransportError, clean: bool, by: EndedBy) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let mut ended = self.ended.lock().unwrap_or_else(PoisonError::into_inner);
        if ended.is_none() {
            if !(clean && self.closing.load(Ordering::SeqCst)) {
                self.uninvited.store(true, Ordering::SeqCst);
            }
            *ended = Some(code);
            self.provisional
                .store(by == EndedBy::Writer, Ordering::SeqCst);
        } else if by == EndedBy::ServerClose && self.provisional.swap(false, Ordering::SeqCst) {
            *ended = Some(code);
        }
        pending.clear();
    }
}

/// What a data connection holding `events` keeps beside its event
/// buffer: the server's newest state, and the wake its session's `ready`
/// waits on.
#[derive(Default)]
pub(crate) struct Inbox {
    /// The newest `server_state`, replaced, never queued: the server
    /// sends at most one pending, and a session reading late reads the
    /// present (`the_newest_server_state_is_held_once`).
    state: Mutex<Option<SessionEvent>>,
    /// Woken by each event buffered, each state held and the end. A
    /// permit is stored when nobody waits, so a wake between a session's
    /// look and its wait is not lost.
    wake: Notify,
}

impl Inbox {
    /// Wake EVERY session task waiting in `ready` -- `ready` takes
    /// `&self`, so two may wait on one session, and the end of a
    /// connection must end both -- and leave a permit for the next
    /// (`every_concurrent_ready_ends_with_the_connection`).
    fn wake_all(&self) {
        self.wake.notify_waiters();
        self.wake.notify_one();
    }

    fn hold(&self, state: SessionEvent) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = Some(state);
        self.wake_all();
    }

    /// Take the held state, if any.
    pub(crate) fn take_state(&self) -> Option<SessionEvent> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Whether a state is held.
    pub(crate) fn holds_state(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// What `ready` waits on: registered BEFORE it looks, so a wake that
    /// lands between the look and the wait is not missed.
    pub(crate) fn wake(&self) -> &Notify {
        &self.wake
    }
}

/// A connection whose hello was answered.
pub(crate) struct Connection {
    out: mpsc::Sender<Outgoing>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
}

/// What `open` returns: the connection, the server's answer, and -- for a
/// data connection holding `events` -- the receiving end of its event
/// buffer and its inbox.
pub(crate) struct Opened {
    pub(crate) connection: Connection,
    pub(crate) response: HelloResponse,
    pub(crate) events: Option<(mpsc::Receiver<SessionEvent>, Arc<Inbox>)>,
}

/// Connect to `socket`, send `hello` and read its answer. A data
/// connection's event buffer holds `event_queue(&response)` events.
///
/// # Errors
/// `BackendUnavailable` when the socket cannot be reached or closes
/// before answering; the server's `close` code when it refuses the hello;
/// `ProtocolViolation` for any other first frame, and for an answer
/// selecting a version the server could not have selected.
pub(crate) async fn open(
    socket: &Path,
    hello: Hello,
    event_queue: fn(&HelloResponse) -> Option<usize>,
) -> Result<Opened, TransportError> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|_| TransportError::BackendUnavailable)?;
    let (read, mut write) = stream.into_split();
    let offered = hello.ipc_version;
    let hello = Frame::Hello(hello)
        .encode()
        .map_err(|_| TransportError::InvalidArgument)?;
    write
        .write_all(&hello)
        .await
        .map_err(|_| TransportError::BackendUnavailable)?;
    let mut reader = Reader::new(read);
    // The server's own hello deadline bounds its answer too.
    let first = tokio::time::timeout(HELLO_TIMEOUT * 2, reader.next())
        .await
        .map_err(|_| TransportError::Timeout)??;
    let response = match first {
        Some(Frame::HelloResponse(response)) => response,
        Some(Frame::Close(close)) => return Err(close.code),
        Some(_) => return Err(TransportError::ProtocolViolation),
        None => return Err(TransportError::BackendUnavailable),
    };
    if !selectable(offered, response.ipc_version) {
        return Err(TransportError::ProtocolViolation);
    }
    let (out, out_rx) = mpsc::channel(OUTGOING);
    // Latest wins: the server holds one nonce outstanding at a time, so
    // an echo for an older ping still unwritten is replaced, not queued
    // ahead of the current one.
    let (pong, pong_rx) = watch::channel::<Option<Frame>>(None);
    let shared = Arc::new(Shared::default());
    let (events_tx, events) = match event_queue(&response) {
        Some(bound) => {
            let (tx, rx) = mpsc::channel(bound.max(1));
            let inbox = Arc::new(Inbox::default());
            (Some((tx, Arc::clone(&inbox))), Some((rx, inbox)))
        }
        None => (None, None),
    };
    // The reader's end is the writer's: a connection the client stopped
    // reading -- the server closed it, or broke the protocol -- is shut on
    // this side too, so the server sees it end and releases the session.
    let (ended, ended_rx) = watch::channel(false);
    let inbox = events_tx.as_ref().map(|(_, inbox)| Arc::clone(inbox));
    let writer = tokio::spawn(write_loop(
        write,
        out_rx,
        pong_rx,
        ended_rx,
        (Arc::clone(&shared), inbox),
    ));
    let reader = tokio::spawn(read_loop(
        reader,
        response.ipc_version,
        Arc::clone(&shared),
        pong,
        events_tx,
        ended,
    ));
    Ok(Opened {
        connection: Connection {
            out,
            shared,
            next_id: AtomicU64::new(0),
            reader: Some(reader),
            writer: Some(writer),
        },
        response,
        events,
    })
}

/// Whether a server answering a hello that offered `offered` could have
/// selected `selected` (LOCAL-IPC.md §Version negotiation):
/// `min(client minor, server minor)`, never above the client's minor nor
/// above what this build speaks. Every later method, event and feature is
/// gated on the selected minor, and the admin port remembers it for the
/// next hello, so an answer outside that is the server's violation rather
/// than a version to adopt. The major is not compared here: the
/// `hello_response` parser refuses any but [`IPC_MAJOR`](interweave_ipc_protocol::IPC_MAJOR),
/// which is the only one offered.
fn selectable(offered: IpcVersion, selected: IpcVersion) -> bool {
    selected.minor <= offered.minor && selected.minor <= IPC_MAX_MINOR
}

impl Connection {
    /// Why the connection ended, or `BackendUnavailable` if it ended
    /// without saying.
    pub(crate) fn gone(&self) -> TransportError {
        self.shared
            .ended()
            .unwrap_or(TransportError::BackendUnavailable)
    }

    /// Whether the connection has ended.
    pub(crate) fn has_ended(&self) -> bool {
        self.shared.ended().is_some()
    }

    /// Send `request` and read its answer as `T`. Dropped before the
    /// answer, the call is cancelled: the server is told, and the answer,
    /// if one still comes, is discarded.
    ///
    /// # Errors
    /// The response's code; `ProtocolViolation` for a result not of `T`'s
    /// shape; the connection's end code once it has ended.
    pub(crate) async fn call<T: DeserializeOwned>(
        &self,
        request: Request,
    ) -> Result<T, TransportError> {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = RequestId::new(format!("r{n}"))?;
        let (answer, response) = oneshot::channel();
        self.shared.register(&id, answer)?;
        let mut guard = CancelOnDrop {
            id: Some(id.clone()),
            out: self.out.clone(),
            shared: Arc::clone(&self.shared),
        };
        self.out
            .send(Outgoing::Frame(Frame::Request(
                request.into_frame(id, None),
            )))
            .await
            .map_err(|_| self.gone())?;
        let response = response.await.map_err(|_| self.gone())?;
        guard.id = None;
        response.outcome()
    }

    /// End the connection the way a client leaving does: shut the write
    /// half and wait for the server to close its side, which it does only
    /// after the session is closed and its lease released.
    ///
    /// # Errors
    /// The connection's end code when it had already ended -- there is no
    /// server left to release anything -- and `Timeout` when the server
    /// did not close within [`CLOSE_WAIT`].
    pub(crate) async fn close(mut self) -> Result<(), TransportError> {
        self.shared.closing.store(true, Ordering::SeqCst);
        if self.shared.uninvited.load(Ordering::SeqCst) {
            return Err(self.gone());
        }
        let _ = self.out.send(Outgoing::Finish).await;
        let Some(reader) = self.reader.as_mut() else {
            return Ok(());
        };
        match tokio::time::timeout(CLOSE_WAIT, reader).await {
            Ok(_) if self.shared.uninvited.load(Ordering::SeqCst) => Err(self.gone()),
            Ok(_) => Ok(()),
            Err(_) => Err(TransportError::Timeout),
        }
    }
}

impl Drop for Connection {
    /// A connection dropped without `close` ends at once: both halves go,
    /// the server reads end of stream and releases what the session held.
    fn drop(&mut self) {
        for task in [self.reader.take(), self.writer.take()]
            .into_iter()
            .flatten()
        {
            task.abort();
        }
    }
}

/// Cancels its call if dropped before the answer arrived.
struct CancelOnDrop {
    id: Option<RequestId>,
    out: mpsc::Sender<Outgoing>,
    shared: Arc<Shared>,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        // Still waiting: tell the server, advisory as LOCAL-IPC's cancel
        // is. A full writer queue skips it; the answer is discarded here
        // either way, because the id is no longer pending.
        if self.shared.take(id.as_str()).is_some() {
            let _ = self
                .out
                .try_send(Outgoing::Frame(Frame::Cancel(Cancel::new(id))));
        }
    }
}

/// Which half saw the end, and how: what decides whether a later end may
/// replace the recorded code ([`Shared::end`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndedBy {
    /// The writer failed on its own.
    Writer,
    /// The reader read the server's `close` frame.
    ServerClose,
    /// The reader saw any other end.
    Reader,
}

/// The connection is over, for either half: the end recorded, every
/// waiting call answered with it, and a session waiting in `ready` woken
/// to read it from `events`.
fn finish(shared: &Shared, inbox: Option<&Inbox>, code: TransportError, clean: bool, by: EndedBy) {
    shared.end(code, clean, by);
    if let Some(inbox) = inbox {
        inbox.wake_all();
    }
}

/// The writer. One that stops for its own reason -- a failed write, a
/// frame past the ceiling -- ends the connection BEFORE it drops its
/// queue, so a call that fails on the dropped queue finds the end already
/// recorded and `events` already refusing: the reader may never see the
/// end, a server that stopped reading and kept writing being one way
/// (`a_failed_write_ends_the_session_before_the_reader_sees_it`).
async fn write_loop(
    mut write: OwnedWriteHalf,
    mut out: mpsc::Receiver<Outgoing>,
    mut pong: watch::Receiver<Option<Frame>>,
    mut ended: watch::Receiver<bool>,
    (shared, inbox): (Arc<Shared>, Option<Arc<Inbox>>),
) {
    loop {
        // An echo goes ahead of whatever requests are queued: the server
        // counts a late pong as a missed one.
        let next = tokio::select! {
            biased;
            _ = ended.wait_for(|ended| *ended) => break,
            Ok(()) = pong.changed() => match pong.borrow_and_update().clone() {
                Some(frame) => Outgoing::Frame(frame),
                None => continue,
            },
            next = out.recv() => match next {
                Some(next) => next,
                None => break,
            },
        };
        let Outgoing::Frame(frame) = next else {
            break;
        };
        let Ok(bytes) = frame.encode() else {
            finish(
                &shared,
                inbox.as_deref(),
                TransportError::PayloadTooLarge,
                false,
                EndedBy::Writer,
            );
            break;
        };
        if write.write_all(&bytes).await.is_err() {
            finish(
                &shared,
                inbox.as_deref(),
                TransportError::BackendUnavailable,
                false,
                EndedBy::Writer,
            );
            break;
        }
    }
    let _ = write.shutdown().await;
}

async fn read_loop(
    mut reader: Reader,
    version: IpcVersion,
    shared: Arc<Shared>,
    echoes: watch::Sender<Option<Frame>>,
    events: Option<(mpsc::Sender<SessionEvent>, Arc<Inbox>)>,
    ended: watch::Sender<bool>,
) {
    let (code, clean, by) = loop {
        match reader.next().await {
            Ok(Some(Frame::Response(response))) => {
                // An id no call waits for was cancelled: discarded.
                if let Some(answer) = shared.take(response.id.as_str()) {
                    let _ = answer.send(response);
                }
            }
            Ok(Some(Frame::Event(frame))) => {
                // An admin connection, or a session not granted `events`,
                // is sent none: one it could never drain would wedge the
                // reader once the buffer filled.
                let Some((events, inbox)) = &events else {
                    break (TransportError::ProtocolViolation, false, EndedBy::Reader);
                };
                // A type above the selected minor is the server's
                // violation, as an unknown one is.
                let event = match frame.event(version) {
                    Ok(event) => event.into_session(),
                    Err(code) => break (code, false, EndedBy::Reader),
                };
                // THE BOUND: a full buffer holds the reader here, with this
                // one event in hand, so the socket is not read until the
                // session drains it (LOCAL-IPC.md, A 2026-09-30) -- the
                // client holds the granted bound plus that one. A session
                // already closed takes nothing, and reading goes on to the
                // end.
                if events.send(event).await.is_ok() {
                    inbox.wake_all();
                }
            }
            Ok(Some(Frame::Ping(ping))) => {
                echoes.send_replace(Some(Frame::Pong(ping.echo())));
            }
            // Held for a session reading events, the newest only; a
            // connection without them has no use for it.
            Ok(Some(Frame::ServerState(state))) => {
                if let Some((_, inbox)) = &events {
                    inbox.hold(SessionEvent::Local(LocalSessionEvent::ServerState {
                        health: state.health,
                        connectivity: state.connectivity,
                    }));
                }
            }
            Ok(Some(Frame::Close(close))) => break (close.code, false, EndedBy::ServerClose),
            Ok(Some(_)) => break (TransportError::ProtocolViolation, false, EndedBy::Reader),
            // A clean end of stream: the answer to a Finish, if one was sent.
            Ok(None) => break (TransportError::BackendUnavailable, true, EndedBy::Reader),
            Err(code) => break (code, false, EndedBy::Reader),
        }
    };
    finish(
        &shared,
        events.as_ref().map(|(_, inbox)| &**inbox),
        code,
        clean,
        by,
    );
    let _ = ended.send(true);
}

/// Frames off the read half.
struct Reader {
    inner: OwnedReadHalf,
    buf: Vec<u8>,
}

impl Reader {
    const fn new(inner: OwnedReadHalf) -> Self {
        Self {
            inner,
            buf: Vec::new(),
        }
    }

    /// The next frame, or `None` at a clean end of stream.
    async fn next(&mut self) -> Result<Option<Frame>, TransportError> {
        loop {
            match decode_frame(&self.buf) {
                Ok(DecodedFrame { body, consumed }) => {
                    self.buf.drain(..consumed);
                    return Frame::parse(&body).map(Some);
                }
                Err(FrameError::Incomplete { .. }) => {}
                Err(_) => return Err(TransportError::ProtocolViolation),
            }
            // `Incomplete` is returned only inside the ceiling, so what is
            // read here is bounded by it.
            let mut chunk = [0_u8; 8192];
            let read = self
                .inner
                .read(&mut chunk)
                .await
                .map_err(|_| TransportError::BackendUnavailable)?;
            if read == 0 {
                return if self.buf.is_empty() {
                    Ok(None)
                } else {
                    Err(TransportError::BackendUnavailable)
                };
            }
            self.buf.extend_from_slice(&chunk[..read]);
        }
    }
}
