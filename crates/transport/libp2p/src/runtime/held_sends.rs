// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Direct sends held while the dial they started is in flight.
//!
//! `TRANSPORT.md` lets the runtime dial an authorized peer it holds a
//! known path to, under the command deadline, and until architect-cto's
//! ruling of 2026-10-06 (relay seq 13444) it never did: a send to a peer
//! with no connection answered `PeerUnreachable` at once, and a person
//! whose peer had just restarted read that for the backoff's 30-60 s.
//! A send now dials once and waits here for the outcome: flushed onto
//! the wire when a connection to the peer is retained, failed when a dial
//! to the peer fails with no other dial, race or connection left for it,
//! answered by current policy when its dial's connection was refused at
//! retention, and otherwise failed at [`SEND_DIAL_HORIZON_MS`] -- the
//! loop wakes at the earliest horizon -- or, at the next retry tick, once
//! its caller has stopped waiting. One end reaches only the horizon: a
//! deferred circuit route the gate refuses when its head-start runs out,
//! which is not a failed dial event (#208 review F5).
//!
//! What it holds counts against the same bounds as an exchange already
//! on the wire (`admit_outbound`), so holding moves no work outside them.

use tokio::sync::oneshot;

use interweave_transport_api::TransportError as DirectError;
use interweave_transport_api::{DirectMessageV2, EndpointId, TransportIdentity};

/// How long a send waits for the connection its dial is making: the
/// contract's default command deadline (`TRANSPORT.md`, 10 s).
///
/// The caller's own deadline is usually what ends the wait first -- the
/// IPC server answers `Timeout` at the request's deadline and drops the
/// reply, which [`HeldSends::take_expired`] sees -- so this is the bound
/// for a caller that never gives up, not the one a person meets.
pub(super) const SEND_DIAL_HORIZON_MS: u64 = 10_000;

/// One send waiting for a connection to its peer.
pub(super) struct HeldSend {
    /// The peer the send is to.
    pub(super) peer: TransportIdentity,
    /// The lease the send was made under, asked again at dispatch: the
    /// endpoint can be revoked, released or narrowed while the send waits.
    pub(super) lease: interweave_local_client_api::EndpointLease,
    /// The frame, its source endpoint already the lease's.
    pub(super) frame: Box<DirectMessageV2>,
    /// The caller waiting for the outcome.
    pub(super) reply: oneshot::Sender<Result<EndpointId, DirectError>>,
    /// When the wait ends whatever happens.
    pub(super) until_ms: u64,
}

/// The sends waiting for a connection, in arrival order.
///
/// Bounded by `admit_outbound`, which counts them with the exchanges in
/// flight before one is held, so the table never exceeds the direct
/// ceiling. A scan per question, as `pending_direct`'s count is: the
/// ceiling is small, and one list cannot disagree with an index of it.
#[derive(Default)]
pub(super) struct HeldSends {
    held: Vec<HeldSend>,
}

impl HeldSends {
    /// Hold `send` until its peer connects, its dials fail or it expires.
    pub(super) fn hold(&mut self, send: HeldSend) {
        self.held.push(send);
    }

    /// The peer of every held send, for the outbound bound's count.
    pub(super) fn peers(&self) -> impl Iterator<Item = &TransportIdentity> {
        self.held.iter().map(|s| &s.peer)
    }

    /// Whether any send to `peer` is held.
    #[must_use]
    pub(super) fn holds(&self, peer: &TransportIdentity) -> bool {
        self.held.iter().any(|s| &s.peer == peer)
    }

    /// Every send held for `peer`, removed, in arrival order.
    pub(super) fn take(&mut self, peer: &TransportIdentity) -> Vec<HeldSend> {
        let (taken, kept) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|s| &s.peer == peer);
        self.held = kept;
        taken
    }

    /// Every send past its horizon at `now_ms` or whose caller stopped
    /// waiting, removed. A send whose reply is closed is returned too, so
    /// the caller of this decides nothing about it but drops it.
    pub(super) fn take_expired(&mut self, now_ms: u64) -> Vec<HeldSend> {
        let (taken, kept) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|s| s.until_ms <= now_ms || s.reply.is_closed());
        self.held = kept;
        taken
    }

    /// Every held send, removed: the runtime is stopping.
    pub(super) fn take_all(&mut self) -> Vec<HeldSend> {
        std::mem::take(&mut self.held)
    }

    /// How many sends are held: each is owed Swarm progress, so the
    /// polling allowance counts them (`polling_room`).
    #[must_use]
    pub(super) fn len(&self) -> usize {
        self.held.len()
    }

    /// The earliest horizon among the held sends, which the loop wakes
    /// at -- not at the retry tick, which a configuration may set far
    /// longer than the horizon (#208 bot thread, B2).
    #[must_use]
    pub(super) fn next_due_ms(&self) -> Option<u64> {
        self.held.iter().map(|s| s.until_ms).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interweave_transport_api::{MessageId, Payload};

    const A: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const B: &str = "12D3KooWHsy9ZMqTYfTPpJd8YXhZGKrLWzkWT9BgX9DeyF8Fs3GQ";

    fn send(
        peer: &str,
        until_ms: u64,
    ) -> (HeldSend, oneshot::Receiver<Result<EndpointId, DirectError>>) {
        let (reply, answer) = oneshot::channel();
        let frame = DirectMessageV2 {
            message_id: MessageId::from_bytes([7; 16]),
            source_endpoint: EndpointId::parse("human").expect("valid"),
            destination_endpoint: None,
            sent_at_ms: 0,
            payload: Payload::at_ceiling(None, b"x".to_vec()).expect("within the ceiling"),
        };
        (
            HeldSend {
                peer: TransportIdentity::parse(peer).expect("valid"),
                lease: interweave_local_client_api::EndpointLease {
                    endpoint: EndpointId::parse("human").expect("valid"),
                    epoch: interweave_local_client_api::Generation::parse("0123456789abcdef")
                        .expect("valid"),
                },
                frame: Box::new(frame),
                reply,
                until_ms,
            },
            answer,
        )
    }

    #[test]
    fn a_peers_sends_are_taken_together_and_only_its_own() {
        let mut held = HeldSends::default();
        let (a1, _a1) = send(A, 10);
        let (b1, _b1) = send(B, 10);
        let (a2, _a2) = send(A, 10);
        held.hold(a1);
        held.hold(b1);
        held.hold(a2);
        let a = TransportIdentity::parse(A).expect("valid");
        assert!(held.holds(&a));
        assert_eq!(held.peers().filter(|p| **p == a).count(), 2);
        assert_eq!(held.take(&a).len(), 2);
        assert!(!held.holds(&a), "taken, not copied");
        assert_eq!(held.len(), 1, "B's send stays");
    }

    #[test]
    fn a_send_expires_at_its_horizon_or_when_its_caller_stops_waiting() {
        let mut held = HeldSends::default();
        let (due, _due) = send(A, 10);
        let (later, _later) = send(A, 20);
        let (abandoned, answer) = send(B, 20);
        drop(answer);
        held.hold(due);
        held.hold(later);
        held.hold(abandoned);
        // The control: before the horizon, only the abandoned one goes.
        let first = held.take_expired(9);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].peer, TransportIdentity::parse(B).expect("valid"));
        assert_eq!(held.next_due_ms(), Some(10), "the earliest horizon");
        assert_eq!(held.take_expired(10).len(), 1, "at the horizon");
        assert_eq!(held.next_due_ms(), Some(20));
        assert_eq!(held.len(), 1);
        assert_eq!(held.take_all().len(), 1);
        assert_eq!(held.len(), 0);
    }
}
