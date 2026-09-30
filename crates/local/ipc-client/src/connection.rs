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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use interweave_ipc_protocol::{
    Cancel, DecodedFrame, Frame, FrameError, HELLO_TIMEOUT, Hello, HelloResponse, Request,
    RequestId, ResponseFrame, decode_frame,
};
use interweave_local_client_api::SessionEvent;
use interweave_transport_api::TransportError;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, oneshot, watch};
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
    /// the code.
    fn end(&self, code: TransportError) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let mut ended = self.ended.lock().unwrap_or_else(PoisonError::into_inner);
        ended.get_or_insert(code);
        pending.clear();
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
/// data connection -- the receiving end of its event buffer.
pub(crate) struct Opened {
    pub(crate) connection: Connection,
    pub(crate) response: HelloResponse,
    pub(crate) events: Option<mpsc::Receiver<SessionEvent>>,
}

/// Connect to `socket`, send `hello` and read its answer. A data
/// connection's event buffer holds `event_queue(&response)` events.
///
/// # Errors
/// `BackendUnavailable` when the socket cannot be reached or closes
/// before answering; the server's `close` code when it refuses the hello;
/// `ProtocolViolation` for any other first frame.
pub(crate) async fn open(
    socket: &Path,
    hello: Hello,
    event_queue: fn(&HelloResponse) -> Option<usize>,
) -> Result<Opened, TransportError> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|_| TransportError::BackendUnavailable)?;
    let (read, mut write) = stream.into_split();
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
    let (out, out_rx) = mpsc::channel(OUTGOING);
    // Capacity one: the server holds one nonce outstanding at a time
    // (LOCAL-IPC.md §Disconnect/reconnect and optional keepalive), so one
    // echo is owed at a time.
    let (pong, pong_rx) = mpsc::channel(1);
    let shared = Arc::new(Shared::default());
    let (events_tx, events) = match event_queue(&response) {
        Some(bound) => {
            let (tx, rx) = mpsc::channel(bound.max(1));
            (Some(tx), Some(rx))
        }
        None => (None, None),
    };
    // The reader's end is the writer's: a connection the client stopped
    // reading -- the server closed it, or broke the protocol -- is shut on
    // this side too, so the server sees it end and releases the session.
    let (ended, ended_rx) = watch::channel(false);
    let writer = tokio::spawn(write_loop(write, out_rx, pong_rx, ended_rx));
    let reader = tokio::spawn(read_loop(
        reader,
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
        if self.has_ended() {
            return Err(self.gone());
        }
        let _ = self.out.send(Outgoing::Finish).await;
        let Some(reader) = self.reader.as_mut() else {
            return Ok(());
        };
        match tokio::time::timeout(CLOSE_WAIT, reader).await {
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

async fn write_loop(
    mut write: OwnedWriteHalf,
    mut out: mpsc::Receiver<Outgoing>,
    mut pong: mpsc::Receiver<Frame>,
    mut ended: watch::Receiver<bool>,
) {
    loop {
        // An echo goes ahead of whatever requests are queued: the server
        // counts a late pong as a missed one.
        let next = tokio::select! {
            biased;
            _ = ended.wait_for(|ended| *ended) => break,
            Some(frame) = pong.recv() => Outgoing::Frame(frame),
            next = out.recv() => match next {
                Some(next) => next,
                None => break,
            },
        };
        let Outgoing::Frame(frame) = next else {
            break;
        };
        let Ok(bytes) = frame.encode() else {
            break;
        };
        if write.write_all(&bytes).await.is_err() {
            break;
        }
    }
    let _ = write.shutdown().await;
}

async fn read_loop(
    mut reader: Reader,
    shared: Arc<Shared>,
    echoes: mpsc::Sender<Frame>,
    events: Option<mpsc::Sender<SessionEvent>>,
    ended: watch::Sender<bool>,
) {
    let code = loop {
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
                let Some(events) = &events else {
                    break TransportError::ProtocolViolation;
                };
                let event = match frame.event() {
                    Ok(event) => event.into_session(),
                    Err(code) => break code,
                };
                // THE BOUND: a full buffer holds the reader here, with this
                // one event in hand, so the socket is not read until the
                // session drains it (LOCAL-IPC.md, A 2026-09-30) -- the
                // client holds the granted bound plus that one. A session
                // already closed takes nothing, and reading goes on to the
                // end.
                let _ = events.send(event).await;
            }
            Ok(Some(Frame::Ping(ping))) => {
                let _ = echoes.try_send(Frame::Pong(ping.echo()));
            }
            Ok(Some(Frame::ServerState(_))) => {}
            Ok(Some(Frame::Close(close))) => break close.code,
            Ok(Some(_)) => break TransportError::ProtocolViolation,
            Ok(None) => break TransportError::BackendUnavailable,
            Err(code) => break code,
        }
    };
    shared.end(code);
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
