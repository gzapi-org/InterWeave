// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! One connection to the daemon: the hello, then a reader and a writer.
//!
//! The reader answers every frame the server sends: a response goes to
//! the call waiting on its id, an event into the session's bounded
//! buffer, a ping is echoed. The writer owns the socket's write half and
//! carries requests, cancels and pongs.

use std::collections::HashMap;
use std::io::Read as _;
use std::os::fd::AsFd as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use interweave_ipc_protocol::{
    CLIENT_SILENCE_TIMEOUT, Cancel, DecodedFrame, Frame, FrameError, HELLO_TIMEOUT, Hello,
    HelloResponse, IPC_MAX_MINOR, IpcVersion, Request, RequestFrame, RequestId, ResponseFrame,
    decode_frame,
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

/// The most the reader takes in once the writer has failed, before it
/// ends: "what has already arrived" read as a bound, since how much the
/// socket holds is not asked of the kernel (that is an `ioctl`, and this
/// crate forbids `unsafe`). A conforming daemon has at most a socket
/// buffer queued when it closes -- 208 KiB at Linux's default -- so its
/// `close` is inside the bound; a daemon that shut its read half and
/// keeps writing is cut off here rather than holding every waiting call
/// (`a_server_that_keeps_writing_after_a_failed_write_is_cut_off`).
const DRAIN_BUDGET: usize = 1024 * 1024;

/// How long `close` waits for the server to finish the session -- it
/// releases the lease before it closes the socket -- before giving up.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// What the writer is handed: frames already encoded, each by the code
/// that made it, so a frame that cannot be encoded fails where it was
/// made and never reaches the connection's own end.
enum Outgoing {
    Bytes(Vec<u8>),
    /// Shut the write half: the server reads end of stream, closes the
    /// session and then the socket.
    Finish,
}

/// What a waiting call is handed: its response, or the connection's end.
type Answer = Result<ResponseFrame, TransportError>;

/// What the reader and the callers share.
#[derive(Default)]
struct Shared {
    /// Calls waiting for their response, by id: answered with it, or with
    /// the code the connection ended with, handed over as it ends.
    pending: Mutex<HashMap<String, oneshot::Sender<Answer>>>,
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
    /// Why the connection ended, once the end is final. A writer's
    /// provisional end is not shown to any caller: the reader's end
    /// always follows it, answers every call registered meanwhile and
    /// wakes `ready` again, so a call, `events` and `ready` all read the
    /// one code the session ends with (`LOCAL-IPC.md` §Close;
    /// `a_provisional_end_is_shown_to_nobody`).
    fn ended(&self) -> Option<TransportError> {
        let ended = self.ended.lock().unwrap_or_else(PoisonError::into_inner);
        if self.provisional.load(Ordering::SeqCst) {
            return None;
        }
        *ended
    }

    /// Register a call, unless the connection has already ended for good.
    /// Checked under the lock `end` drains under: a call registered after
    /// the reader's `end` drained would wait on an answer nothing will
    /// send (`a_call_on_an_ended_connection_is_refused_not_left_waiting`).
    /// A call registered while the end is still the writer's provisional
    /// one is answered by the reader's end that follows it.
    fn register(
        &self,
        id: &RequestId,
        answer: oneshot::Sender<Answer>,
    ) -> Result<(), TransportError> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(code) = self.ended() {
            return Err(code);
        }
        pending.insert(id.as_str().to_owned(), answer);
        Ok(())
    }

    fn take(&self, id: &str) -> Option<oneshot::Sender<Answer>> {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id)
    }

    /// The connection is over: the code is recorded, and every waiting
    /// call is answered with it then, not when it is next polled. The
    /// first end recorded stands, except that a writer's failure is
    /// provisional: the server's own `close` frame, read after it,
    /// replaces its code, so a caller is told why the SERVER ended the
    /// connection rather than that a write failed against the socket it
    /// was closing. The writer's end answers no call: the reader, which
    /// then reads what has already arrived, answers them when it stops,
    /// and its end makes the code final, so a call answers the code the
    /// session ends with (`a_failed_write_keeps_the_servers_close_code`;
    /// architect-cto's ruling, 01a11be2).
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
        } else if by == EndedBy::ServerClose && self.provisional.load(Ordering::SeqCst) {
            *ended = Some(code);
        }
        if by != EndedBy::Writer {
            self.provisional.store(false, Ordering::SeqCst);
            let code = ended.unwrap_or(code);
            for (_, answer) in pending.drain() {
                let _ = answer.send(Err(code));
            }
        }
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
    let (pong, pong_rx) = watch::channel::<Option<Vec<u8>>>(None);
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
    // And the writer's failure is the reader's cue to stop waiting: it
    // reads what has already arrived, then ends.
    let (write_failed, write_failed_rx) = watch::channel(false);
    let inbox = events_tx.as_ref().map(|(_, inbox)| Arc::clone(inbox));
    let writer = tokio::spawn(write_loop(
        write,
        out_rx,
        pong_rx,
        (ended_rx, write_failed),
        (Arc::clone(&shared), inbox),
    ));
    let reader = tokio::spawn(read_loop(
        reader,
        response.ipc_version,
        Arc::clone(&shared),
        pong,
        events_tx,
        (ended, write_failed_rx),
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
        self.exchange(|id| request.into_frame(id, None)).await
    }

    /// [`call`](Self::call) for the frame `frame` makes under a fresh id.
    /// A frame past the 128 KiB ceiling is refused here, as that call's
    /// `PayloadTooLarge`, and the connection carries on: no request a
    /// typed [`Request`] can hold is that large -- the largest legal
    /// `direct.send` fits with its whole envelope (`ipc-protocol`'s
    /// `the_largest_legal_payload_fits_with_its_whole_envelope`) -- so the
    /// refusal is reached through this seam alone
    /// (`an_unencodable_request_fails_its_call_and_not_the_connection`).
    async fn exchange<T: DeserializeOwned>(
        &self,
        frame: impl FnOnce(RequestId) -> RequestFrame,
    ) -> Result<T, TransportError> {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = RequestId::new(format!("r{n}"))?;
        let bytes = Frame::Request(frame(id.clone()))
            .encode()
            .map_err(|_| TransportError::PayloadTooLarge)?;
        let (answer, response) = oneshot::channel();
        self.shared.register(&id, answer)?;
        let mut guard = CancelOnDrop {
            id: Some(id),
            out: self.out.clone(),
            shared: Arc::clone(&self.shared),
        };
        // A writer that has stopped refuses the send, but the call is
        // registered: the reader's end, which always follows, answers it
        // with the code the session ends with, not whatever was recorded
        // at this instant.
        let _ = self.out.send(Outgoing::Bytes(bytes)).await;
        let response = response.await.map_err(|_| self.gone())??;
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
        // Ended for good already: nothing left to release. A writer's
        // provisional end waits below for the reader's, which names the
        // code.
        if self.shared.uninvited.load(Ordering::SeqCst) && self.has_ended() {
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
        // either way, because the id is no longer pending. A cancel carries
        // an id of 128 characters at most, far inside the ceiling; were it
        // refused, it would be skipped as a full queue skips it.
        if self.shared.take(id.as_str()).is_some()
            && let Ok(bytes) = Frame::Cancel(Cancel::new(id)).encode()
        {
            let _ = self.out.try_send(Outgoing::Bytes(bytes));
        }
    }
}

/// Which half saw the end, and how: what decides whether a later end may
/// replace the recorded code ([`Shared::end`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndedBy {
    /// The writer failed on its own. Provisional, and it answers no call:
    /// the reader's end that follows does both.
    Writer,
    /// The reader read the server's `close` frame.
    ServerClose,
    /// The reader saw any other end.
    Reader,
}

/// The connection is over, for either half: the end recorded and a
/// session waiting in `ready` woken to read it from `events`. The
/// reader's end answers every waiting call with the code the session ends
/// with; the writer's is provisional and answers none, the reader's
/// following it ([`Shared::end`]).
fn finish(shared: &Shared, inbox: Option<&Inbox>, code: TransportError, clean: bool, by: EndedBy) {
    shared.end(code, clean, by);
    if let Some(inbox) = inbox {
        inbox.wake_all();
    }
}

/// The writer. One whose write fails ends the connection BEFORE it drops
/// its queue, so a call that fails on the dropped queue finds the end
/// already recorded and `events` already refusing, and then tells the
/// reader, which may be waiting on a server that stopped reading and
/// kept writing, to read what has arrived and stop
/// (`a_failed_write_ends_the_session_before_the_reader_sees_it`).
async fn write_loop(
    mut write: OwnedWriteHalf,
    mut out: mpsc::Receiver<Outgoing>,
    mut pong: watch::Receiver<Option<Vec<u8>>>,
    (mut ended, write_failed): (watch::Receiver<bool>, watch::Sender<bool>),
    (shared, inbox): (Arc<Shared>, Option<Arc<Inbox>>),
) {
    loop {
        // An echo goes ahead of whatever requests are queued: the server
        // counts a late pong as a missed one.
        let next = tokio::select! {
            biased;
            _ = ended.wait_for(|ended| *ended) => break,
            Ok(()) = pong.changed() => match pong.borrow_and_update().clone() {
                Some(bytes) => Outgoing::Bytes(bytes),
                None => continue,
            },
            next = out.recv() => match next {
                Some(next) => next,
                None => break,
            },
        };
        let Outgoing::Bytes(bytes) = next else {
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
            write_failed.send_replace(true);
            break;
        }
    }
    let _ = write.shutdown().await;
}

/// The reader. Until the writer fails it waits on the socket; after, it
/// reads what has already arrived, up to [`DRAIN_BUDGET`] -- a Unix socket keeps what the
/// server sent before it closed, so a `close` already sent is read -- and
/// then ends, answering every waiting call with the code the session
/// ends with (`a_failed_write_keeps_the_servers_close_code`). Nothing it
/// waits on afterwards can hold a call: a server that stopped reading and
/// kept its write half open sends nothing more to wait for
/// (`a_failed_write_ends_the_session_before_the_reader_sees_it`).
///
/// And it bounds a silent server, once it has evidence the server runs
/// keepalive: [`CLIENT_SILENCE_TIMEOUT`] arms at the first `ping` read --
/// the wire says nothing else about keepalive, and a daemon with it off
/// never pings (`no_ping_read_never_times_out`) -- and then, with no
/// frame of any kind read for that long, the connection ends `Timeout`
/// and so does every waiting call (`a_ping_then_silence_ends_timeout`).
/// A reader paused on a full buffer is not reading, so the bound is
/// suspended there and starts afresh when it reads again
/// (`a_paused_reader_does_not_time_out`).
async fn read_loop(
    mut reader: Reader,
    version: IpcVersion,
    shared: Arc<Shared>,
    echoes: watch::Sender<Option<Vec<u8>>>,
    events: Option<(mpsc::Sender<SessionEvent>, Arc<Inbox>)>,
    (ended, mut write_failed): (watch::Sender<bool>, watch::Receiver<bool>),
) {
    let mut draining = false;
    // When the silence bound ends the connection: `None` until a ping is
    // read.
    let mut silence: Option<tokio::time::Instant> = None;
    let (code, clean, by) = loop {
        let next = if draining {
            match reader.next_now() {
                // Nothing more has arrived: the provisional end stands.
                Ok(None) => break (TransportError::BackendUnavailable, false, EndedBy::Reader),
                next => next,
            }
        } else {
            tokio::select! {
                biased;
                // Cancel-safe: `next` keeps what it read in its buffer. A
                // writer that ended without failing drops its sender, which
                // disables this arm rather than firing it.
                Ok(_) = write_failed.wait_for(|failed| *failed) => {
                    draining = true;
                    continue;
                }
                next = reader.next() => next,
                () = tokio::time::sleep_until(silence.unwrap_or_else(tokio::time::Instant::now)),
                    if silence.is_some() =>
                {
                    break (TransportError::Timeout, false, EndedBy::Reader);
                }
            }
        };
        if let (Ok(Some(_)), Some(deadline)) = (&next, silence.as_mut()) {
            *deadline = tokio::time::Instant::now() + CLIENT_SILENCE_TIMEOUT;
        }
        match next {
            Ok(Some(Frame::Response(response))) => {
                // An id no call waits for was cancelled: discarded.
                if let Some(answer) = shared.take(response.id.as_str()) {
                    let _ = answer.send(Ok(response));
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
                // end. Once the writer has failed the reader no longer
                // waits for room: an event that does not fit ends the
                // reading there, as nothing more having arrived does.
                let room = if draining {
                    events.try_reserve()
                } else {
                    tokio::select! {
                        biased;
                        room = events.reserve() => room.map_err(|_| {
                            mpsc::error::TrySendError::Closed(())
                        }),
                        Ok(_) = write_failed.wait_for(|failed| *failed) => {
                            draining = true;
                            events.try_reserve()
                        }
                    }
                };
                // Reading again: the suspended bound starts afresh.
                if let Some(deadline) = silence.as_mut() {
                    *deadline = tokio::time::Instant::now() + CLIENT_SILENCE_TIMEOUT;
                }
                match room {
                    Ok(permit) => {
                        permit.send(event);
                        inbox.wake_all();
                    }
                    Err(mpsc::error::TrySendError::Closed(())) => {}
                    Err(mpsc::error::TrySendError::Full(())) => {
                        break (TransportError::BackendUnavailable, false, EndedBy::Reader);
                    }
                }
            }
            Ok(Some(Frame::Ping(ping))) => {
                // The server runs keepalive: the silence bound is armed.
                silence.get_or_insert_with(|| tokio::time::Instant::now() + CLIENT_SILENCE_TIMEOUT);
                // A nonce is 64 characters at most, far inside the
                // ceiling; were its echo refused, the ping would go
                // unanswered and the server's keepalive would decide.
                if let Ok(bytes) = Frame::Pong(ping.echo()).encode() {
                    echoes.send_replace(Some(bytes));
                }
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
    /// The duplicate [`Reader::next_now`] reads through, once it has.
    now: Option<std::os::unix::net::UnixStream>,
    /// What [`Reader::next_now`] has read, against [`DRAIN_BUDGET`].
    drained: usize,
}

impl Reader {
    const fn new(inner: OwnedReadHalf) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            now: None,
            drained: 0,
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

    /// The next frame among what has already arrived, without waiting:
    /// `None` once nothing complete is left to read, the stream's end
    /// included, or once [`DRAIN_BUDGET`] bytes have been read.
    ///
    /// Read through a duplicate of the socket, not `try_read`: tokio's
    /// `try_read` answers `WouldBlock` without asking the kernel until its
    /// reactor has seen the socket readable, so a `close` already sitting
    /// in the socket went unread when the writer failed first (measured,
    /// current-thread). The duplicate shares the descriptor's non-blocking
    /// mode, so its read never waits either
    /// (`a_failed_write_ends_the_session_before_the_reader_sees_it`, whose
    /// server keeps its write half open, would hang if it did).
    fn next_now(&mut self) -> Result<Option<Frame>, TransportError> {
        loop {
            match decode_frame(&self.buf) {
                Ok(DecodedFrame { body, consumed }) => {
                    self.buf.drain(..consumed);
                    return Frame::parse(&body).map(Some);
                }
                Err(FrameError::Incomplete { .. }) => {}
                Err(_) => return Err(TransportError::ProtocolViolation),
            }
            let socket = match &mut self.now {
                Some(socket) => socket,
                None => self.now.insert(
                    self.inner
                        .as_ref()
                        .as_fd()
                        .try_clone_to_owned()
                        .map(std::os::unix::net::UnixStream::from)
                        .map_err(|_| TransportError::BackendUnavailable)?,
                ),
            };
            if self.drained >= DRAIN_BUDGET {
                return Ok(None);
            }
            let mut chunk = [0_u8; 8192];
            match socket.read(&mut chunk) {
                Ok(0) => return Ok(None),
                Ok(read) => {
                    self.drained += read;
                    self.buf.extend_from_slice(&chunk[..read]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(_) => return Err(TransportError::BackendUnavailable),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use std::collections::BTreeSet;

    use interweave_ipc_protocol::{
        ChannelParams, MAX_BODY_BYTES, RequestedCapability, encode_frame,
    };
    use interweave_transport_api::ChannelId;
    use serde::de::IgnoredAny;
    use tokio::net::UnixListener;

    use super::*;

    const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const PATIENCE: Duration = Duration::from_secs(5);

    fn join() -> Request {
        Request::ChannelJoin(ChannelParams {
            channel: ChannelId::parse("general").expect("channel"),
        })
    }

    /// The writer's provisional end is shown to nobody: a call still
    /// registers, and is answered by the reader's end with the code that
    /// end makes final -- here the server's `close` -- which is the code
    /// every later reader of the end sees. The control: the reader's end
    /// is final, and a call after it is refused with that code.
    #[test]
    fn a_provisional_end_is_shown_to_nobody() {
        let shared = Shared::default();
        shared.end(TransportError::BackendUnavailable, false, EndedBy::Writer);
        assert_eq!(shared.ended(), None, "provisional: not shown");
        let (answer, mut answered) = oneshot::channel();
        let id = RequestId::new("r0").expect("id");
        assert_eq!(shared.register(&id, answer), Ok(()), "still registers");
        shared.end(
            TransportError::ProtocolViolation,
            false,
            EndedBy::ServerClose,
        );
        assert_eq!(
            answered.try_recv().expect("answered").map(|_| ()),
            Err(TransportError::ProtocolViolation)
        );
        assert_eq!(shared.ended(), Some(TransportError::ProtocolViolation));
        let (late, _) = oneshot::channel();
        let id = RequestId::new("r1").expect("id");
        assert_eq!(
            shared.register(&id, late),
            Err(TransportError::ProtocolViolation),
            "final: refused"
        );
    }

    async fn next(reader: &mut Reader) -> Frame {
        tokio::time::timeout(PATIENCE, reader.next())
            .await
            .expect("the client writes in time")
            .expect("a frame")
            .expect("not the end")
    }

    /// A request frame past the 128 KiB ceiling -- built through the
    /// `exchange` seam, since no typed `Request` is that large -- fails
    /// its own call with `PayloadTooLarge` and puts nothing on the wire:
    /// the server's next frame is the control's request, answered, on a
    /// connection that never ended.
    #[tokio::test]
    async fn an_unencodable_request_fails_its_call_and_not_the_connection() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("data.sock");
        let listener = UnixListener::bind(&path).expect("binds");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accepts");
            let (read, mut write) = stream.into_split();
            let mut reader = Reader::new(read);
            assert!(matches!(next(&mut reader).await, Frame::Hello(_)));
            let answer = |body: String| encode_frame(&body).expect("a frame");
            write
                .write_all(&answer(format!(
                    r#"{{"type":"hello_response","ipc_version":{{"major":2,"minor":0}},"transport_contract_version":"2.0","peer":"{PEER}","granted_capabilities":["commands"]}}"#
                )))
                .await
                .expect("written");
            let Frame::Request(request) = next(&mut reader).await else {
                panic!("a request");
            };
            write
                .write_all(&answer(format!(
                    r#"{{"type":"response","id":"{}","ok":true,"result":{{}}}}"#,
                    request.id.as_str()
                )))
                .await
                .expect("written");
            request.method
        });
        let hello = crate::session::hello(
            "k",
            None,
            BTreeSet::from([RequestedCapability::Commands]),
            BTreeSet::new(),
        );
        let opened = open(&path, hello, |_| None).await.expect("opens");
        let connection = opened.connection;

        let refused = connection
            .exchange::<IgnoredAny>(|id| {
                let mut frame = join().into_frame(id, None);
                frame.method = "m".repeat(MAX_BODY_BYTES);
                frame
            })
            .await;
        assert_eq!(refused.map(|_| ()), Err(TransportError::PayloadTooLarge));
        assert!(!connection.has_ended(), "the connection carries on");

        let control = tokio::time::timeout(PATIENCE, connection.call::<IgnoredAny>(join()))
            .await
            .expect("answered in time");
        assert!(control.is_ok(), "the control is answered: {control:?}");
        assert_eq!(
            server.await.expect("the server"),
            "channel.join",
            "the refused frame never reached the wire"
        );
    }
}
