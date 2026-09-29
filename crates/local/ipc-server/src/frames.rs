// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Framed I/O on one connection (`LOCAL-IPC.md` §Framing).
//!
//! The reader never trusts a declared length: `decode_frame` checks it
//! against the 128 KiB ceiling before the buffer is grown to hold it, so
//! a hostile prefix costs nothing.
//!
//! The writer has two lanes. The CONTROL lane carries what must never
//! wait behind application traffic -- responses, `close`, `ping`,
//! `server_state` -- and is drained first; the EVENT lane is bounded by
//! the session's own event queue, and the event pump fills it only when
//! it has room (plan §16 (8)): the server adds no queue of its own, so
//! overflow stays the binding's drop-oldest-broadcast behaviour.

use interweave_ipc_protocol::{DecodedFrame, Frame, FrameError, decode_frame};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;

/// What the reader bumps into.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// Not a frame: the connection closes with `ProtocolViolation`.
    Frame(FrameError),
    /// The peer went away mid-frame.
    Truncated,
    /// The socket failed.
    Io,
}

/// Frames off a byte stream.
pub(crate) struct FrameReader<R> {
    inner: R,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub(crate) const fn new(inner: R) -> Self {
        Self {
            inner,
            buf: Vec::new(),
        }
    }

    /// The next frame's body, or `None` at a clean end of stream (between
    /// frames).
    ///
    /// Cancel-safe: a partial frame stays in the buffer.
    pub(crate) async fn next(&mut self) -> Result<Option<String>, ReadError> {
        loop {
            match decode_frame(&self.buf) {
                Ok(DecodedFrame { body, consumed }) => {
                    self.buf.drain(..consumed);
                    return Ok(Some(body));
                }
                Err(FrameError::Incomplete { .. }) => {}
                Err(e) => return Err(ReadError::Frame(e)),
            }
            // `Incomplete` is only returned for a declared length inside the
            // ceiling, so what is read here is bounded by it.
            let mut chunk = [0_u8; 8192];
            let read = self
                .inner
                .read(&mut chunk)
                .await
                .map_err(|_| ReadError::Io)?;
            if read == 0 {
                return if self.buf.is_empty() {
                    Ok(None)
                } else {
                    Err(ReadError::Truncated)
                };
            }
            self.buf.extend_from_slice(&chunk[..read]);
        }
    }
}

/// The two lanes into one connection's writer.
#[derive(Debug, Clone)]
pub(crate) struct Lanes {
    pub(crate) control: mpsc::Sender<Frame>,
    pub(crate) events: mpsc::Sender<Frame>,
}

/// Room for every response a connection can owe at once, plus a ping,
/// a `server_state` and a `close`.
pub(crate) const CONTROL_LANE: usize = crate::MAX_IN_FLIGHT + crate::MAX_PENDING + 3;

/// Start a connection's writer: control first, then events, until both
/// lanes are dropped or a `close` has been written.
pub(crate) fn spawn_writer<W>(inner: W, event_lane: usize) -> (Lanes, tokio::task::JoinHandle<()>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (control, control_rx) = mpsc::channel(CONTROL_LANE);
    let (events, events_rx) = mpsc::channel(event_lane.max(1));
    let task = tokio::spawn(write_loop(inner, control_rx, events_rx));
    (Lanes { control, events }, task)
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut inner: W,
    mut control: mpsc::Receiver<Frame>,
    mut events: mpsc::Receiver<Frame>,
) {
    loop {
        let frame = tokio::select! {
            biased;
            frame = control.recv() => match frame {
                Some(frame) => frame,
                None => break,
            },
            Some(frame) = events.recv() => frame,
        };
        let closing = matches!(frame, Frame::Close(_));
        let Ok(bytes) = frame.encode() else {
            // A frame this crate built past the ceiling is its own bug;
            // the connection cannot recover a sensible stream from it.
            break;
        };
        if inner.write_all(&bytes).await.is_err() {
            break;
        }
        if closing {
            break;
        }
    }
    let _ = inner.shutdown().await;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use interweave_ipc_protocol::{Close, MAX_BODY_BYTES, encode_frame};
    use interweave_transport_api::TransportError;

    #[tokio::test]
    async fn frames_split_across_reads_and_packed_in_one_are_both_read() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = FrameReader::new(server);
        let one = encode_frame(r#"{"type":"ping","nonce":"aaaaaaaaaaaaaaaa"}"#).expect("frame");
        let two = encode_frame(r#"{"type":"cancel","id":"1"}"#).expect("frame");
        let writer = tokio::spawn(async move {
            // One byte at a time, then two frames in one write.
            for byte in &one {
                client.write_all(&[*byte]).await.expect("write");
            }
            client
                .write_all(&[two.clone(), two].concat())
                .await
                .expect("write");
        });
        assert!(
            reader
                .next()
                .await
                .expect("one")
                .expect("some")
                .contains("ping")
        );
        assert!(
            reader
                .next()
                .await
                .expect("two")
                .expect("some")
                .contains("cancel")
        );
        assert!(
            reader
                .next()
                .await
                .expect("three")
                .expect("some")
                .contains("cancel")
        );
        writer.await.expect("writer");
        assert!(reader.next().await.expect("eof").is_none(), "a clean end");
    }

    #[tokio::test]
    async fn a_declared_length_past_the_ceiling_is_refused_before_it_is_read() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = FrameReader::new(server);
        let declared = u32::try_from(MAX_BODY_BYTES + 1).expect("fits");
        client
            .write_all(&declared.to_be_bytes())
            .await
            .expect("write");
        assert!(matches!(
            reader.next().await,
            Err(ReadError::Frame(FrameError::BodyTooLarge { .. }))
        ));
        assert!(
            reader.buf.capacity() < 1024,
            "nothing reserved for the claim"
        );
    }

    #[tokio::test]
    async fn a_stream_ending_inside_a_frame_is_truncated_not_clean() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = FrameReader::new(server);
        client.write_all(&[0, 0, 0, 9, b'{']).await.expect("write");
        drop(client);
        assert!(matches!(reader.next().await, Err(ReadError::Truncated)));
    }

    /// Both lanes ready at once: the control frame goes first. An
    /// unbiased select would win this half the time, so it is run until
    /// such a select would have lost it with near certainty.
    #[tokio::test]
    async fn the_control_lane_goes_first_and_a_close_ends_the_writer() {
        for _ in 0..32 {
            let (client, server) = tokio::io::duplex(1 << 16);
            let (lanes, task) = spawn_writer(server, 4);
            let event =
                Frame::parse(r#"{"type":"server_state","health":"healthy"}"#).expect("frame");
            lanes.events.send(event).await.expect("queued");
            lanes
                .control
                .send(Frame::Close(Close::new(TransportError::ShuttingDown)))
                .await
                .expect("queued");
            task.await.expect("the writer ends after the close");
            let mut reader = FrameReader::new(client);
            let first = reader.next().await.expect("read").expect("a frame");
            assert!(first.contains("close"), "the control lane first: {first}");
            assert!(
                reader.next().await.expect("read").is_none(),
                "and nothing after the close"
            );
        }
    }
}
