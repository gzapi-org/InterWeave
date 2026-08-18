// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The ADR-0044 retention state machine.
//!
//! Message content is durable in exactly three states — pending outbound,
//! unread inbound, and inbound kept after reading — and this module is
//! the only thing that decides which. It stores nothing itself; a store
//! carries out [`Durability`], which is what lets every transition be
//! tested by enumeration.
//!
//! # A remote sender cannot make a receiver keep anything
//!
//! [`InboundMessage::keep`] takes **no argument at all** beyond `&mut
//! self`. There is no field, flag, or parameter through which envelope
//! content, a contact label, an EndpointId, or a notification action
//! could reach the decision — a remote sender has nothing to set. And
//! [`KeepRefused::NotYetRead`] enforces the second half: `Keep` is
//! available only after local read state, so content that was never
//! presented cannot become durable.
//!
//! # No read receipt exists
//!
//! [`InboundMessage::mark_read`] changes local state and returns nothing
//! to send. `read` is a UI state; it does not prove a human perceived
//! anything and generates no network traffic.

use serde::{Deserialize, Serialize};

/// Whether the content must exist in durable storage.
///
/// The single output of this module. A `Remove` is an instruction to
/// delete, not a suggestion: transport-terminal outbound and read-unkept
/// inbound are gone by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Durability {
    /// Content must be durably stored.
    Durable,
    /// Content must not be durably stored; delete any existing copy.
    ///
    /// It may remain in bounded process memory for the current session,
    /// which is how a read-but-unkept message stays on screen.
    Remove,
}

/// Where an outbound message is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundState {
    /// Composed and not yet transport-terminal. Durable.
    Pending,
    /// Transport-terminal: accepted, published, cancelled, or abandoned.
    ///
    /// One state, not four. What matters for retention is that transport
    /// will do no more with it — distinguishing the causes here would
    /// invite a "delivered" claim the transport never made.
    Terminal,
}

/// What ended an outbound message's pending state.
///
/// Diagnostic only. All four produce the same retention answer, and the
/// type exists so a UI can explain itself without the state machine
/// acquiring four states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCause {
    /// Direct send received `AcceptedV2`.
    Accepted,
    /// Broadcast published locally.
    ///
    /// Terminal because broadcast has no per-recipient acknowledgement.
    /// A UI must not call this "delivered to recipients".
    Published,
    /// The user cancelled or deleted it.
    Cancelled,
}

/// A message this profile is sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundMessage {
    state: OutboundState,
    cause: Option<TerminalCause>,
}

impl Default for OutboundMessage {
    fn default() -> Self {
        Self::composed()
    }
}

impl OutboundMessage {
    /// A newly composed message, durable before transport is invoked.
    ///
    /// The order is load-bearing: the durable pending record is created
    /// **first**, then transport is called. Sending first would lose the
    /// message if the process died between the call and the record.
    #[must_use]
    pub const fn composed() -> Self {
        Self {
            state: OutboundState::Pending,
            cause: None,
        }
    }

    /// The current state.
    #[must_use]
    pub const fn state(&self) -> OutboundState {
        self.state
    }

    /// Why it became terminal, if it has.
    #[must_use]
    pub const fn cause(&self) -> Option<TerminalCause> {
        self.cause
    }

    /// Whether the content must be durably stored right now.
    #[must_use]
    pub const fn durability(&self) -> Durability {
        match self.state {
            OutboundState::Pending => Durability::Durable,
            OutboundState::Terminal => Durability::Remove,
        }
    }

    /// Record that transport will do no more with this message.
    ///
    /// Returns the resulting durability so a caller cannot forget to act
    /// on it. Idempotent: a duplicate terminal event keeps the first
    /// cause, because a retry reaching terminal twice must not rewrite
    /// history.
    pub fn transport_terminal(&mut self, cause: TerminalCause) -> Durability {
        if self.state == OutboundState::Pending {
            self.state = OutboundState::Terminal;
            self.cause = Some(cause);
        }
        self.durability()
    }

    /// Whether a transient failure leaves it durable.
    ///
    /// It does. A send that failed ambiguously is still pending: deleting
    /// it would lose content the user believes they sent, and the whole
    /// reason pending is durable is crash and restart survival.
    #[must_use]
    pub const fn remains_pending_after_transient_failure(&self) -> bool {
        matches!(self.state, OutboundState::Pending)
    }

    /// Whether this content may enter a portable encrypted backup.
    ///
    /// Never. Pending outbound is excluded so a restored or second device
    /// cannot become an implicit replay or delayed-send source.
    #[must_use]
    pub const fn backup_eligible(&self) -> bool {
        false
    }
}

/// Where an inbound message is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboundState {
    /// Committed by the application and not yet read. Durable.
    Unread,
    /// Read, and the receiver has not kept it. Not durable.
    ReadEphemeral,
    /// Read, and the receiver explicitly kept it. Durable.
    Kept,
}

/// Why a `Keep` was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepRefused {
    /// The message has not entered local read state.
    ///
    /// `Keep` is a decision about something the receiver has seen. Allowing
    /// it beforehand would let a notification action, or anything else
    /// acting on an unopened message, make content durable that the human
    /// never looked at.
    NotYetRead,
    /// The content is already gone from durable storage.
    ///
    /// A read-unkept message may be kept later **while it is still in
    /// memory**; once the process has exited, the content is gone by
    /// design and there is nothing to write back.
    ContentNoLongerHeld,
}

/// A message this profile has received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundMessage {
    state: InboundState,
    /// Whether the body is still held anywhere, memory included.
    content_held: bool,
}

impl InboundMessage {
    /// Commit a newly received message as unread.
    ///
    /// This happens **before** normal UI presentation or notification, so
    /// a message the user is told about is a message the store already
    /// holds.
    #[must_use]
    pub const fn committed_unread() -> Self {
        Self {
            state: InboundState::Unread,
            content_held: true,
        }
    }

    /// The current state.
    #[must_use]
    pub const fn state(&self) -> InboundState {
        self.state
    }

    /// Whether the content must be durably stored right now.
    #[must_use]
    pub const fn durability(&self) -> Durability {
        match self.state {
            InboundState::Unread | InboundState::Kept => Durability::Durable,
            InboundState::ReadEphemeral => Durability::Remove,
        }
    }

    /// Enter local read state.
    ///
    /// Returns the resulting durability — `Remove` for a message not yet
    /// kept, which is the transition that makes ordinary conversation
    /// evaporate. Generates nothing to send: there is no read receipt,
    /// and this does not prove a human perceived the content.
    pub fn mark_read(&mut self) -> Durability {
        if self.state == InboundState::Unread {
            self.state = InboundState::ReadEphemeral;
        }
        self.durability()
    }

    /// The receiver chooses to keep it.
    ///
    /// Takes **no argument**. There is no parameter through which remote
    /// content could influence this, which is how "a remote sender cannot
    /// request or force retention" is enforced rather than asserted.
    ///
    /// # Errors
    /// Returns [`KeepRefused::NotYetRead`] before local read state, or
    /// [`KeepRefused::ContentNoLongerHeld`] once the body is gone.
    pub fn keep(&mut self) -> Result<Durability, KeepRefused> {
        match self.state {
            InboundState::Unread => Err(KeepRefused::NotYetRead),
            InboundState::ReadEphemeral if !self.content_held => {
                Err(KeepRefused::ContentNoLongerHeld)
            }
            InboundState::ReadEphemeral | InboundState::Kept => {
                self.state = InboundState::Kept;
                Ok(self.durability())
            }
        }
    }

    /// The receiver removes `Keep`.
    ///
    /// Deletion is immediate. The message returns to read-ephemeral and
    /// its durable copy goes now, not at some later cleanup.
    pub fn unkeep(&mut self) -> Durability {
        if self.state == InboundState::Kept {
            self.state = InboundState::ReadEphemeral;
        }
        self.durability()
    }

    /// The process ended; anything not durable is gone.
    ///
    /// After this a read-unkept message can no longer be kept, which is
    /// the design rather than a limitation: the content was deleted when
    /// it was read.
    pub fn session_ended(&mut self) {
        if self.durability() == Durability::Remove {
            self.content_held = false;
        }
    }

    /// Whether the body is still available to a later `Keep`.
    #[must_use]
    pub const fn content_held(&self) -> bool {
        self.content_held
    }

    /// Whether this content may enter a portable encrypted backup.
    ///
    /// Inbound unread and inbound kept may; read-ephemeral has no content
    /// to include.
    #[must_use]
    pub const fn backup_eligible(&self) -> bool {
        matches!(self.state, InboundState::Unread | InboundState::Kept)
    }
}

/// Whether the human store can still accept new unread content.
///
/// Not a detail. If the store cannot durably hold unread inbound, the
/// client must stop presenting itself as a healthy durable receiver:
/// [`StorageHealth::degraded_response`] says what it must do instead of
/// accepting a stream it cannot retain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageHealth {
    /// Unread inbound can be committed durably.
    Healthy,
    /// It cannot.
    Degraded,
}

/// What a client must do while storage is degraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DegradedResponse {
    /// Release or disable the human direct endpoint.
    pub release_human_endpoint: bool,
    /// Suspend local human broadcast joins and delivery.
    pub suspend_broadcast_joins: bool,
    /// Surface the degradation to the user.
    pub surface_to_user: bool,
}

impl StorageHealth {
    /// The required reaction.
    ///
    /// Degrading rather than silently dropping is the point: an endpoint
    /// that stays leased while its store cannot retain would accept
    /// messages it is about to lose, and `AcceptedV2` would then be a
    /// claim the receiver could not honour. Suspending the joins matters
    /// too — the profile may keep the GossipSub mesh warm with no local
    /// consumer, which preserves the no-buffer rule.
    ///
    /// This is an application reaction and changes no transport semantics.
    #[must_use]
    pub const fn degraded_response(self) -> Option<DegradedResponse> {
        match self {
            Self::Healthy => None,
            Self::Degraded => Some(DegradedResponse {
                release_human_endpoint: true,
                suspend_broadcast_joins: true,
                surface_to_user: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbound_is_durable_while_pending_and_removed_once_terminal() {
        let mut m = OutboundMessage::composed();
        assert_eq!(m.durability(), Durability::Durable);
        assert_eq!(
            m.transport_terminal(TerminalCause::Accepted),
            Durability::Remove
        );
        assert_eq!(m.state(), OutboundState::Terminal);
    }

    #[test]
    fn a_transient_failure_leaves_outbound_pending_and_durable() {
        // Deleting it would lose content the user believes they sent.
        let m = OutboundMessage::composed();
        assert!(m.remains_pending_after_transient_failure());
        assert_eq!(m.durability(), Durability::Durable);
    }

    #[test]
    fn reaching_terminal_twice_keeps_the_first_cause() {
        let mut m = OutboundMessage::composed();
        m.transport_terminal(TerminalCause::Accepted);
        m.transport_terminal(TerminalCause::Cancelled);
        assert_eq!(m.cause(), Some(TerminalCause::Accepted));
    }

    #[test]
    fn broadcast_publication_is_terminal_but_is_not_delivery() {
        // Terminal for RETENTION because broadcast has no per-recipient
        // acknowledgement — not because anyone received it.
        let mut m = OutboundMessage::composed();
        assert_eq!(
            m.transport_terminal(TerminalCause::Published),
            Durability::Remove
        );
        assert_eq!(m.cause(), Some(TerminalCause::Published));
    }

    #[test]
    fn inbound_unread_is_durable_and_reading_removes_it() {
        let mut m = InboundMessage::committed_unread();
        assert_eq!(m.durability(), Durability::Durable);
        assert_eq!(m.mark_read(), Durability::Remove);
        assert_eq!(m.state(), InboundState::ReadEphemeral);
    }

    #[test]
    fn keep_is_refused_before_the_message_has_been_read() {
        // Otherwise a notification action could make content durable that
        // the human never looked at.
        let mut m = InboundMessage::committed_unread();
        assert_eq!(m.keep(), Err(KeepRefused::NotYetRead));
        assert_eq!(m.state(), InboundState::Unread);
    }

    #[test]
    fn keep_after_read_makes_it_durable_again() {
        let mut m = InboundMessage::committed_unread();
        m.mark_read();
        assert_eq!(m.keep(), Ok(Durability::Durable));
        assert_eq!(m.state(), InboundState::Kept);
        assert!(m.backup_eligible());
    }

    #[test]
    fn a_read_unkept_message_can_be_kept_while_still_in_memory() {
        let mut m = InboundMessage::committed_unread();
        m.mark_read();
        assert!(m.content_held());
        assert!(m.keep().is_ok());
    }

    #[test]
    fn once_the_session_ends_a_read_unkept_message_is_gone_by_design() {
        let mut m = InboundMessage::committed_unread();
        m.mark_read();
        m.session_ended();
        assert!(!m.content_held());
        assert_eq!(m.keep(), Err(KeepRefused::ContentNoLongerHeld));
    }

    #[test]
    fn a_kept_message_survives_the_session() {
        let mut m = InboundMessage::committed_unread();
        m.mark_read();
        m.keep().expect("kept");
        m.session_ended();
        assert!(m.content_held());
        assert_eq!(m.durability(), Durability::Durable);
    }

    #[test]
    fn an_unread_message_survives_the_session() {
        let mut m = InboundMessage::committed_unread();
        m.session_ended();
        assert!(m.content_held());
        assert_eq!(m.durability(), Durability::Durable);
    }

    #[test]
    fn removing_keep_deletes_immediately() {
        let mut m = InboundMessage::committed_unread();
        m.mark_read();
        m.keep().expect("kept");
        assert_eq!(m.unkeep(), Durability::Remove);
        assert_eq!(m.state(), InboundState::ReadEphemeral);
    }

    #[test]
    fn only_three_states_are_ever_durable() {
        // The core invariant, walked exhaustively rather than described.
        let mut out = OutboundMessage::composed();
        assert_eq!(out.durability(), Durability::Durable); // pending
        out.transport_terminal(TerminalCause::Accepted);
        assert_eq!(out.durability(), Durability::Remove);

        let mut inb = InboundMessage::committed_unread();
        assert_eq!(inb.durability(), Durability::Durable); // unread
        inb.mark_read();
        assert_eq!(inb.durability(), Durability::Remove);
        inb.keep().expect("kept");
        assert_eq!(inb.durability(), Durability::Durable); // kept
    }

    #[test]
    fn backup_eligibility_excludes_all_outbound() {
        // A restored or second device must not become an implicit replay
        // or delayed-send source.
        let mut out = OutboundMessage::composed();
        assert!(!out.backup_eligible());
        out.transport_terminal(TerminalCause::Accepted);
        assert!(!out.backup_eligible());

        let mut inb = InboundMessage::committed_unread();
        assert!(inb.backup_eligible());
        inb.mark_read();
        assert!(!inb.backup_eligible());
        inb.keep().expect("kept");
        assert!(inb.backup_eligible());
    }

    #[test]
    fn degraded_storage_releases_the_endpoint_and_suspends_joins() {
        assert_eq!(StorageHealth::Healthy.degraded_response(), None);
        let r = StorageHealth::Degraded
            .degraded_response()
            .expect("degraded");
        // Staying leased would accept messages the receiver is about to
        // lose, making AcceptedV2 a claim it cannot honour.
        assert!(r.release_human_endpoint);
        assert!(r.suspend_broadcast_joins);
        assert!(r.surface_to_user);
    }
}
